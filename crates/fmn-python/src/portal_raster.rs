//! In-memory raster authoring over Atlas and Marionette's immutable resources.
//! Preparation never touches a scene; publication changes only its image axis.

use crate::{
    BridgeMobject, CapabilityError, PyRuntimeError, PyScene, PyValueError, StaleHandleError,
    install_native_tree, native_error, stage_error, with_stage,
};
use fmn_hash::{Digest, Sha256, sha256};
use fmn_mobject::{ImageColorSpace, ImageResource, ImageSampler, RenderPrimitive};
use fmn_output::{ImageDecodeError, ImageDecodeLimits, ImageDecodeReport, ImageDecoder};
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyDict, PyList};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

#[path = "portal_raster_init.rs"]
mod initialization;

#[path = "portal_raster_transition.rs"]
mod transition;

const MAX_PIXELS: u64 = 16_777_216;
const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;
const MAX_IMAGE_INPUTS: usize = 4096;

/// A decode receipt belongs to immutable native pixels, never an editable
/// Python attribute. Copies and in-memory snapshots preserve the allocation.
/// Durable embedded pixels are explicit baked inputs and receive that label.
#[derive(Clone)]
pub(crate) struct PortalImageInput {
    // A process-local decoder job identity, never serialized or hashed. Zero
    // identifies embedded pixels, whose normalized content is their identity.
    // Keeping Weak<[u8]> here would retain the large inline byte allocation
    // throughout report/checkpoint history even after its last Arc was dropped.
    identity: u64,
    path: Option<String>,
    report: Option<ImageDecodeReport>,
    rgba_digest: Digest,
    pixel_digest: Digest,
    width: u32,
    height: u32,
}

impl PortalImageInput {
    fn embedded(image: &ImageResource) -> Self {
        // Same normalized raster identity as Reel. Linear is an explicit
        // native material encoding, in addition to decoded PNG/JPEG intents.
        let mut hash = Sha256::new();
        hash.update(b"fmn-normalized-raster-v1\0");
        hash.update(&image.width().to_le_bytes());
        hash.update(&image.height().to_le_bytes());
        match image.color_space() {
            ImageColorSpace::Srgb => hash.update(&[0]),
            ImageColorSpace::Gamma(value) => {
                hash.update(&[1]);
                hash.update(&value.to_le_bytes());
            }
            ImageColorSpace::Linear => hash.update(&[2]),
        }
        hash.update(image.pixels());
        Self {
            identity: 0,
            path: None,
            report: None,
            rgba_digest: image.content_digest(),
            pixel_digest: hash.finalize(),
            width: image.width(),
            height: image.height(),
        }
    }

    fn same_input(&self, other: &Self) -> bool {
        // Copies/interpolated frames reuse one receipt; two independent
        // decoder jobs retain both invocations even for identical input bytes.
        self.identity == other.identity && self.pixel_digest == other.pixel_digest
    }

    fn key(&self) -> (u64, Digest) {
        (self.identity, self.pixel_digest)
    }

    fn invocation(&self) -> Option<&fmn_output::InvocationReport> {
        self.report.as_ref()?.invocation.as_ref()
    }
}

struct RegisteredImageInput {
    pixels: Weak<[u8]>,
    inputs: Arc<[PortalImageInput]>,
}

// Weak lookup keys are pruned on access. They cannot keep a raster alive, and
// reserve its address until pruning so reuse cannot attach an old receipt to
// unrelated pixels. Long-lived reports/checkpoints contain metadata only.
static IMAGE_INPUTS: OnceLock<Mutex<Vec<RegisteredImageInput>>> = OnceLock::new();
static NEXT_IMAGE_INPUT_ID: AtomicU64 = AtomicU64::new(1);
static CERTIFIED_IMAGE_SESSIONS: AtomicUsize = AtomicUsize::new(0);

fn registered_inputs() -> PyResult<std::sync::MutexGuard<'static, Vec<RegisteredImageInput>>> {
    let mut inputs = IMAGE_INPUTS
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| PyRuntimeError::new_err("image receipt registry is poisoned"))?;
    inputs.retain(|input| input.pixels.strong_count() != 0);
    Ok(inputs)
}

