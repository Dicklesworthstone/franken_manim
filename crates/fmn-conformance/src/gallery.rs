//! The Look Gallery (§16.3 plane 3, fm-t1v): perceptual smoke alarms over
//! captured Reference/FrankenManim image pairs, plus the file-backed manifest
//! that records the human verdict on each pair.
//!
//! Two halves, deliberately separated:
//!
//! 1. **Metrics — smoke alarms, never hard gates.** [`compare_pair`] computes
//!    three documented numbers over an RGBA8 image pair:
//!
//!    - **Global SSIM** ([`PairMetrics::ssim`]): the single-statistic SSIM of
//!      Wang et al. over the Rec. 709 luma plane (`0.2126 R + 0.7152 G +
//!      0.0722 B`, computed on the sRGB8 codes exactly as the engine-equivalence
//!      lane's `ssim_luma` does after canonical RGBA8 encoding — the formula
//!      family `engine.rs`'s `FAST_VISUAL_BUDGET_V1_MIN_SSIM` and
//!      `metal.rs`'s `METAL_VISUAL_BUDGET_V1_MIN_SSIM` consume). Global means,
//!      sample variances and covariance (divisor `max(n−1, 1)`), stability
//!      constants `C1 = (0.01·255)²`, `C2 = (0.03·255)²`. The *global* form
//!      was chosen by the accelerator spike as a stable smoke alarm in place
//!      of an unreviewed windowed metric; this module keeps that ruling.
//!    - **Edge distance** ([`EdgeDistance`]): symmetric chamfer distance, in
//!      pixels, between thresholded Sobel edge maps — the boring standard
//!      boundary metric. Edges are pixels whose 3×3 Sobel L1 gradient
//!      magnitude on the luma plane reaches [`EDGE_L1_THRESHOLD`]; distances
//!      come from the Borgefors 3-4 two-pass chamfer transform (3 orthogonal,
//!      4 diagonal, reported divided by 3). Each directed score is the mean
//!      distance from one side's edge pixels to the other side's nearest edge
//!      pixel; [`EdgeDistance::symmetric`] is their mean. Conventions: no
//!      edge pixels on either side → 0; one side empty → that directed score
//!      is the frame diagonal (the worst possible) and the empty→full
//!      direction is 0.
//!    - **Local-error percentiles** ([`ErrorPercentiles`]): per-pixel error =
//!      max `|Δ|` over the R, G and B channels, normalized to `[0, 1]` by
//!      ÷255 (alpha is excluded: both pipelines composite it to identity on
//!      these stills, and the metric exists to see *colour* drift). p50/p95/
//!      p99 by the nearest-rank method on the sorted errors, plus the maximum.
//!
//!    Each returns numbers in a struct. Thresholds are verdict *inputs* for a
//!    human reviewer, never assertions — per D-16 the Reference is a design
//!    oracle and aesthetic bar, never a pixel warden.
//!
//! 2. **The verdict workflow.** [`Verdict`] is §16.3's vocabulary —
//!    `AtLeastAsGood`, `DifferentButFine` (Behavior-Noted), `Regression` — and
//!    [`GalleryManifest`] is its ledger: a versioned TSV artifact
//!    (`fixtures/look_gallery.tsv`, format `# fmn-look-gallery v1`) pairing
//!    each captured Reference image (`gallery/reference_captures/`, private
//!    per §15.3) with its committed FrankenManim render
//!    (`docs/g0/g0-2-renders/`), the current verdict, and the change that
//!    last moved it. A row may carry the explicit
//!    `reference-capture-missing` refusal until its private capture exists;
//!    this is not an aesthetic verdict. The review tooling is three functions:
//!    [`render_pairs`] resolves a manifest against a checkout (a missing
//!    committed render is an error; an absent private capture only mutes the
//!    smoke alarm), [`GalleryManifest::record_verdict`] moves one panel's
//!    verdict with its reason and bumps the manifest revision, and
//!    [`GalleryManifest::regressions_since`] diffs two manifest revisions for
//!    panels whose verdict worsened.
//!
//! [`canonical_png_panel`] is the deliberate bridge from certified Lumen
//! frames to this review plane. It applies fmn-frame's bit-exact canonical
//! transfer and fmn-codec's owned deterministic PNG encoder, so the panel a
//! human reviews is itself a certified, lockable artifact rather than output
//! from a throwaway decoder.

