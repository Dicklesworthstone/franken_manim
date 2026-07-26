//! fm-k77 — the G0-2 renderer look study.
//!
//! Renders the four calibration panels the analytic prototype can express, in
//! the *same pixel coordinates* as the one-time Reference captures, so the two
//! sets can be flipped between rather than merely described. The Reference
//! stills live in `gallery/reference_captures/` (gitignored, private per the
//! §15.3 fixture policy); these renders are our own primitives and are
//! committed under `docs/g0/g0-2-renders/`.
//!
//! What the comparison is FOR: §20.1 spike 2 fixes Lumen's aesthetic constants
//! before W5 scales. The constants themselves are decided in
//! `docs/g0/G0-2-look-study-ratification.md` from the Reference's shader source
//! and from measurements of the captures; these images are the visual half of
//! that evidence — the part a human reviewer signs off on (R2).
//!
//! ```text
//! cargo run --release --bin g0_2_look [-- <output-dir>]
//! ```

use fmn_spike_accelerator::cpu::{self, Surface};
use fmn_spike_accelerator::scene::{self, CalibrationPanel};

/// Capture resolution. The Reference stills are 1920x1080 and the panel
/// geometry is measured in those pixels, so anything else would defeat the
/// registration this study depends on.
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const TILE: u32 = 16;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir)?;

    println!("G0-2 look study — analytic prototype vs the captured Reference");
    println!("  {WIDTH}x{HEIGHT}, tile {TILE}, aa_width 1.5 px (VMobject default)");
    println!();

    for panel in CalibrationPanel::ALL {
        let ir = scene::calibration(panel, WIDTH, HEIGHT, TILE);
        let surface = cpu::render(&ir);
        let name = format!("fmn-{}.png", panel.id().replace('_', "-"));
        write_png(dir, &name, &surface)?;
        println!(
            "    {:<20} paths {:>3}  segments {:>5}  styles {:>2}  {}",
            panel.id(),
            ir.paths.len(),
            ir.segments.len(),
            ir.styles.len(),
            describe(&surface),
        );
    }

    println!();
    println!("Compare against gallery/reference_captures/<id>.png (same pixel grid).");
    Ok(())
}

/// A one-line liveness summary, so a blank render cannot be reported as a
/// successful one — the failure mode that cost the capture harness a full run
/// (see docs/look_gallery/CAPTURE_INVENTORY.md).
fn describe(s: &Surface) -> String {
    let bg = s.get(2, 2);
    let mut moved = 0usize;
    let mut peak = 0.0f32;
    for y in 0..s.height {
        for x in 0..s.width {
            let p = s.get(x, y);
            let d = (0..3).map(|i| (p[i] - bg[i]).abs()).fold(0.0, f32::max);
            if d > 1.0 / 255.0 {
                moved += 1;
            }
            peak = peak.max(d);
        }
    }
    let total = (s.width * s.height) as f64;
    format!(
        "non-background {:.2}%  peak delta {:.3}",
        100.0 * moved as f64 / total,
        peak
    )
}

fn write_png(
    dir: &std::path::Path,
    name: &str,
    s: &Surface,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = dir.join(name);
    let bytes = fmn_codec::png::encode_rgba8(
        s.width,
        s.height,
        &s.to_srgb8(),
        fmn_codec::deflate::CompressionLevel::Default,
    );
    std::fs::write(&path, bytes)?;
    println!("      wrote {}", path.display());
    Ok(())
}
