//! fm-k77 — the G0-2 renderer look study.
//!
//! Renders the four 2D calibration panels in the *same pixel coordinates* as
//! the one-time Reference captures, so the two sets can be flipped between
//! rather than merely described. The gradient panel runs through production
//! Lumen; the three panels whose look was already ratified remain the compact
//! analytic calibration fixtures. The Reference stills live in
//! `gallery/reference_captures/` (gitignored, private per the §15.3 fixture
//! policy); these renders are our own primitives and are committed under
//! `docs/g0/g0-2-renders/`.
//!
//! What the comparison is FOR: §20.1 spike 2 fixes Lumen's aesthetic constants
//! before W5 scales. The constants themselves are decided in
//! `docs/g0/G0-2-look-study-ratification.md` from the Reference's shader source
//! and from measurements of the captures; these images are the visual half of
//! that evidence — the part a human reviewer signs off on (R2).
//!
//! ```text
//! cargo run --release --bin g0_2_look [-- <output-dir> [--gradient-only|--lighting-only]]
//! ```

use fmn_core::color::Srgb;
use fmn_core::constants::{BLUE_E, GREEN, PURPLE, RED, TEAL, YELLOW};
use fmn_frame::{FrameBuffer, PixelFormat};
use fmn_geom::QuadPath;
use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_render::bin::{Binning, ScreenMap, Tiling, Viewport};
use fmn_render::engine::{FrameConfig, FrameJob};
use fmn_render::fill::MonoTable;
use fmn_render::plan::RenderPlan;
use fmn_render::{
    CameraConfig, CameraFrame, SurfaceDraw, SurfaceMesh, SurfaceVertex, ThreeDCamera, ThreeDDraw,
    ThreeDJob,
};
use fmn_spike_accelerator::cpu::{self, Surface};
use fmn_spike_accelerator::scene::{self, CalibrationPanel};

/// Capture resolution. The Reference stills are 1920x1080 and the panel
/// geometry is measured in those pixels, so anything else would defeat the
/// registration this study depends on.
const WIDTH: u32 = 1920;
const HEIGHT: u32 = 1080;
const TILE: u32 = 16;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| ".".into());
    let modes = args.collect::<Vec<_>>();
    let lighting_only = modes.iter().any(|argument| argument == "--lighting-only");
    let gradient_only = modes.iter().any(|argument| argument == "--gradient-only");
    if lighting_only && gradient_only {
        return Err("--gradient-only and --lighting-only are mutually exclusive".into());
    }
    let dir = std::path::Path::new(&dir);
    std::fs::create_dir_all(dir)?;

    println!("G0-2 look study — analytic prototype vs the captured Reference");
    println!("  {WIDTH}x{HEIGHT}, tile {TILE}, aa_width 1.5 px (VMobject default)");
    println!();

    if !lighting_only {
        for panel in CalibrationPanel::ALL {
            if gradient_only && panel != CalibrationPanel::GradientFills {
                continue;
            }
            if panel == CalibrationPanel::GradientFills {
                let frame = render_gradient_fills()?;
                let name = format!("fmn-{}.png", panel.id().replace('_', "-"));
                write_frame_png(dir, &name, &frame)?;
                println!(
                    "    {:<20} production Lumen  {}",
                    panel.id(),
                    describe_frame(&frame),
                );
                continue;
            }
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
    }

    if !gradient_only {
        let lighting = render_lighting_3d()?;
        write_frame_png(dir, "fmn-lighting-3d.png", &lighting)?;
        println!(
            "    {:<20} fixed UV sphere  {}",
            "lighting_3d",
            describe_frame(&lighting),
        );
    }

    println!();
    println!("Compare against gallery/reference_captures/<id>.png (same pixel grid).");
    Ok(())
}