use fmn_codec::{CompressionLevel, encode_rgba8};
use fmn_frame::convert::rgba16f_to_rgba8;
use fmn_frame::{FrameBuffer, FrameError, FrameLayout, PixelFormat};
use std::collections::BTreeMap;
use std::fmt;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic counter making concurrent tmp-file names unique (same pattern as
/// the self-golden rig: `cargo test` is parallel, tmp-and-rename must not
/// collide within the process).
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The manifest format tag; the first line of every look-gallery TSV.
pub const MANIFEST_HEADER: &str = "# fmn-look-gallery v1";

const MANIFEST_REVISION_PREFIX: &str = "# revision: ";
const MANIFEST_COLUMNS: &str = "# columns: panel\treference\trender\tverdict\tchanged";

/// Maximum byte length of one look-gallery manifest. The ledger is a small,
/// reviewed TSV; one MiB leaves ample growth room while keeping malformed
/// file and in-memory inputs bounded before row ownership begins.
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

/// Sobel L1 gradient magnitude at or above which a luma pixel is an edge.
///
/// The 3×3 Sobel kernel's maximum L1 response is `4·255` (a perfect step); a
/// quarter of that is a full-scale step over roughly eight pixels, which
/// rejects smooth shading ramps (lighting, glow falloff) while keeping every
/// silhouette, stroke boundary and glyph stem. Documented, fixed, and
/// deliberately not Otsu: a data-dependent threshold would make the metric
/// incomparable across revisions of the same panel.
pub const EDGE_L1_THRESHOLD: f64 = 255.0;

/// A borrowed tight-row RGBA8 image: `width × height × 4` bytes, row 0 on top
/// (the D-23 output orientation fmn-codec's `DecodedPng` normalizes to).
#[derive(Clone, Copy, Debug)]
pub struct RgbaView<'a> {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width × height × 4` bytes of RGBA, row-major, tight rows.
    pub pixels: &'a [u8],
}

impl<'a> RgbaView<'a> {
    /// Wrap a buffer, refusing zero dimensions and wrong lengths.
    ///
    /// # Errors
    /// [`GalleryError::InvalidImage`] if either dimension is zero or the
    /// buffer is not exactly `width × height × 4` bytes.
    pub fn new(width: u32, height: u32, pixels: &'a [u8]) -> Result<Self, GalleryError> {
        if width == 0 || height == 0 {
            return Err(GalleryError::InvalidImage(format!(
                "zero dimension ({width}x{height})"
            )));
        }
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .and_then(|bytes| usize::try_from(bytes).ok())
            .ok_or_else(|| {
                GalleryError::InvalidImage(format!(
                    "dimensions {width}x{height} overflow the addressable RGBA8 byte length"
                ))
            })?;
        if pixels.len() != expected {
            return Err(GalleryError::InvalidImage(format!(
                "{} bytes for {width}x{height} RGBA8, expected {expected}",
                pixels.len()
            )));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }
}

/// Encode one certified linear-light frame as the canonical Look Gallery PNG.
///
/// The conversion is the certified `Rgba16F -> Rgba8` table lookup and the
/// encoder uses the owned deterministic best-compression route. The resulting
/// bytes contain no timestamps and are identical across thread counts and the
/// certified platform matrix when the input frame is identical.
///
/// # Errors
/// [`FrameError`] if `frame` is not `Rgba16F` or its dimensions cannot form a
/// valid tight `Rgba8` layout. No alternate conversion is selected silently.
pub fn canonical_png_panel(frame: &FrameBuffer) -> Result<Vec<u8>, FrameError> {
    let width = frame.layout().width();
    let height = frame.layout().height();
    let layout = FrameLayout::tight(PixelFormat::Rgba8, width, height)?;
    let mut rgba8 = FrameBuffer::new(layout);
    rgba16f_to_rgba8(frame, &mut rgba8)?;
    Ok(encode_rgba8(
        width,
        height,
        rgba8.plane(0),
        CompressionLevel::Best,
    ))
}

/// The three smoke-alarm numbers for one reference/candidate pair.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PairMetrics {
    /// Global luma SSIM in `[-1, 1]`; `1.0` iff the luma planes are identical.
    pub ssim: f64,
    /// Symmetric chamfer distance between thresholded Sobel edge maps.
    pub edge: EdgeDistance,
    /// Nearest-rank percentiles of the per-pixel max RGB channel error.
    pub error: ErrorPercentiles,
}

/// Boundary disagreement between two images, in pixels. See the module docs
/// for the exact definition and the empty-edge-map conventions.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct EdgeDistance {
    /// Edge pixels found in the reference image.
    pub reference_edges: u64,
    /// Edge pixels found in the candidate image.
    pub candidate_edges: u64,
    /// Mean distance from reference edge pixels to the nearest candidate edge.
    pub reference_to_candidate: f64,
    /// Mean distance from candidate edge pixels to the nearest reference edge.
    pub candidate_to_reference: f64,
    /// Mean of the two directed scores.
    pub symmetric: f64,
}

/// Nearest-rank percentiles of the per-pixel error distribution, plus the max.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ErrorPercentiles {
    /// Median per-pixel max-RGB-channel error, in `[0, 1]`.
    pub p50: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile.
    pub p99: f64,
    /// Largest per-pixel error.
    pub max: f64,
}

/// §16.3's verdict vocabulary for one gallery pair, plus the explicit
/// `ReferenceCaptureMissing` refusal used when no comparison pixels exist.
/// Among reviewed panels the ordering is a severity order:
/// `AtLeastAsGood < DifferentButFine < Regression`.
/// `DifferentButFine` is always Behavior-Noted — the `changed` field of its
/// manifest row names the BN note or ratification that carries the behavior.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Verdict {
    /// No captured Reference image exists, so assigning an aesthetic verdict
    /// would fabricate evidence. This is an evidence-gap refusal, not a
    /// human review result and not part of the severity order.
    ReferenceCaptureMissing,
    /// The FrankenManim panel is at least as good as the Reference capture.
    AtLeastAsGood,
    /// The panels differ in a way that is fine *and written down* (a
    /// Behavior-Noted ruling, e.g. BN-06's fill field).
    DifferentButFine,
    /// The FrankenManim panel is worse in a way nobody has signed off. The
    /// only verdict that demands action.
    Regression,
}

