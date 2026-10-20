//! Generate a self-contained audible browser demo without source media assets.
use fmn_scene::timeline_bundle::{BundleSoundtrack, attach_bundle_soundtrack};
use std::{fs::OpenOptions, io::Write, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let output = args
        .next()
        .map(PathBuf::from)
        .ok_or("usage: export_audio_bundle OUTPUT.fmtl")?;
    if args.next().is_some() {
        return Err("usage: export_audio_bundle OUTPUT.fmtl".into());
    }
    let pictures = fmn_wasm::demo_bundle::demo_bundle()?;
    let rate = 48_000;
    let frames = 84_000;
    let mut samples = Vec::with_capacity(frames * 2);
    for frame in 0..frames {
        let t = frame as f64 / f64::from(rate);
        let envelope =
            ((t - 0.25) * 20.0).clamp(0.0, 1.0) * ((1.75 - t) * 10.0).clamp(0.0, 1.0) * 0.15;
        for frequency in [440.0, 660.0] {
            samples.push((envelope * fmn_dmath::sin(std::f64::consts::TAU * frequency * t)) as f32);
        }
    }
    let audio = BundleSoundtrack::new(rate, 2, samples)?;
    let bytes = attach_bundle_soundtrack(&pictures, &audio, usize::MAX)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    println!(
        "{}: {} bytes, 45 pictures at 30 fps, stereo PCM at 48000 Hz",
        output.display(),
        bytes.len()
    );
    Ok(())
}
