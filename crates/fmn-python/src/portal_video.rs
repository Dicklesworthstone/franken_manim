//! SceneFileWriter → Reel negotiation. This adapter owns no encoder or process.
//!
//! A transparent camera selects alpha-preserving MOV; the ordinary writer
//! defaults are promoted to RGBA/qtrle. Explicit incompatible choices fail
//! before opening the encoder or reserving an output generation.

mod quality;

#[cfg(test)]
mod conversion_tests;

use fmn_frame::convert::{rgba16f_to_bgra8, rgba16f_to_nv12, rgba16f_to_p010, rgba16f_to_rgba8};
use fmn_frame::{ChromaSiting, ColorRange, FrameBuffer, FrameError, FrameLayout, PixelFormat};
use fmn_output::{ColorDescription, Container, EncoderChoice, VideoJob, WireFormat};
use pyo3::exceptions::{PyAttributeError, PyTypeError};
use pyo3::prelude::*;
use pyo3::types::PyBool;

use crate::{CapabilityError, PortalFrameFormat, PyRuntimeError, PyScene, PyValueError};

pub(crate) struct PortalVideoConfig {
    pub(crate) ffmpeg_bin: String,
    pub(crate) job: VideoJob,
    pub(crate) audio_quality: fmn_output::AudioQuality,
    pub(crate) job_limits: fmn_output::JobLimits,
    pub(crate) max_input_bytes: u64,
}

impl PortalVideoConfig {
    pub(crate) fn from_scene(
        scene: &Bound<'_, PyScene>,
        format: PortalFrameFormat,
        width: u32,
        height: u32,
        fps: u32,
    ) -> PyResult<Self> {
        check_scene_ownership(scene)?;
        let config = Self::parse_scene(scene, format, width, height, fps)?;
        check_scene_ownership(scene)?;
        Ok(config)
    }

    pub(crate) fn from_recording_scene(
        scene: &Bound<'_, PyScene>,
        format: PortalFrameFormat,
        width: u32,
        height: u32,
        fps: u32,
    ) -> PyResult<Self> {
        crate::portal_recording::check_available(scene)?;
        let config = Self::parse_scene(scene, format, width, height, fps)?;
        crate::portal_recording::check_available(scene)?;
        Ok(config)
    }

    fn parse_scene(
        scene: &Bound<'_, PyScene>,
        format: PortalFrameFormat,
        width: u32,
        height: u32,
        fps: u32,
    ) -> PyResult<Self> {
        // No engine borrow may cross a Python descriptor/callback.
        let writer = scene.getattr("file_writer")?;
        let ffmpeg_bin: String = writer.getattr("ffmpeg_bin")?.extract()?;
        let codec: String = writer.getattr("video_codec")?.extract()?;
        let pixel_format: String = writer.getattr("pixel_format")?.extract()?;
        for name in ["saturation", "gamma"] {
            let value: f64 = writer.getattr(name)?.extract()?;
            if value != 1.0 {
                return Err(CapabilityError::new_err(format!(
                    "non-default file_writer {name} requires a native color-transform stage; ffmpeg filters are forbidden"
                )));
            }
        }
        let rgba: [f64; 4] = scene
            .getattr("camera")?
            .getattr("background_rgba")?
            .extract()?;
        if !rgba.iter().all(|component| component.is_finite()) || !(0.0..=1.0).contains(&rgba[3]) {
            return Err(PyValueError::new_err(
                "camera background must be finite with opacity in 0..=1",
            ));
        }
        if ffmpeg_bin.is_empty() || ffmpeg_bin.contains('\0') {
            return Err(PyValueError::new_err(
                "file_writer.ffmpeg_bin must be nonempty and contain no NUL",
            ));
        }
        let quality = quality::from_writer(&writer)?;
        let audio_quality = quality::audio_from_writer(&writer)?;
        let job = video_job(
            format,
            width,
            height,
            fps,
            &codec,
            &pixel_format,
            rgba[3] < 1.0,
        )
        .map_err(PyValueError::new_err)?
        .with_quality(quality)
        .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let layout = FrameLayout::tight(job.wire.frame_format(), width, height)
            .map_err(|error| PyValueError::new_err(error.to_string()))?;
        let frame_bytes = u64::try_from(layout.total_bytes())
            .map_err(|_| crate::PyOverflowError::new_err("render frame size exceeds u64"))?;
        let default_input_bytes = frame_bytes
            .checked_mul(crate::PORTAL_MAX_RENDER_FRAMES)
            .ok_or_else(|| crate::PyOverflowError::new_err("render stream budget exceeds u64"))?;
        let (limits, max_input_bytes) = limits_from_writer(&writer, default_input_bytes)?;
        if max_input_bytes < frame_bytes {
            return Err(PyValueError::new_err(
                "file_writer.ffmpeg_max_input_bytes cannot hold one output frame",
            ));
        }
        let job_limits = fmn_output::admit_render_ffmpeg_limits(&limits, max_input_bytes)
            .map_err(PyValueError::new_err)?;
        // Python getters may adopt mobjects, advance time, or recursively
        // acquire a generation. Never replace that state or probe an encoder
        // against the now-stale preflight in begin_portal_render.
        Ok(Self {
            ffmpeg_bin,
            job,
            audio_quality,
            job_limits,
            max_input_bytes,
        })
    }
}