impl Verdict {
    /// The TSV token for this verdict.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::ReferenceCaptureMissing => "reference-capture-missing",
            Self::AtLeastAsGood => "at-least-as-good",
            Self::DifferentButFine => "different-but-fine",
            Self::Regression => "regression",
        }
    }

    /// Parse a TSV verdict token.
    #[must_use]
    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "reference-capture-missing" => Some(Self::ReferenceCaptureMissing),
            "at-least-as-good" => Some(Self::AtLeastAsGood),
            "different-but-fine" => Some(Self::DifferentButFine),
            "regression" => Some(Self::Regression),
            _ => None,
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

/// One manifest row: a named pair, its verdict, and the change that last
/// moved the verdict.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GalleryRow {
    /// Panel id (`[a-z0-9._-]`, e.g. `gradient_fills`); a path-safe name.
    pub panel: String,
    /// Repo-relative path of the captured Reference image.
    pub reference: String,
    /// Repo-relative path of the committed FrankenManim render.
    pub render: String,
    /// The current human verdict, or the explicit missing-capture refusal.
    pub verdict: Verdict,
    /// Free-text record of the change that last moved the verdict (no tabs
    /// or newlines; by convention `bead date: reason`).
    pub changed: String,
}

/// A verdict movement between two manifest revisions.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VerdictChange {
    /// The panel that moved.
    pub panel: String,
    /// The verdict in the earlier manifest (`None` for a panel that did not
    /// exist there).
    pub from: Option<Verdict>,
    /// The verdict in the later manifest.
    pub to: Verdict,
}

/// A parsed look-gallery manifest: format version 1, a monotone `revision`
/// bumped by every verdict change, and rows sorted by panel on write.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GalleryManifest {
    /// Manifest revision; `record_verdict` bumps it.
    pub revision: u64,
    /// The gallery rows.
    pub rows: Vec<GalleryRow>,
}

/// One manifest row resolved against a checkout.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ResolvedPair {
    /// Panel id.
    pub panel: String,
    /// Absolute path of the Reference capture.
    pub reference: PathBuf,
    /// Absolute path of the FrankenManim render (existence verified by
    /// [`render_pairs`]).
    pub render: PathBuf,
    /// Whether the private Reference capture exists in this checkout. Absence
    /// mutes the smoke alarm for this pair; it is never an error (§15.3
    /// fixtures are gitignored).
    pub reference_present: bool,
}

/// Everything the gallery can refuse to do, as a named error.
#[derive(Debug)]
pub enum GalleryError {
    /// An `RgbaView` failed validation.
    InvalidImage(String),
    /// The two images of a pair differ in dimensions; no metric is defined.
    DimensionMismatch {
        /// `(width, height)` of the reference.
        reference: (u32, u32),
        /// `(width, height)` of the candidate.
        candidate: (u32, u32),
    },
    /// The manifest text violates format v1.
    Corrupt {
        /// 1-based line number of the offending line.
        line: usize,
        /// What was wrong with it.
        detail: String,
    },
    /// The manifest exceeds the format envelope and therefore cannot be
    /// parsed or emitted canonically.
    ManifestTooLarge {
        /// Maximum permitted UTF-8 byte length.
        limit: usize,
    },
    /// Filesystem failure reading or writing the manifest.
    Io {
        /// The path being read or written.
        path: PathBuf,
        /// The underlying error.
        err: std::io::Error,
    },
    /// `record_verdict` was given a panel the manifest does not contain.
    UnknownPanel(String),
    /// `record_verdict` was given a change note containing a tab or newline,
    /// which would corrupt the TSV.
    InvalidChangeNote,
    /// `record_verdict` cannot advance an already maximal manifest revision.
    RevisionOverflow,
    /// A committed FrankenManim render named by the manifest is missing from
    /// the checkout. This is the "missing pair" the tests fail on.
    MissingRender {
        /// The panel whose render is missing.
        panel: String,
        /// The path that was expected to exist.
        path: PathBuf,
    },
}

impl fmt::Display for GalleryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidImage(detail) => write!(f, "invalid image: {detail}"),
            Self::DimensionMismatch {
                reference,
                candidate,
            } => write!(
                f,
                "dimension mismatch: reference {}x{}, candidate {}x{}",
                reference.0, reference.1, candidate.0, candidate.1
            ),
            Self::Corrupt { line, detail } => {
                write!(f, "corrupt look-gallery manifest, line {line}: {detail}")
            }
            Self::ManifestTooLarge { limit } => {
                write!(
                    f,
                    "look-gallery manifest exceeds the {limit}-byte format limit"
                )
            }
            Self::Io { path, err } => {
                write!(f, "look-gallery I/O failure at {}: {err}", path.display())
            }
            Self::UnknownPanel(panel) => write!(f, "unknown gallery panel {panel:?}"),
            Self::InvalidChangeNote => {
                write!(f, "change note must not contain tabs or newlines")
            }
            Self::RevisionOverflow => {
                write!(
                    f,
                    "look-gallery manifest revision cannot advance past u64::MAX"
                )
            }
            Self::MissingRender { panel, path } => write!(
                f,
                "gallery panel {panel:?} names a render that is missing from the \
                 checkout: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for GalleryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { err, .. } => Some(err),
            _ => None,
        }
    }
}