fn inputs_for(image: &ImageResource, registry: &[RegisteredImageInput]) -> Arc<[PortalImageInput]> {
    let pixels = image.shared_pixels();
    registry
        .iter()
        .find(|input| {
            input
                .pixels
                .upgrade()
                .is_some_and(|stored| Arc::ptr_eq(&stored, &pixels))
        })
        .map_or_else(
            || Arc::from([PortalImageInput::embedded(image)]),
            |input| Arc::clone(&input.inputs),
        )
}

fn register_inputs(image: &ImageResource, inputs: Arc<[PortalImageInput]>) -> PyResult<()> {
    let mut registry = registered_inputs()?;
    registry.push(RegisteredImageInput {
        pixels: Arc::downgrade(&image.shared_pixels()),
        inputs,
    });
    Ok(())
}

type ImageKey = (u32, u32, ImageColorSpace, Digest);
type CheckpointInputs = HashMap<ImageKey, Arc<[PortalImageInput]>>;

fn image_key(image: &ImageResource) -> ImageKey {
    (
        image.width(),
        image.height(),
        image.color_space(),
        image.content_digest(),
    )
}

/// SceneState's public checkpoint bytes deliberately contain baked pixels.
/// While a scene is live, bind their exact digest to the native input receipts
/// captured with them. This preserves origin across undo/redo without changing
/// the durable wire format, retaining pixel buffers, or trusting Python flags.
#[derive(Default)]
pub(crate) struct ImageCheckpoints {
    checkpoints: BTreeMap<Digest, CheckpointInputs>,
    entries: usize,
}

impl ImageCheckpoints {
    pub(crate) fn capture<'a>(
        &mut self,
        bytes: &[u8],
        images: impl Iterator<Item = &'a ImageResource>,
    ) -> PyResult<()> {
        let registry = registered_inputs()?;
        let mut captured = CheckpointInputs::new();
        for image in images.flat_map(|image| std::iter::once(image).chain(image.dark_image())) {
            let pixels = image.shared_pixels();
            if let Some(receipt) = registry.iter().find(|input| {
                input
                    .pixels
                    .upgrade()
                    .is_some_and(|stored| Arc::ptr_eq(&stored, &pixels))
            }) {
                let inputs = captured
                    .entry(image_key(image))
                    .or_insert_with(|| Arc::from([]));
                merge_inputs(inputs, &receipt.inputs);
            }
        }
        if captured.is_empty() {
            return Ok(());
        }
        let digest = sha256(bytes);
        let previous_entries = if let Some(existing) = self.checkpoints.get(&digest) {
            // Byte-identical checkpoint captures can have different decoder
            // histories. Retain their union rather than silently erasing one.
            for (key, inputs) in existing {
                merge_inputs(
                    captured.entry(*key).or_insert_with(|| Arc::from([])),
                    inputs,
                );
            }
            existing.values().map(|inputs| inputs.len()).sum()
        } else {
            0
        };
        let captured_entries: usize = captured.values().map(|inputs| inputs.len()).sum();
        let next_entries = self.entries - previous_entries + captured_entries;
        if (self.checkpoints.len() >= 4096 && !self.checkpoints.contains_key(&digest))
            || next_entries > 65_536
        {
            return Err(PyValueError::new_err(
                "scene image checkpoint receipts exceed their bounded history budget",
            ));
        }
        self.entries = next_entries;
        self.checkpoints.insert(digest, captured);
        Ok(())
    }

    pub(crate) fn restore<'a>(
        &self,
        bytes: &[u8],
        images: impl Iterator<Item = &'a ImageResource>,
    ) -> PyResult<()> {
        if self.checkpoints.is_empty() {
            return Ok(());
        }
        let Some(captured) = self.checkpoints.get(&sha256(bytes)) else {
            return Ok(());
        };
        let mut registry = registered_inputs()?;
        for image in images.flat_map(|image| std::iter::once(image).chain(image.dark_image())) {
            if let Some(inputs) = captured.get(&image_key(image)) {
                registry.push(RegisteredImageInput {
                    pixels: Arc::downgrade(&image.shared_pixels()),
                    inputs: Arc::clone(inputs),
                });
            }
        }
        Ok(())
    }
}

fn merge_inputs(target: &mut Arc<[PortalImageInput]>, incoming: &[PortalImageInput]) {
    let mut merged = target.to_vec();
    for input in incoming {
        if !merged.iter().any(|known| known.same_input(input)) {
            merged.push(input.clone());
        }
    }
    *target = Arc::from(merged);
}