fn positive_limit(writer: &Bound<'_, PyAny>, name: &str) -> PyResult<Option<u64>> {
    let value = match writer.getattr(name) {
        Ok(value) if value.is_none() => return Ok(None),
        Ok(value) => value,
        Err(error) if error.is_instance_of::<PyAttributeError>(writer.py()) => return Ok(None),
        Err(error) => return Err(error),
    };
    if value.is_instance_of::<PyBool>() {
        return Err(PyTypeError::new_err(format!(
            "file_writer.{name} must be a positive integer, not bool"
        )));
    }
    let value = value.extract::<u64>()?;
    if value == 0 {
        return Err(PyValueError::new_err(format!(
            "file_writer.{name} must be nonzero"
        )));
    }
    Ok(Some(value))
}

pub(crate) fn limits_from_writer(
    writer: &Bound<'_, PyAny>,
    default_input_bytes: u64,
) -> PyResult<(fmn_output::JobLimits, u64)> {
    let mut limits = fmn_output::default_render_ffmpeg_limits();
    if let Some(seconds) = positive_limit(writer, "ffmpeg_timeout")? {
        limits.timeout = std::time::Duration::from_secs(seconds);
    }
    if let Some(bytes) = positive_limit(writer, "ffmpeg_max_log_bytes")? {
        limits.max_log_bytes = bytes;
    }
    if let Some(bytes) = positive_limit(writer, "ffmpeg_max_artifact_bytes")? {
        limits.max_artifact_bytes = bytes;
    }
    let max_input_bytes = positive_limit(writer, "ffmpeg_max_input_bytes")?
        .unwrap_or(default_input_bytes)
        .min(default_input_bytes);
    // A compressed audio source may expand well beyond its byte ceiling.
    // Validate job bounds independently here; only the video adapter
    // intersects its artifact allowance with cumulative raw-frame input.
    let limits = fmn_output::admit_render_ffmpeg_limits(&limits, limits.max_artifact_bytes)
        .map_err(PyValueError::new_err)?;
    Ok((limits, max_input_bytes))
}