// ------------------------------------------------------------ the metrics

/// The Rec. 709 luma plane of an image, per the engine-equivalence lane's
/// canonical conversion (`0.2126 R + 0.7152 G + 0.0722 B` over sRGB8 codes).
fn luma_plane(view: &RgbaView<'_>) -> Vec<f64> {
    view.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]))
        .collect()
}

/// Global SSIM over two luma planes of equal, nonzero length.
///
/// This is the formula family the engine-equivalence budgets consume (see the
/// module docs): global statistics, sample-variance divisor `max(n−1, 1)`,
/// `C1 = (0.01·255)²`, `C2 = (0.03·255)²`. Identical planes score exactly 1.0.
#[must_use]
pub fn global_ssim_luma(reference: &[f64], candidate: &[f64]) -> f64 {
    assert_eq!(
        reference.len(),
        candidate.len(),
        "SSIM planes differ in length"
    );
    assert!(!reference.is_empty(), "SSIM requires at least one pixel");
    let count = reference.len() as f64;
    let reference_mean = reference.iter().sum::<f64>() / count;
    let candidate_mean = candidate.iter().sum::<f64>() / count;
    let mut reference_variance = 0.0;
    let mut candidate_variance = 0.0;
    let mut covariance = 0.0;
    for (&r, &c) in reference.iter().zip(candidate) {
        reference_variance += (r - reference_mean) * (r - reference_mean);
        candidate_variance += (c - candidate_mean) * (c - candidate_mean);
        covariance += (r - reference_mean) * (c - candidate_mean);
    }
    let divisor = (count - 1.0).max(1.0);
    reference_variance /= divisor;
    candidate_variance /= divisor;
    covariance /= divisor;

    let c1 = fmn_dmath::powi(0.01 * 255.0, 2);
    let c2 = fmn_dmath::powi(0.03 * 255.0, 2);
    ((2.0 * reference_mean * candidate_mean + c1) * (2.0 * covariance + c2))
        / ((reference_mean * reference_mean + candidate_mean * candidate_mean + c1)
            * (reference_variance + candidate_variance + c2))
}

/// The thresholded Sobel edge map of a luma plane (interior pixels only;
/// border pixels can never be edges because the kernel needs a full 3×3
/// neighborhood).
fn edge_map(luma: &[f64], width: usize, height: usize) -> Vec<bool> {
    let mut edges = vec![false; luma.len()];
    if width < 3 || height < 3 {
        return edges;
    }
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let at = |dx: usize, dy: usize| luma[(y + dy - 1) * width + (x + dx - 1)];
            // 3×3 Sobel: gx weights the right column positive, gy the bottom.
            let gx = at(2, 0) + 2.0 * at(2, 1) + at(2, 2) - at(0, 0) - 2.0 * at(0, 1) - at(0, 2);
            let gy = at(0, 2) + 2.0 * at(1, 2) + at(2, 2) - at(0, 0) - 2.0 * at(1, 0) - at(2, 0);
            edges[y * width + x] = gx.abs() + gy.abs() >= EDGE_L1_THRESHOLD;
        }
    }
    edges
}

/// Borgefors 3-4 chamfer distance transform: each pixel's distance (in thirds
/// of a pixel — 3 orthogonal, 4 diagonal) to the nearest `true` cell of
/// `target`, by one forward and one backward raster pass.
const fn chamfer_far(width: u32, height: u32) -> u64 {
    // The largest inputs sum to less than 2^33, so widening before the
    // arithmetic covers the complete public dimension domain exactly.
    3 * (width as u64 + height as u64) + 4
}

fn chamfer34(target: &[bool], width: u32, height: u32) -> Vec<u64> {
    // Larger than any true 3-4 distance inside the frame.
    let far = chamfer_far(width, height);
    let (width, height) = (width as usize, height as usize);
    let mut dt: Vec<u64> = target.iter().map(|&e| if e { 0 } else { far }).collect();
    let at = |dt: &[u64], x: usize, y: usize| dt[y * width + x];
    for y in 0..height {
        for x in 0..width {
            let mut best = at(&dt, x, y);
            if x > 0 {
                best = best.min(at(&dt, x - 1, y) + 3);
            }
            if y > 0 {
                best = best.min(at(&dt, x, y - 1) + 3);
                if x > 0 {
                    best = best.min(at(&dt, x - 1, y - 1) + 4);
                }
                if x + 1 < width {
                    best = best.min(at(&dt, x + 1, y - 1) + 4);
                }
            }
            dt[y * width + x] = best;
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let mut best = at(&dt, x, y);
            if x + 1 < width {
                best = best.min(at(&dt, x + 1, y) + 3);
            }
            if y + 1 < height {
                best = best.min(at(&dt, x, y + 1) + 3);
                if x + 1 < width {
                    best = best.min(at(&dt, x + 1, y + 1) + 4);
                }
                if x > 0 {
                    best = best.min(at(&dt, x - 1, y + 1) + 4);
                }
            }
            dt[y * width + x] = best;
        }
    }
    dt
}