fn same_resource_inputs(left: &ImageResource, right: &ImageResource) -> PyResult<bool> {
    if left != right {
        return Ok(false);
    }
    let registry = registered_inputs()?;
    for (left, right) in std::iter::once(left)
        .chain(left.dark_image())
        .zip(std::iter::once(right).chain(right.dark_image()))
    {
        if Arc::ptr_eq(&left.shared_pixels(), &right.shared_pixels()) {
            continue;
        }
        let a = inputs_for(left, &registry);
        let b = inputs_for(right, &registry);
        if a.len() != b.len()
            || !a.iter().all(|input| {
                b.iter().any(|other| {
                    input.same_input(other)
                        || (input.report.is_none()
                            && other.report.is_none()
                            && input.pixel_digest == other.pixel_digest)
                })
            })
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Constant pixels can still depend on two distinct authored inputs. Prepare
/// one isolated intermediate resource so sharing it cannot taint the original
/// native endpoint or discard the other endpoint's decoder receipt.
fn constant_material(start: &ImageResource, end: &ImageResource) -> PyResult<ImageResource> {
    if same_resource_inputs(start, end)? {
        return Ok(start.clone());
    }
    let independent = |image: &ImageResource| {
        ImageResource::rgba8(
            image.width(),
            image.height(),
            image.pixels().to_vec(),
            image.color_space(),
            image.sampler(),
        )
        .map_err(native_error)
    };
    let mut image = independent(start)?;
    if let Some(dark) = start.dark_image() {
        image = image
            .with_dark_image(independent(dark)?)
            .map_err(native_error)?;
    }
    inherit_inputs(&image, &[start, end])?;
    Ok(image)
}

/// A sampled transition retains the source closure of both endpoint materials.
/// Otherwise a pre-render interpolation could erase an external decoder origin.
fn inherit_inputs(image: &ImageResource, sources: &[&ImageResource]) -> PyResult<()> {
    let registry = registered_inputs()?;
    let mut inputs = Vec::new();
    for source in sources {
        for resource in std::iter::once(*source).chain(source.dark_image()) {
            for input in inputs_for(resource, &registry).iter() {
                if !inputs
                    .iter()
                    .any(|known: &PortalImageInput| known.same_input(input))
                {
                    if CERTIFIED_IMAGE_SESSIONS.load(Ordering::Acquire) != 0
                        && inputs.len() >= MAX_IMAGE_INPUTS
                    {
                        return Err(PyValueError::new_err(
                            "raster transition exceeds the 4096-input provenance budget",
                        ));
                    }
                    inputs.push(input.clone());
                }
            }
        }
    }
    drop(registry);
    let inputs = Arc::from(inputs);
    register_inputs(image, Arc::clone(&inputs))?;
    if let Some(dark) = image.dark_image() {
        register_inputs(dark, inputs)?;
    }
    Ok(())
}

pub(crate) struct ImageInputs {
    seen: Vec<Weak<[u8]>>,
    known: HashSet<(u64, Digest)>,
    pub(crate) inputs: Vec<PortalImageInput>,
    certified: bool,
}

impl ImageInputs {
    pub(crate) fn new(certified: bool) -> Self {
        if certified {
            CERTIFIED_IMAGE_SESSIONS.fetch_add(1, Ordering::AcqRel);
        }
        Self {
            seen: Vec::new(),
            known: HashSet::new(),
            inputs: Vec::new(),
            certified,
        }
    }

    pub(crate) fn observe<'a>(
        &mut self,
        images: impl Iterator<Item = &'a ImageResource>,
    ) -> Result<(), fmn_scene::IntegrationError> {
        let registry = registered_inputs()
            .map_err(|error| fmn_scene::IntegrationError::new("image-input", error.to_string()))?;
        self.seen.retain(|image| image.strong_count() != 0);
        for image in images.flat_map(|image| std::iter::once(image).chain(image.dark_image())) {
            let pixels = image.shared_pixels();
            if self.seen.iter().any(|seen| {
                seen.upgrade()
                    .is_some_and(|seen| Arc::ptr_eq(&seen, &pixels))
            }) {
                continue;
            }
            let inputs = inputs_for(image, &registry);
            for input in inputs.iter() {
                if self.certified && input.invocation().is_some() {
                    return Err(fmn_scene::IntegrationError::new(
                        "image-input",
                        "certified rendering refuses ffmpeg-decoded images; supply native PNG/JPEG or explicit baked RGBA pixels",
                    ));
                }
                if !self.known.contains(&input.key()) {
                    if self.certified && self.inputs.len() >= MAX_IMAGE_INPUTS {
                        return Err(fmn_scene::IntegrationError::new(
                            "image-input",
                            "render image inputs exceed the 4096-input budget",
                        ));
                    }
                    self.known.insert(input.key());
                    self.inputs.push(input.clone());
                }
            }
            self.seen.push(Arc::downgrade(&pixels));
        }
        Ok(())
    }

    pub(crate) fn invocations(&self) -> Vec<fmn_output::InvocationReport> {
        self.inputs
            .iter()
            .filter_map(|input| input.invocation().cloned())
            .collect()
    }
}

impl Drop for ImageInputs {
    fn drop(&mut self) {
        if self.certified {
            CERTIFIED_IMAGE_SESSIONS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

pub(crate) fn input_facts(scene: &Bound<'_, PyScene>) -> PyResult<Py<PyList>> {
    let facts = PyList::empty(scene.py());
    for input in &scene.borrow().render_image_inputs {
        let fact = PyDict::new(scene.py());
        fact.set_item("path", &input.path)?;
        fact.set_item("width", input.width)?;
        fact.set_item("height", input.height)?;
        fact.set_item("rgba_sha256", input.rgba_digest.to_hex())?;
        fact.set_item(
            "source_sha256",
            input
                .report
                .as_ref()
                .map_or(input.rgba_digest, |report| report.source_digest)
                .to_hex(),
        )?;
        fact.set_item("pixel_sha256", input.pixel_digest.to_hex())?;
        fact.set_item(
            "container",
            input
                .report
                .as_ref()
                .map_or("rgba8", |report| report.format.name()),
        )?;
        fact.set_item(
            "decoder",
            match &input.report {
                Some(report) if report.invocation.is_some() => "ffmpeg",
                Some(_) => "native",
                None => "embedded-rgba8",
            },
        )?;
        fact.set_item(
            "decoder_sha256",
            input
                .invocation()
                .map(|invocation| &invocation.provenance.tool_sha256_hex),
        )?;
        facts.append(fact)?;
    }
    Ok(facts.unbind())
}

fn decode_error(error: ImageDecodeError) -> PyErr {
    match error {
        ImageDecodeError::Capability(_) | ImageDecodeError::TranscoderRequired { .. } => {
            CapabilityError::new_err(error.to_string())
        }
        ImageDecodeError::Boundary(_) => PyRuntimeError::new_err(error.to_string()),
        _ => PyValueError::new_err(error.to_string()),
    }
}

/// Owned preparation value; shared pixels are immutable, not a writable view.
#[pyclass(frozen, name = "_RasterImage")]
pub(super) struct RasterImage {
    pub(super) resource: ImageResource,
}

#[pymethods]
impl RasterImage {
    #[new]
    fn new(py: Python<'_>, width: u32, height: u32, rgba: &Bound<'_, PyBytes>) -> PyResult<Self> {
        let count = u64::from(width) * u64::from(height);
        if width == 0 || height == 0 || count > MAX_PIXELS {
            return Err(PyValueError::new_err(
                "raster dimensions exceed the 16M-pixel budget or are empty",
            ));
        }
        if rgba.as_bytes().len() as u64 != count * 4 {
            return Err(PyValueError::new_err(
                "raster input must contain exactly width * height * 4 RGBA8 bytes",
            ));
        }
        let bytes = rgba.as_bytes();
        py.detach(|| {
            ImageResource::rgba8(
                width,
                height,
                bytes.to_vec(),
                ImageColorSpace::Srgb,
                ImageSampler::default(),
            )
            .map(|resource| Self { resource })
            .map_err(native_error)
        })
    }

    #[staticmethod]
    #[pyo3(signature = (encoded, *, ffmpeg_bin=None, source_path=None))]
    pub(super) fn decode(
        py: Python<'_>,
        encoded: &Bound<'_, PyBytes>,
        ffmpeg_bin: Option<PathBuf>,
        source_path: Option<String>,
    ) -> PyResult<Self> {
        let bytes = encoded.as_bytes();
        if bytes.len() > MAX_ENCODED_BYTES {
            return Err(PyValueError::new_err(
                "raster input exceeds the 64 MiB encoded budget",
            ));
        }
        if ffmpeg_bin
            .as_ref()
            .is_some_and(|path| path.as_os_str().is_empty())
        {
            return Err(PyValueError::new_err(
                "ffmpeg_bin must be a nonempty executable path",
            ));
        }
        let certified = CERTIFIED_IMAGE_SESSIONS.load(Ordering::Acquire) != 0
            || py
                .import("fmn_python.effect_audit")?
                .getattr("certification_active")?
                .call0()?
                .extract::<bool>()?;
        py.detach(|| {
            // Bound the decoder BEFORE allocation/decompression, not afterwards.
            // During certification even an explicit capability is disabled,
            // so an exotic input refuses before any tool lookup or process.
            let mut decoder = ImageDecoder::new(
                ImageDecodeLimits {
                    max_input_bytes: MAX_ENCODED_BYTES as u64,
                    max_pixels: MAX_PIXELS,
                    ..ImageDecodeLimits::default()
                },
                if certified { None } else { ffmpeg_bin },
            )
            .map_err(decode_error)?;
            let decoded = decoder.decode(bytes).map_err(|error| match error {
                ImageDecodeError::TranscoderRequired { format } if certified => CapabilityError::new_err(format!(
                    "certified rendering refuses external {} image decoding; supply native PNG/JPEG or explicit baked RGBA pixels",
                    format.name(),
                )),
                error => decode_error(error),
            })?;
            let color_space = match decoded.image.intent {
                fmn_codec::png::ColorIntent::Gamma { gamma_100000 } => {
                    ImageColorSpace::Gamma(gamma_100000)
                }
                fmn_codec::png::ColorIntent::Srgb { .. }
                | fmn_codec::png::ColorIntent::AssumedSrgb => ImageColorSpace::Srgb,
            };
            let resource = ImageResource::rgba8(
                decoded.image.width,
                decoded.image.height,
                decoded.image.rgba,
                color_space,
                ImageSampler::default(),
            )
            .map_err(native_error)?;
            register_inputs(
                &resource,
                Arc::from([PortalImageInput {
                    identity: NEXT_IMAGE_INPUT_ID.fetch_add(1, Ordering::Relaxed),
                    path: source_path,
                    pixel_digest: decoded.report.pixel_digest,
                    report: Some(decoded.report),
                    ..PortalImageInput::embedded(&resource)
                }]),
            )?;
            Ok(Self { resource })
        })
    }

    fn __copy__(&self) -> Self {
        Self {
            resource: self.resource.clone(),
        }
    }

    fn __deepcopy__(&self, _memo: &Bound<'_, PyAny>) -> Self {
        self.__copy__()
    }

    #[getter]
    fn size(&self) -> (u32, u32) {
        (self.resource.width(), self.resource.height())
    }

    fn pixels<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, self.resource.pixels())
    }
}

#[pyfunction(signature = (target, image, factory, height=4.0, opacity=1.0, z_index=0))]
fn _build_raster_image<'py>(
    target: &Bound<'py, BridgeMobject>,
    image: &RasterImage,
    factory: &Bound<'py, PyAny>,
    height: f64,
    opacity: f64,
    z_index: i32,
) -> PyResult<Bound<'py, PyList>> {
    let width = height * f64::from(image.resource.width()) / f64::from(image.resource.height());
    if !height.is_finite()
        || height <= 0.0
        || height > f64::from(f32::MAX)
        || !width.is_finite()
        || width > f64::from(f32::MAX)
        || !opacity.is_finite()
        || opacity.abs() > f64::from(f32::MAX)
    {
        return Err(PyValueError::new_err(
            "image size and opacity must be finite and f32-representable; height must be positive",
        ));
    }
    let object = fmn_library::ImageMobject::from_resource(image.resource.clone())
        .map_err(native_error)?
        .with_height(height)
        .with_opacity(opacity)
        .with_z_index(z_index);
    install_native_tree(target, factory, object)
}