fn render_gradient_fills() -> Result<FrameBuffer, Box<dyn std::error::Error>> {
    let mut stage = Stage::new();

    // Pixel coordinates measured from the Reference capture. Using an identity
    // ScreenMap keeps the review image registered without making those
    // measurements part of any runtime API.
    // The Reference Square is `UR -> UL -> DL -> DR`. Its camera flips world
    // y into image rows, so preserve that *screen-space* point order here:
    // top-right -> top-left -> bottom-left -> bottom-right.
    let square = polygon(&[
        [930.0, 332.5],
        [516.0, 332.5],
        [516.0, 746.5],
        [930.0, 746.5],
    ])?;
    add_gradient(
        &mut stage,
        &square,
        GradientStyle {
            fill: (BLUE_E, YELLOW, 0.8),
            stroke: (RED, GREEN, 810.0, 1.0),
        },
    )?;

    let circle = QuadPath::arc(
        0.0,
        -std::f64::consts::TAU,
        205.48,
        [1195.86, 539.49, 0.0],
        None,
    );
    add_gradient(
        &mut stage,
        &circle,
        GradientStyle {
            fill: (PURPLE, TEAL, 0.5),
            stroke: (RED, RED, 540.0, 1.0),
        },
    )?;

    let config = FrameConfig::new(
        Viewport {
            width: WIDTH,
            height: HEIGHT,
        },
        ScreenMap {
            scale: 1.0,
            origin: [0.0, 0.0],
        },
        Srgb::from_rgb8(0x33, 0x33, 0x33).to_linear(1.0),
    );
    let mut plan = RenderPlan::new();
    plan.sync(&stage, 0)?;
    let mono = MonoTable::build(&plan, config.map);
    let mut binning = Binning::build(&plan, config.viewport, Tiling::default(), config.map)?;
    binning.prune_occluded(&plan)?;
    Ok(FrameJob::new(&plan, &mono, &binning, config)?.render(4)?)
}

#[derive(Debug, Clone, Copy)]
struct GradientStyle {
    fill: (Srgb, Srgb, f64),
    stroke: (Srgb, Srgb, f64, f64),
}

fn add_gradient(
    stage: &mut Stage,
    path: &QuadPath,
    style: GradientStyle,
) -> Result<Mob, Box<dyn std::error::Error>> {
    let mob = stage.add(gradient_mobject(path, style)?);
    stage.add_to_scene(mob)?;
    Ok(mob)
}

#[allow(clippy::cast_possible_truncation)]
fn gradient_mobject(
    path: &QuadPath,
    style: GradientStyle,
) -> Result<Mobject, Box<dyn std::error::Error>> {
    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), path.points().len());
    let fill = srgb_record(style.fill.0, style.fill.2);
    let stroke = srgb_record(style.stroke.0, style.stroke.3);
    for (index, point) in path.points().iter().enumerate() {
        if !buffer.write(
            index,
            "point",
            &[point[0] as f32, point[1] as f32, point[2] as f32],
        ) || !buffer.write(index, "fill_rgba", &fill)
            || !buffer.write(index, "stroke_rgba", &stroke)
            || !buffer.write(index, "stroke_width", &[style.stroke.2 as f32])
        {
            return Err("gallery VMobject schema is missing a required render field".into());
        }
    }
    let last = buffer
        .len()
        .checked_sub(1)
        .ok_or("gallery path unexpectedly has no records")?;
    if !buffer.write(last, "fill_rgba", &srgb_record(style.fill.1, style.fill.2))
        || !buffer.write(
            last,
            "stroke_rgba",
            &srgb_record(style.stroke.1, style.stroke.3),
        )
    {
        return Err("gallery gradient fields are absent from the VMobject schema".into());
    }
    Ok(Mobject::from_buffer(buffer))
}

fn srgb_record(color: Srgb, alpha: f64) -> [f32; 4] {
    [color.r as f32, color.g as f32, color.b as f32, alpha as f32]
}