/// Mean chamfer distance, in pixels, from the `true` cells of `source` to the
/// nearest `true` cell of `target`, under the conventions in the module docs.
fn directed_chamfer(source: &[bool], target: &[bool], width: u32, height: u32) -> (u64, f64) {
    let source_count = source.iter().filter(|&&e| e).count() as u64;
    let target_count = target.iter().filter(|&&e| e).count() as u64;
    if source_count == 0 {
        return (0, 0.0);
    }
    if target_count == 0 {
        let width = f64::from(width);
        let height = f64::from(height);
        let diagonal = (width * width + height * height).sqrt();
        return (source_count, diagonal);
    }
    let dt = chamfer34(target, width, height);
    let total: u128 = source
        .iter()
        .zip(&dt)
        .filter(|(e, _)| **e)
        .map(|(_, &d)| u128::from(d))
        .sum();
    (source_count, total as f64 / 3.0 / source_count as f64)
}

/// The symmetric chamfer edge distance between two luma planes.
///
/// # Errors
/// [`GalleryError::InvalidImage`] if the dimensions are zero, their pixel
/// count is not addressable on this target, or either luma plane is not
/// exactly `width * height` samples.
pub fn edge_distance_luma(
    reference: &[f64],
    candidate: &[f64],
    width: u32,
    height: u32,
) -> Result<EdgeDistance, GalleryError> {
    if width == 0 || height == 0 {
        return Err(GalleryError::InvalidImage(format!(
            "zero raw-luma dimension ({width}x{height})"
        )));
    }
    let expected = u64::from(width)
        .checked_mul(u64::from(height))
        .and_then(|pixels| usize::try_from(pixels).ok())
        .ok_or_else(|| {
            GalleryError::InvalidImage(format!(
                "raw-luma dimensions {width}x{height} overflow the addressable sample length"
            ))
        })?;
    for (side, actual) in [
        ("reference", reference.len()),
        ("candidate", candidate.len()),
    ] {
        if actual != expected {
            return Err(GalleryError::InvalidImage(format!(
                "raw-luma {side} has {actual} samples for {width}x{height}, expected {expected}"
            )));
        }
    }

    let (w, h) = (width as usize, height as usize);
    let reference_edges = edge_map(reference, w, h);
    let candidate_edges = edge_map(candidate, w, h);
    let (reference_count, reference_to_candidate) =
        directed_chamfer(&reference_edges, &candidate_edges, width, height);
    let (candidate_count, candidate_to_reference) =
        directed_chamfer(&candidate_edges, &reference_edges, width, height);
    Ok(EdgeDistance {
        reference_edges: reference_count,
        candidate_edges: candidate_count,
        reference_to_candidate,
        candidate_to_reference,
        symmetric: (reference_to_candidate + candidate_to_reference) / 2.0,
    })
}

/// Nearest-rank percentiles of the per-pixel max RGB channel error.
///
/// Nearest-rank: for quantile `q`, the element at index `⌈q·n⌉−1` of the
/// sorted errors. Deterministic, interpolation-free, and defined for every
/// nonempty image.
#[must_use]
pub fn error_percentiles(reference: &RgbaView<'_>, candidate: &RgbaView<'_>) -> ErrorPercentiles {
    let mut errors: Vec<f64> = reference
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(candidate.pixels.as_chunks::<4>().0)
        .map(|(a, b)| {
            let mut worst = 0.0_f64;
            for channel in 0..3 {
                let error = f64::from(a[channel].abs_diff(b[channel])) / 255.0;
                if error > worst {
                    worst = error;
                }
            }
            worst
        })
        .collect();
    errors.sort_by(f64::total_cmp);
    let n = errors.len();
    let pick = |q: f64| {
        let rank = (q * n as f64).ceil() as usize;
        errors[rank.max(1) - 1]
    };
    ErrorPercentiles {
        p50: pick(0.50),
        p95: pick(0.95),
        p99: pick(0.99),
        max: errors[n - 1],
    }
}

/// Compute all three smoke-alarm metrics for one pair.
///
/// # Errors
/// [`GalleryError::DimensionMismatch`] if the two views differ in size.
pub fn compare_pair(
    reference: &RgbaView<'_>,
    candidate: &RgbaView<'_>,
) -> Result<PairMetrics, GalleryError> {
    if (reference.width, reference.height) != (candidate.width, candidate.height) {
        return Err(GalleryError::DimensionMismatch {
            reference: (reference.width, reference.height),
            candidate: (candidate.width, candidate.height),
        });
    }
    let reference_luma = luma_plane(reference);
    let candidate_luma = luma_plane(candidate);
    Ok(PairMetrics {
        ssim: global_ssim_luma(&reference_luma, &candidate_luma),
        edge: edge_distance_luma(
            &reference_luma,
            &candidate_luma,
            reference.width,
            reference.height,
        )?,
        error: error_percentiles(reference, candidate),
    })
}