#[pyfunction(signature = (target, dark=false))]
fn _read_raster_image(target: &Bound<'_, BridgeMobject>, dark: bool) -> PyResult<RasterImage> {
    with_stage(target, |stage, mob| {
        let entry = stage
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("raster target is stale"))?;
        let image = entry
            .image_resource()
            .ok_or_else(|| PyValueError::new_err("mobject has no native image resource"))?;
        let selected = if dark {
            image
                .dark_image()
                .ok_or_else(|| PyValueError::new_err("mobject has no dark-side image resource"))?
        } else {
            image
        };
        Ok(RasterImage {
            resource: selected.clone().without_dark_image(),
        })
    })?
}

#[pyfunction(signature = (target, image, dark_image=None))]
fn _replace_raster_image(
    target: &Bound<'_, BridgeMobject>,
    image: &RasterImage,
    dark_image: Option<&RasterImage>,
) -> PyResult<()> {
    let mut candidate = image.resource.clone();
    if let Some(dark) = dark_image {
        candidate = candidate
            .with_dark_image(dark.resource.clone())
            .map_err(native_error)?;
    }
    with_stage(target, |stage, mob| -> PyResult<()> {
        let entry = stage
            .get(mob)
            .ok_or_else(|| StaleHandleError::new_err("raster target is stale"))?;
        if entry.image_resource().is_none()
            || !matches!(
                entry.render_primitive(),
                RenderPrimitive::ImageQuad
                    | RenderPrimitive::SurfaceGrid { .. }
                    | RenderPrimitive::TriangleMesh
            )
        {
            return Err(PyValueError::new_err(
                "pixel replacement requires an existing native image or textured surface",
            ));
        }
        if entry.render_primitive() == RenderPrimitive::ImageQuad
            && candidate.dark_image().is_some()
        {
            return Err(PyValueError::new_err(
                "image quads do not accept dark-side textures",
            ));
        }
        // No geometry bake/resize, owner change, callback or frame-clock step.
        stage
            .set_image_resource(mob, Some(candidate))
            .map_err(stage_error)?;
        Ok(())
    })?
}