pub(crate) fn check_scene_ownership(scene: &Bound<'_, PyScene>) -> PyResult<()> {
    // A fresh scene's arena holds its own listed camera frame (fm-6l2i), a
    // point-less entry the arena replacement carries across.
    let frame = crate::scene_frame_entry(scene)?.map(|(_, mob)| mob);
    let scene = scene.try_borrow()?;
    if scene
        .render
        .lock()
        .map_err(|_| PyRuntimeError::new_err("portal render session lock was poisoned"))?
        .is_some()
    {
        return Err(PyRuntimeError::new_err(
            "a portal render generation is already active",
        ));
    }
    let only_frame = |mob: &fmn_mobject::Mob| Some(*mob) == frame;
    if !scene.proxies.borrow().keys().all(only_frame)
        || !scene.engine.borrow().stage().roots().iter().all(only_frame)
        || scene.engine.borrow().stage().time() != 0.0
        || !scene.engine.borrow().sound_requests().is_empty()
    {
        return Err(PyRuntimeError::new_err(
            "render configuration must be installed before Scene construction mutates engine state",
        ));
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn video_job(
    format: PortalFrameFormat,
    width: u32,
    height: u32,
    fps: u32,
    codec: &str,
    pixel_format: &str,
    transparent: bool,
) -> Result<VideoJob, String> {
    if transparent && format != PortalFrameFormat::Mov {
        return Err("transparent video requires MOV output; select format='mov'".into());
    }
    let wire =
        WireFormat::from_config(pixel_format, transparent).map_err(|error| error.to_string())?;
    let codec = codec.trim();
    if codec.is_empty()
        || !codec
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
    {
        return Err("file_writer.video_codec must be a nonempty encoder name (letters, digits, underscores)".into());
    }
    let encoder = if codec.eq_ignore_ascii_case("auto") || (transparent && codec == "libx264") {
        EncoderChoice::Auto
    } else {
        EncoderChoice::Named(codec.to_owned())
    };
    let job = VideoJob {
        width,
        height,
        fps: (fps, 1),
        wire,
        color: ColorDescription::for_native_wire(wire),
        container: if transparent {
            Container::MovTransparent
        } else if format == PortalFrameFormat::Mov {
            Container::Mov
        } else {
            Container::Mp4
        },
        encoder,
        crf: None,
    };
    let encoder = job.resolved_encoder().map_err(|error| error.to_string())?;
    if transparent && encoder.as_deref() != Some("qtrle") {
        return Err("transparent video currently requires the qtrle encoder".into());
    }
    // Both ordinary container paths encode 4:2:0, even for an RGBA input wire.
    if !transparent && (!width.is_multiple_of(2) || !height.is_multiple_of(2)) {
        return Err("ordinary video requires even width and height for 4:2:0 output".into());
    }
    Ok(job)
}

/// One bounded wire-format frame retained by an output generation.
///
/// `unchanged` is supplied only by Lumen's successful raw-pixel identity
/// proof, never inferred from the clock, a wait(), or tile-hit counts. Every
/// call still fills a distinct ordered-emitter reservation. No output slot or
/// mutable renderer buffer is retained across publication.
pub(crate) struct RetainedConversion {
    frame: FrameBuffer,
    valid: bool,
}

impl RetainedConversion {
    pub(crate) fn new(layout: FrameLayout) -> Self {
        Self {
            frame: FrameBuffer::new(layout),
            valid: false,
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        self.frame.layout().total_bytes()
    }

    /// Return true only when a previous conversion supplied this frame.
    pub(crate) fn write_frame(
        &mut self,
        source: &FrameBuffer,
        destination: &mut FrameBuffer,
        unchanged: bool,
    ) -> Result<bool, FrameError> {
        let reuse = unchanged && self.valid;
        // Retire the cached result across every fallible operation. In
        // particular an invalid source cannot leave a reusable success flag.
        self.valid = false;
        if destination.layout() != self.frame.layout()
            || source.layout().width() != destination.layout().width()
            || source.layout().height() != destination.layout().height()
        {
            return Err(FrameError::DimensionMismatch);
        }
        if source.layout().format() != PixelFormat::Rgba16F {
            return Err(FrameError::FormatMismatch {
                expected: "Rgba16F raw frame",
                got: source.layout().format(),
            });
        }
        if reuse {
            destination
                .as_bytes_mut()
                .copy_from_slice(self.frame.as_bytes());
        } else {
            convert_frame(source, destination, None)?;
            self.frame
                .as_bytes_mut()
                .copy_from_slice(destination.as_bytes());
        }
        self.valid = true;
        Ok(reuse)
    }
}

/// Convert from Lumen's linear-light frame directly to the negotiated wire.
/// P010 must not pass through RGBA8: doing so irreversibly discards the extra
/// precision before the 10-bit encoder ever sees it. The fused 8-bit kernels
/// retain the existing RGBA/BGRA/NV12 bytes, orientation and alpha policy.
///
/// The legacy session scratch argument is retained for existing callers, but
/// is never read or written. The destination format, not scratch presence,
/// determines the conversion, and none of these paths allocates frame scratch.
pub(crate) fn convert_frame(
    source: &FrameBuffer,
    destination: &mut FrameBuffer,
    _scratch: Option<&mut FrameBuffer>,
) -> Result<(), FrameError> {
    match destination.layout().format() {
        PixelFormat::Rgba8 => rgba16f_to_rgba8(source, destination),
        PixelFormat::Bgra8 => rgba16f_to_bgra8(source, destination),
        PixelFormat::Nv12 => {
            rgba16f_to_nv12(source, destination, ColorRange::Limited, ChromaSiting::Left)
        }
        PixelFormat::P010 => {
            rgba16f_to_p010(source, destination, ColorRange::Limited, ChromaSiting::Left)
        }
        _ => Err(FrameError::UnsupportedConversion(
            "unsupported portal output wire",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_video_defaults_and_all_wire_aliases() {
        for (name, expected) in [
            ("auto", WireFormat::Nv12),
            ("rgba", WireFormat::Rgba8),
            ("rgba8", WireFormat::Rgba8),
            ("BGRA", WireFormat::Bgra8),
            ("bgra8", WireFormat::Bgra8),
            ("nv12", WireFormat::Nv12),
            ("yuv420p", WireFormat::Nv12),
            ("p010", WireFormat::P010),
            ("p010le", WireFormat::P010),
            ("yuv420p10le", WireFormat::P010),
        ] {
            let job = video_job(PortalFrameFormat::Mp4, 96, 54, 8, "libx264", name, false).unwrap();
            assert_eq!(job.wire, expected);
            assert_eq!(job.color.transfer, fmn_output::Transfer::Srgb);
            assert_eq!(job.color.primaries, fmn_output::Primaries::Bt709);
            assert_eq!(
                job.color.range,
                if expected.has_alpha() {
                    ColorRange::Full
                } else {
                    ColorRange::Limited
                }
            );
            assert_eq!(job.resolved_encoder().unwrap().as_deref(), Some("libx264"));
            assert_eq!(job.container, Container::Mp4);
        }
    }

    #[test]
    fn portal_video_transparent_defaults_and_bgra_preserve_alpha() {
        for name in ["auto", "yuv420p", "rgba", "bgra"] {
            let job = video_job(PortalFrameFormat::Mov, 95, 53, 8, "libx264", name, true).unwrap();
            assert!(job.wire.has_alpha());
            assert_eq!(job.container, Container::MovTransparent);
            assert_eq!(job.resolved_encoder().unwrap().as_deref(), Some("qtrle"));
            assert_eq!(job.color, ColorDescription::srgb_full());
        }
    }

    #[test]
    fn portal_video_incompatible_profiles_fail_before_process_creation() {
        for (format, codec, wire, transparent) in [
            (PortalFrameFormat::Mp4, "libx264", "rgba", true),
            (PortalFrameFormat::Mov, "qtrle", "nv12", true),
            (PortalFrameFormat::Mov, "qtrle", "p010", true),
            (PortalFrameFormat::Mov, "libx265", "rgba", true),
            (PortalFrameFormat::Mp4, "libx264", "yuv444p", false),
            (PortalFrameFormat::Mp4, "", "rgba", false),
            (PortalFrameFormat::Mp4, "libx264 -vf eq", "rgba", false),
        ] {
            assert!(video_job(format, 96, 54, 8, codec, wire, transparent).is_err());
        }
        assert!(video_job(PortalFrameFormat::Mp4, 95, 54, 8, "auto", "rgba", false).is_err());
    }

    #[test]
    fn portal_video_callback_ownership_is_fail_closed() {
        crate::with_python_test_module("portal video ownership", |py, _module, globals| {
            let source =
                std::ffi::CString::new(include_str!("../tests/video_ownership.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("writer callbacks cannot invalidate generation ownership");
        });
    }

    #[test]
    fn portal_retained_conversion_production_acceptance() {
        crate::with_python_test_module("retained portal conversion", |py, _module, globals| {
            let source = std::ffi::CString::new(include_str!("../tests/retained_conversion.py"))
                .expect("retained conversion suite contains no NUL");
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("real portal pixels, callbacks, timing, routes and conversion reuse");
        });
    }

    #[test]
    fn portal_video_production_acceptance_suite() {
        crate::with_python_test_module("portal video negotiation", |py, _module, globals| {
            let source = std::ffi::CString::new(include_str!("../tests/video_options.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("real decoded negotiated video output");
        });
    }
}