// ------------------------------------------------------------ the manifest

/// Panel ids are path components: the golden rig's conservative charset.
fn valid_panel(panel: &str) -> bool {
    !panel.is_empty()
        && panel.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
        && !panel.starts_with('.')
}

/// A manifest path is repo-relative, `/`-separated, and stays inside the
/// repository: no leading or trailing `/`, no `..`, no `.` components, no
/// backslashes, no control characters.
fn valid_repo_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.ends_with('/')
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// Split one manifest row without allocating a field vector for malformed
/// input carrying an arbitrary number of tab separators.
fn split_gallery_row(line: &str) -> Option<[&str; 5]> {
    let mut fields = line.split('\t');
    let exact = [
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    ];
    fields.next().is_none().then_some(exact)
}

fn oversized_manifest() -> GalleryError {
    GalleryError::ManifestTooLarge {
        limit: MAX_MANIFEST_BYTES,
    }
}

impl GalleryManifest {
    /// Parse manifest text in format v1.
    ///
    /// # Errors
    /// [`GalleryError::Corrupt`] on any format violation: wrong header, bad
    /// revision or columns line, non-canonical line endings or row order,
    /// wrong field count, unknown verdict token, invalid panel or path, or a
    /// duplicate panel.
    pub fn parse(text: &str) -> Result<Self, GalleryError> {
        if text.len() > MAX_MANIFEST_BYTES {
            return Err(oversized_manifest());
        }
        if text.is_empty() {
            return Err(GalleryError::Corrupt {
                line: 1,
                detail: "empty manifest".to_string(),
            });
        }
        if let Some(offset) = text.find('\r') {
            let line = text[..offset].bytes().filter(|byte| *byte == b'\n').count() + 1;
            return Err(GalleryError::Corrupt {
                line,
                detail: "carriage returns are not canonical; use LF line endings".to_string(),
            });
        }
        let Some(body) = text.strip_suffix('\n') else {
            return Err(GalleryError::Corrupt {
                line: text.bytes().filter(|byte| *byte == b'\n').count() + 1,
                detail: "manifest must end with a final LF".to_string(),
            });
        };

        let mut rows: Vec<GalleryRow> = Vec::new();
        let mut previous_panel: Option<&str> = None;
        let mut lines = body.split('\n').enumerate();
        let Some((_, header)) = lines.next() else {
            return Err(GalleryError::Corrupt {
                line: 1,
                detail: "empty manifest".to_string(),
            });
        };
        if header != MANIFEST_HEADER {
            return Err(GalleryError::Corrupt {
                line: 1,
                detail: format!("first line must be {MANIFEST_HEADER:?}"),
            });
        }

        let Some((_, revision_line)) = lines.next() else {
            return Err(GalleryError::Corrupt {
                line: 2,
                detail: "missing '# revision: N' line".to_string(),
            });
        };
        let revision_text = revision_line
            .strip_prefix(MANIFEST_REVISION_PREFIX)
            .ok_or_else(|| GalleryError::Corrupt {
                line: 2,
                detail: format!("second line must start with {MANIFEST_REVISION_PREFIX:?}"),
            })?;
        let revision = revision_text
            .parse::<u64>()
            .map_err(|_| GalleryError::Corrupt {
                line: 2,
                detail: format!("revision is not a non-negative integer: {revision_text:?}"),
            })?;
        if revision.to_string() != revision_text {
            return Err(GalleryError::Corrupt {
                line: 2,
                detail: format!(
                    "revision is not in canonical unsigned-decimal form: {revision_text:?}"
                ),
            });
        }

        let Some((_, columns)) = lines.next() else {
            return Err(GalleryError::Corrupt {
                line: 3,
                detail: format!("missing columns line {MANIFEST_COLUMNS:?}"),
            });
        };
        if columns != MANIFEST_COLUMNS {
            return Err(GalleryError::Corrupt {
                line: 3,
                detail: format!("third line must be {MANIFEST_COLUMNS:?}"),
            });
        }

        for (index, line) in lines {
            let line_number = index + 1;
            let corrupt = |detail: String| GalleryError::Corrupt {
                line: line_number,
                detail,
            };
            if line.is_empty() {
                return Err(corrupt("blank lines are not canonical".to_string()));
            }
            if line.starts_with(MANIFEST_REVISION_PREFIX) {
                return Err(corrupt(
                    "duplicate revision line: format v1 requires exactly one".to_string(),
                ));
            }
            if line.starts_with('#') {
                return Err(corrupt(format!(
                    "unexpected comment line {line:?}: format v1 has exactly three prelude lines"
                )));
            }
            let Some([panel, reference, render, verdict, changed]) = split_gallery_row(line) else {
                let field_count = line.split('\t').count();
                return Err(corrupt(format!(
                    "expected 5 tab-separated fields, found {}",
                    field_count
                )));
            };
            if !valid_panel(panel) {
                return Err(corrupt(format!(
                    "invalid panel id {panel:?}: use only [a-z0-9._-], not starting with '.'"
                )));
            }
            if let Some(previous) = previous_panel {
                if panel == previous {
                    return Err(corrupt(format!("duplicate panel {panel:?}")));
                }
                if panel < previous {
                    return Err(corrupt(format!(
                        "panel {panel:?} is out of canonical order after {previous:?}"
                    )));
                }
            }
            previous_panel = Some(panel);
            if !valid_repo_path(reference) {
                return Err(corrupt(format!(
                    "invalid reference path {reference:?}: must be repo-relative and stay \
                     inside the repository"
                )));
            }
            if !valid_repo_path(render) {
                return Err(corrupt(format!(
                    "invalid render path {render:?}: must be repo-relative and stay inside \
                     the repository"
                )));
            }
            let verdict = Verdict::from_token(verdict).ok_or_else(|| {
                corrupt(format!(
                    "unknown verdict {verdict:?}: expected at-least-as-good, \
                     different-but-fine, or regression"
                ))
            })?;
            if changed.is_empty() {
                return Err(corrupt(format!(
                    "panel {panel:?} has an empty change note: record what moved the verdict"
                )));
            }
            rows.push(GalleryRow {
                panel: panel.to_string(),
                reference: reference.to_string(),
                render: render.to_string(),
                verdict,
                changed: changed.to_string(),
            });
        }
        Ok(Self { revision, rows })
    }