/// Compare complete immutable descriptors as well as pixels, including the
/// dark side. History and animation admission can additionally preserve input
/// origins without changing the public rendered-equality predicate.
#[pyfunction(signature = (left, right, *, include_inputs=false))]
fn _raster_images_equal(
    left: &Bound<'_, BridgeMobject>,
    right: &Bound<'_, BridgeMobject>,
    include_inputs: bool,
) -> PyResult<bool> {
    let read = |object: &Bound<'_, BridgeMobject>| -> PyResult<Option<ImageResource>> {
        with_stage(object, |stage, mob| {
            stage
                .get(mob)
                .map(|entry| entry.image_resource().cloned())
                .ok_or_else(|| StaleHandleError::new_err("raster comparison target is stale"))
        })?
    };
    let (left, right) = (read(left)?, read(right)?);
    match (left, right) {
        (Some(left), Some(right)) if include_inputs => same_resource_inputs(&left, &right),
        (left, right) => Ok(left == right),
    }
}

pub(crate) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<RasterImage>()?;
    initialization::install(module)?;
    transition::install(module)?;
    module.add_function(wrap_pyfunction!(_raster_images_equal, module)?)?;
    module.add_function(wrap_pyfunction!(_build_raster_image, module)?)?;
    module.add_function(wrap_pyfunction!(_read_raster_image, module)?)?;
    module.add_function(wrap_pyfunction!(_replace_raster_image, module)?)?;
    Ok(())
}

