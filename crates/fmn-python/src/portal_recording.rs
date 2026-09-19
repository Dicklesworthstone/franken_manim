//! Attach Reel output to an existing Scene without replacing its arena or clock.
//!
//! Full renders configure a pristine Scene; insert recordings instead inherit
//! the live rational clock and RNG. Only publication state is new. The capture
//! sink, ordered emitter, codecs, audio mixer and ffmpeg boundary are shared.

use super::*;

pub(crate) fn install(module: &Bound<'_, PyModule>) -> PyResult<()> {
    // This runs after the schema bootstrap. Do not recreate PyO3's __all__:
    // these are private host boundaries, not a new wildcard import surface.
    module.setattr("_portal_begin_recording", wrap_pyfunction!(_portal_begin_recording, module)?)?;
    module.setattr("_portal_scene_clock", wrap_pyfunction!(_portal_scene_clock, module)?)?;
    Ok(())
}

pub(crate) fn check_available(scene: &Bound<'_, PyScene>) -> PyResult<()> {
    let namespace = scene.getattr("__dict__")?;
    let namespace = namespace.cast::<PyDict>()?;
    if namespace
        .get_item("_fmn_scene_execution")?
        .is_some_and(|owner| !owner.is_none())
    {
        return Err(PyRuntimeError::new_err(
            "recording must start between play/wait calls, not inside an animation or updater",
        ));
    }
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
    Ok(())
}

#[pyfunction]
fn _portal_scene_clock(scene: &Bound<'_, PyScene>) -> PyResult<(u32, i64)> {
    let scene = scene.try_borrow()?;
    let engine = scene.engine.borrow();
    Ok((engine.fps(), engine.time().frames()))
}

fn recording_format(format: &str) -> PyResult<PortalOutputFormat> {
    let frame_format = match format {
        "png_sequence" => PortalFrameFormat::PngSequence,
        "gif" => PortalFrameFormat::Gif,
        "y4m" => PortalFrameFormat::Y4m,
        "mp4" => PortalFrameFormat::Mp4,
        "mov" => PortalFrameFormat::Mov,
        "wav" => return Ok(PortalOutputFormat::Wav),
        _ => {
            return Err(CapabilityError::new_err(
                "live recording requires png_sequence, gif, y4m, wav, mp4, or mov; use Camera.capture for a still",
            ));
        }
    };
    Ok(PortalOutputFormat::Frames(frame_format))
}

#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn _portal_begin_recording(
    scene: &Bound<'_, PyScene>,
    destination: String,
    format: &str,
    width: u32,
    height: u32,
    fps: u32,
    threads: usize,
) -> PyResult<i64> {
    if destination.is_empty() || destination.contains('\0') {
        return Err(PyValueError::new_err("recording destination must be nonempty and contain no NUL"));
    }
    if width == 0 || height == 0 || u64::from(width) * u64::from(height) > 16_777_216
        || !(1..=96).contains(&threads)
    {
        return Err(PyValueError::new_err(
            "recording requires positive dimensions, at most 16777216 pixels, and 1..96 threads",
        ));
    }
    let destination = std::path::absolute(destination).map_err(native_error)?;
    let format = recording_format(format)?;
    check_available(scene)?;
    if fps != scene.borrow().engine.borrow().fps() {
        return Err(PyValueError::new_err(
            "recording FPS must equal the live Scene clock; it cannot resample or reset an existing scene",
        ));
    }
    let video = match &format {
        PortalOutputFormat::Frames(frame @ (PortalFrameFormat::Mp4 | PortalFrameFormat::Mov)) => {
            Some(portal_video::PortalVideoConfig::from_recording_scene(scene, *frame, width, height, fps)?)
        }
        _ => None,
    };
    let mut audio = portal_audio::PortalAudio::from_scene(scene, &format, video.as_ref())?;
    // Authored descriptors above may have run arbitrary Python. Recheck before
    // opening output; never steal a recursively acquired generation.
    check_available(scene)?;
    let engine = Rc::clone(&scene.borrow().engine);
    let (start_frame, first_cue) = {
        let live = engine.borrow();
        if live.fps() != fps {
            return Err(PyValueError::new_err("the live Scene clock changed during recording preflight"));
        }
        (live.time().frames(), live.sound_requests().len())
    };
    if start_frame < 0 {
        return Err(PyValueError::new_err("recording cannot start at a negative scene frame"));
    }
    audio.begin_at_request(first_cue);
    let (mut session, _unused_runtime_config) = PortalRenderSession::new(
        destination, width, height, fps, threads, format, video, audio, false,
    )?;
    // Crucially, do NOT build or install a replacement Scene/EngineState here.
    // Every existing mobject, live NumPy view, updater and RNG keeps its owner.
    session.output_timeline_mut().start_at(start_frame)?;
    let slot = Arc::clone(&scene.borrow().render);
    let mut output = match slot.lock() {
        Ok(output) => output,
        Err(_) => {
            session.abort();
            return Err(PyRuntimeError::new_err("portal render session lock was poisoned"));
        }
    };
    if output.is_some() {
        session.abort();
        return Err(PyRuntimeError::new_err("a portal render generation is already active"));
    }
    {
        let mut owner = scene.borrow_mut();
        owner.render_invocations.clear();
        owner.render_audio_inputs.clear();
    }
    *output = Some(session);
    Ok(start_frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_does_not_admit_endpoint_only_or_unknown_formats() {
        for format in ["png", "svg", "jpeg", "", "PNG_SEQUENCE"] {
            assert!(recording_format(format).is_err(), "{format}");
        }
        for format in ["png_sequence", "gif", "y4m", "wav", "mp4", "mov"] {
            assert!(recording_format(format).is_ok(), "{format}");
        }
    }

    #[test]
    fn native_live_recording_acceptance() {
        crate::with_python_test_module("live recording", |py, _module, globals| {
            globals.set_item("__file__", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/live_recording.py")).unwrap();
            let source = CString::new(include_str!("../tests/live_recording.py")).unwrap();
            py.run(source.as_c_str(), Some(globals), Some(globals))
                .inspect_err(|error| error.print(py))
                .expect("real native live recording and audio rebasing");
        });
    }
}