    /// Load a manifest from disk.
    ///
    /// # Errors
    /// [`GalleryError::Io`] on read failure, [`GalleryError::Corrupt`] on a
    /// format violation.
    pub fn load(path: &Path) -> Result<Self, GalleryError> {
        let file = std::fs::File::open(path).map_err(|err| GalleryError::Io {
            path: path.to_path_buf(),
            err,
        })?;
        let mut bytes = Vec::new();
        file.take((MAX_MANIFEST_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|err| GalleryError::Io {
                path: path.to_path_buf(),
                err,
            })?;
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(oversized_manifest());
        }
        let text = std::str::from_utf8(&bytes).map_err(|err| GalleryError::Io {
            path: path.to_path_buf(),
            err: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
        })?;
        Self::parse(text)
    }

    fn serialized_len(&self) -> Result<usize, GalleryError> {
        let revision = self.revision.to_string();
        let mut len = MANIFEST_HEADER
            .len()
            .saturating_add(MANIFEST_REVISION_PREFIX.len())
            .saturating_add(revision.len())
            .saturating_add(MANIFEST_COLUMNS.len())
            .saturating_add(3);
        for row in &self.rows {
            len = len
                .saturating_add(row.panel.len())
                .saturating_add(row.reference.len())
                .saturating_add(row.render.len())
                .saturating_add(row.verdict.token().len())
                .saturating_add(row.changed.len())
                .saturating_add(5);
        }
        if len > MAX_MANIFEST_BYTES {
            Err(oversized_manifest())
        } else {
            Ok(len)
        }
    }

    /// The canonical text form: header, revision, column legend, rows sorted
    /// by panel. `to_text(parse(text))` is byte-identical for canonical input.
    ///
    /// # Errors
    /// [`GalleryError::ManifestTooLarge`] if the canonical document would
    /// exceed the format envelope.
    pub fn to_text(&self) -> Result<String, GalleryError> {
        let mut rows: Vec<&GalleryRow> = self.rows.iter().collect();
        rows.sort_by(|a, b| a.panel.cmp(&b.panel));
        let mut out = String::with_capacity(self.serialized_len()?);
        out.push_str(MANIFEST_HEADER);
        out.push('\n');
        out.push_str(MANIFEST_REVISION_PREFIX);
        out.push_str(&self.revision.to_string());
        out.push('\n');
        out.push_str(MANIFEST_COLUMNS);
        out.push('\n');
        for row in rows {
            out.push_str(&row.panel);
            out.push('\t');
            out.push_str(&row.reference);
            out.push('\t');
            out.push_str(&row.render);
            out.push('\t');
            out.push_str(row.verdict.token());
            out.push('\t');
            out.push_str(&row.changed);
            out.push('\n');
        }
        Ok(out)
    }