/// Named assets under the configured asset root (fm-portal-asset-virtualization-r13-qve7):
/// ImageMobject, SVGMobject and Scene.add_sound find their files as the
/// Reference does, over custom_config.yml's `directories`, and a missing or
/// network asset refuses by name. Returns the number of passing cases.
#[cfg(feature = "gauntlet")]
pub fn run_portal_gauntlet_asset_root() -> Result<u64, String> {
    crate::with_python_test_module("asset root", |py, _module, globals| {
        let source = std::ffi::CString::new(include_str!("../tests/asset_root.py"))
            .map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("result")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "asset-root acceptance omitted its test result".to_owned())?
            .getattr("testsRun")
            .and_then(|count| count.extract())
            .map_err(|error| error.to_string())
    })
}

/// Production image preparation, rendering and certification through the
/// embedded portal. The three native cases need no optional host executable;
/// the complete acceptance suite additionally exercises real external decoding.
#[cfg(feature = "gauntlet")]
pub fn run_portal_gauntlet_image_inputs() -> Result<u64, String> {
    crate::with_python_test_module("native image input Gauntlet", |py, _module, globals| {
        globals
            .set_item("_FMN_IMAGE_NATIVE_GAUNTLET", true)
            .map_err(|error| error.to_string())?;
        let source = std::ffi::CString::new(include_str!("../tests/image_inputs.py"))
            .map_err(|error| error.to_string())?;
        py.run(source.as_c_str(), Some(globals), Some(globals))
            .inspect_err(|error| error.print(py))
            .map_err(|error| error.to_string())?;
        globals
            .get_item("result")
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "image acceptance omitted its test result".to_owned())?
            .getattr("testsRun")
            .and_then(|count| count.extract())
            .map_err(|error| error.to_string())
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn retained_receipt_metadata_does_not_reserve_pixel_allocations() {
        let _lock = crate::python_embedding_lock();
        let resource = fmn_mobject::ImageResource::rgba8(
            1,
            1,
            vec![12, 34, 56, 255],
            fmn_mobject::ImageColorSpace::Srgb,
            fmn_mobject::ImageSampler::default(),
        )
        .unwrap();
        let pixels = resource.shared_pixels();
        let baseline = std::sync::Arc::weak_count(&pixels);
        let receipt = super::PortalImageInput::embedded(&resource);
        let history = vec![receipt.clone(), receipt];
        assert_eq!(std::sync::Arc::weak_count(&pixels), baseline);
        assert!(history[0].same_input(&history[1]));
        let independently_baked = fmn_mobject::ImageResource::rgba8(
            1,
            1,
            pixels.to_vec(),
            fmn_mobject::ImageColorSpace::Srgb,
            fmn_mobject::ImageSampler::default(),
        )
        .unwrap();
        assert!(history[0].same_input(&super::PortalImageInput::embedded(&independently_baked)));
    }

    #[test]
    fn standard_pixel_streams_outlive_the_certified_input_closure_budget() {
        let _lock = crate::python_embedding_lock();
        let mut standard = super::ImageInputs::new(false);
        let mut certified = super::ImageInputs::new(true);
        for index in 0..=super::MAX_IMAGE_INPUTS {
            let [low, high] = u16::try_from(index).unwrap().to_le_bytes();
            let resource = fmn_mobject::ImageResource::rgba8(
                1,
                1,
                vec![low, high, 0, 255],
                fmn_mobject::ImageColorSpace::Srgb,
                fmn_mobject::ImageSampler::default(),
            )
            .unwrap();
            standard
                .observe(std::iter::once(&resource))
                .expect("standard raw pixel stream");
            let captured = certified.observe(std::iter::once(&resource));
            assert_eq!(captured.is_ok(), index < super::MAX_IMAGE_INPUTS);
        }
        assert_eq!(standard.inputs.len(), super::MAX_IMAGE_INPUTS + 1);
        assert_eq!(certified.inputs.len(), super::MAX_IMAGE_INPUTS);
    }

    #[test]
    fn governed_image_input_acceptance() {
        crate::with_python_test_module("image inputs", |py, _module, globals| {
            let source = std::ffi::CString::new(include_str!("../tests/image_inputs.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("governed image sources, pixels, receipts and certification");
        });
    }

    #[test]
    fn asset_root_acceptance() {
        crate::with_python_test_module("asset root", |py, _module, globals| {
            let source = std::ffi::CString::new(include_str!("../tests/asset_root.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("configured asset directories, named refusals and the mix");
        });
    }

    #[test]
    fn production_raster_frame_and_texture_acceptance() {
        crate::with_python_test_module("raster frames and textures", |py, _module, globals| {
            for text in [
                include_str!("../tests/raster_textures.py"),
                include_str!("../tests/raster_frames.py"),
            ] {
                let source = std::ffi::CString::new(text).unwrap();
                py.run(source.as_c_str(), Some(globals), Some(globals))
                    .inspect_err(|error| error.print(py))
                    .expect("native live texture frames and history");
            }
        });
    }

    #[test]
    fn production_raster_authoring_acceptance() {
        crate::with_python_test_module("raster authoring", |py, _module, globals| {
            let source =
                std::ffi::CString::new(include_str!("../tests/raster_authoring.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("native raster authoring and snapshots");
        });
    }
}