fn polygon(points: &[[f64; 2]]) -> Result<QuadPath, Box<dyn std::error::Error>> {
    let first = points
        .first()
        .ok_or("gallery polygon unexpectedly has no points")?;
    let mut path = QuadPath::default();
    path.start_new_path([first[0], first[1], 0.0]);
    for point in &points[1..] {
        path.add_line_to([point[0], point[1], 0.0], false)?;
    }
    path.add_line_to([first[0], first[1], 0.0], false)?;
    Ok(path)
}

fn render_lighting_3d() -> Result<FrameBuffer, Box<dyn std::error::Error>> {
    let background = Srgb::from_hex("#333333")?.to_linear(1.0);
    let mut frame = CameraFrame::default();
    frame.set_euler_angles(
        Some(20.0_f64.to_radians()),
        Some(70.0_f64.to_radians()),
        Some(0.0),
    )?;
    let camera = ThreeDCamera::new(CameraConfig {
        resolution: (WIDTH, HEIGHT),
        background,
        samples: 4,
        frame,
        ..CameraConfig::default()
    })?;
    let color = Srgb::from_hex("#1C758A")?.to_linear(1.0);
    let resolution = (101, 51);
    let mut vertices = Vec::with_capacity((resolution.0 * resolution.1) as usize);
    for u_index in 0..resolution.0 {
        let u = std::f64::consts::TAU * f64::from(u_index) / f64::from(resolution.0 - 1);
        for v_index in 0..resolution.1 {
            let v = std::f64::consts::PI * f64::from(v_index) / f64::from(resolution.1 - 1);
            let radial = fmn_dmath::sin(v);
            let normal = [
                fmn_dmath::cos(u) * radial,
                fmn_dmath::sin(u) * radial,
                -fmn_dmath::cos(v),
            ];
            vertices.push(SurfaceVertex::colored(
                [2.0 * normal[0], 2.0 * normal[1], 2.0 * normal[2]],
                normal,
                color,
            ));
        }
    }
    let mesh = SurfaceMesh::from_uv_grid(vertices, resolution)?;
    let draw = SurfaceDraw::new(&mesh);
    let draws = [ThreeDDraw::Surface(draw)];
    Ok(ThreeDJob::new(&camera, &draws, Tiling::default())?.render(4)?)
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

fn describe_frame(frame: &FrameBuffer) -> String {
    let rgba = frame_to_srgb8(frame);
    let bg = &rgba[..4];
    let moved = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| {
            pixel
                .iter()
                .zip(bg)
                .take(3)
                .any(|(component, background)| component.abs_diff(*background) > 1)
        })
        .count();
    let total = u64::from(frame.layout().width()) * u64::from(frame.layout().height());
    format!(
        "non-background {:.2}%  sha256 {}",
        100.0 * moved as f64 / total as f64,
        fmn_hash::sha256(frame.as_bytes()),
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

fn write_frame_png(
    dir: &std::path::Path,
    name: &str,
    frame: &FrameBuffer,
) -> Result<(), Box<dyn std::error::Error>> {
    if frame.layout().format() != PixelFormat::Rgba16F {
        return Err("G0-2 lighting renderer did not return Rgba16F".into());
    }
    let path = dir.join(name);
    let bytes = fmn_codec::png::encode_rgba8(
        frame.layout().width(),
        frame.layout().height(),
        &frame_to_srgb8(frame),
        fmn_codec::deflate::CompressionLevel::Default,
    );
    std::fs::write(&path, bytes)?;
    println!("      wrote {}", path.display());
    Ok(())
}

fn frame_to_srgb8(frame: &FrameBuffer) -> Vec<u8> {
    let tables = fmn_frame::transfer::tables();
    let mut rgba = Vec::with_capacity(frame.as_bytes().len() / 2);
    for pixel in frame.as_bytes().as_chunks::<8>().0 {
        for component in 0..3 {
            let bits = u16::from_le_bytes([pixel[component * 2], pixel[component * 2 + 1]]);
            rgba.push(tables.srgb8_from_f16(bits));
        }
        let alpha = u16::from_le_bytes([pixel[6], pixel[7]]);
        rgba.push(tables.linear8_from_f16(alpha));
    }
    rgba
}