    /// Save the manifest atomically (tmp file in the same directory, then
    /// rename — the self-golden rig's pattern).
    ///
    /// # Errors
    /// [`GalleryError::ManifestTooLarge`] if the canonical document exceeds
    /// the format envelope; [`GalleryError::Io`] on any filesystem failure.
    pub fn save(&self, path: &Path) -> Result<(), GalleryError> {
        let text = self.to_text()?;
        let io = |err| GalleryError::Io {
            path: path.to_path_buf(),
            err,
        };
        let sequence = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = path.with_extension(format!("tmp{sequence}"));
        {
            let mut file = std::fs::File::create(&tmp).map_err(io)?;
            file.write_all(text.as_bytes()).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        std::fs::rename(&tmp, path).map_err(io)
    }

    /// Move one panel's verdict, recording why, and bump the revision.
    ///
    /// # Errors
    /// [`GalleryError::UnknownPanel`] if the panel is not in the manifest;
    /// [`GalleryError::InvalidChangeNote`] if the note contains a tab or
    /// newline (or is empty); [`GalleryError::RevisionOverflow`] if the
    /// manifest revision is already `u64::MAX`;
    /// [`GalleryError::ManifestTooLarge`] if the updated canonical document
    /// would exceed the format envelope. Every refusal leaves `self`
    /// unchanged.
    pub fn record_verdict(
        &mut self,
        panel: &str,
        verdict: Verdict,
        changed: &str,
    ) -> Result<VerdictChange, GalleryError> {
        if changed.is_empty() || changed.contains(['\t', '\n', '\r']) {
            return Err(GalleryError::InvalidChangeNote);
        }
        let row_index = self
            .rows
            .iter()
            .position(|row| row.panel == panel)
            .ok_or_else(|| GalleryError::UnknownPanel(panel.to_string()))?;
        let next_revision = self
            .revision
            .checked_add(1)
            .ok_or(GalleryError::RevisionOverflow)?;
        let current_len = self.serialized_len()?;
        let row = &self.rows[row_index];
        let from = row.verdict;
        let next_len = current_len
            .saturating_sub(self.revision.to_string().len())
            .saturating_sub(row.verdict.token().len())
            .saturating_sub(row.changed.len())
            .saturating_add(next_revision.to_string().len())
            .saturating_add(verdict.token().len())
            .saturating_add(changed.len());
        if next_len > MAX_MANIFEST_BYTES {
            return Err(oversized_manifest());
        }
        let row = &mut self.rows[row_index];
        row.verdict = verdict;
        row.changed = changed.to_string();
        self.revision = next_revision;
        Ok(VerdictChange {
            panel: panel.to_string(),
            from: Some(from),
            to: verdict,
        })
    }

    /// The panels whose verdict *worsened* relative to an earlier manifest
    /// revision, sorted by panel. Worsened means strictly higher on the
    /// severity order `AtLeastAsGood < DifferentButFine < Regression`; the
    /// `ReferenceCaptureMissing` refusal is incomparable and therefore never
    /// a regression. A
    /// panel that did not exist in `earlier` counts only when it enters at
    /// `Regression` (a new `DifferentButFine` panel is a review item, not a
    /// regression).
    #[must_use]
    pub fn regressions_since(&self, earlier: &Self) -> Vec<VerdictChange> {
        let old: BTreeMap<&str, Verdict> = earlier
            .rows
            .iter()
            .map(|row| (row.panel.as_str(), row.verdict))
            .collect();
        let mut changes = Vec::new();
        for row in &self.rows {
            match old.get(row.panel.as_str()) {
                Some(&from)
                    if from != Verdict::ReferenceCaptureMissing
                        && row.verdict != Verdict::ReferenceCaptureMissing
                        && row.verdict > from =>
                {
                    changes.push(VerdictChange {
                        panel: row.panel.clone(),
                        from: Some(from),
                        to: row.verdict,
                    });
                }
                // ubs:ignore — enum equality on the verdict vocabulary, never a secret.
                None if row.verdict == Verdict::Regression => changes.push(VerdictChange {
                    panel: row.panel.clone(),
                    from: None,
                    to: row.verdict,
                }),
                _ => {}
            }
        }
        changes.sort_by(|a, b| a.panel.cmp(&b.panel));
        changes
    }

    /// Look up one panel's row.
    #[must_use]
    pub fn row(&self, panel: &str) -> Option<&GalleryRow> {
        self.rows.iter().find(|row| row.panel == panel)
    }
}

/// Resolve a manifest against a checkout rooted at `repo_root`, sorted by
/// panel.
///
/// A missing **render** is [`GalleryError::MissingRender`]: the FrankenManim
/// panels are committed artifacts and their absence is a broken checkout (the
/// "missing pair" the tests fail on). A missing **reference** is not an
/// error — the Reference captures are private §15.3 fixtures and a checkout
/// without them simply cannot run the smoke alarm; the returned
/// [`ResolvedPair::reference_present`] flag says which pairs are measurable.
///
/// # Errors
/// [`GalleryError::MissingRender`] for the first panel whose render is absent.
pub fn render_pairs(
    manifest: &GalleryManifest,
    repo_root: &Path,
) -> Result<Vec<ResolvedPair>, GalleryError> {
    let mut pairs = Vec::with_capacity(manifest.rows.len());
    for row in &manifest.rows {
        let render = repo_root.join(&row.render);
        if !render.is_file() {
            return Err(GalleryError::MissingRender {
                panel: row.panel.clone(),
                path: render,
            });
        }
        let reference = repo_root.join(&row.reference);
        pairs.push(ResolvedPair {
            panel: row.panel.clone(),
            reference_present: reference.is_file(),
            reference,
            render,
        });
    }
    pairs.sort_by(|a, b| a.panel.cmp(&b.panel));
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::chamfer_far;

    #[test]
    fn chamfer_sentinel_covers_the_full_u32_dimension_domain() {
        let far = chamfer_far(u32::MAX, u32::MAX);
        assert_eq!(far, 25_769_803_774);
        assert!(far > u64::from(u32::MAX));
    }
}
