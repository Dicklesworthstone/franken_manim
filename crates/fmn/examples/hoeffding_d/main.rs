//! Hoeffding's D — a 3Blue1Brown-style explainer in eight chapters, rendered
//! end to end by franken_manim's native Rust front door (no Python, no
//! LaTeX) with a procedurally synthesized score mixed natively, and encoded
//! through its one sandboxed external tool, ffmpeg.
//!
//! ```text
//! cargo run --release -p fmn --example hoeffding_d -- <command>
//!
//! stats                      # the numbers every chapter shows
//! glyphs OUT                 # typesetting probe sheet (PNG sequence)
//! script                     # narration lines as TSV (id, text)
//! render all|<chapter>... [--res 1920x1080] [--fps 60] [--out DIR]
//!        [--format mp4|png|y4m] [--narration DIR] [--silent] [--threads N]
//!        [--encoder libx264|libx265|h264_nvenc|hevc_nvenc|h264_videotoolbox]
//!        [--crf N] [--preset P] [--bitrate BPS] [--max-frames N]
//! ```
//!
//! MP4 needs ffmpeg on `PATH`; PNG sequences and Y4M are fully native. The
//! narration directory holds one `<id>.wav` per `script` line; without it the
//! chapters keep their visual-only pacing.

mod chapters;
mod kit;
mod narration;
mod sound;
mod stats;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use fmn::platform::process::{StdFfmpegLocator, StdProcessRunner};
use fmn::prelude::*;
use fmn::rendering::{EncoderPreset, EncoderTune, VideoQuality};

use crate::kit::{BACKGROUND, Kit};

const USAGE: &str = "usage: hoeffding_d stats | script | glyphs OUT | render all|CHAPTER... [--res WxH] [--fps N] [--out DIR] [--format mp4|png|y4m]";

struct Args {
    command: String,
    targets: Vec<String>,
    res: (u32, u32),
    fps: u32,
    out: PathBuf,
    format: RenderFormat,
    silent: bool,
    narration: Option<PathBuf>,
    threads: Option<u32>,
    encoder: String,
    quality: VideoQuality,
    max_frames: Option<u64>,
}

fn value<T: std::str::FromStr>(
    it: &mut impl Iterator<Item = String>,
    flag: &str,
) -> Result<T, String> {
    it.next()
        .ok_or_else(|| format!("{flag} needs a value"))?
        .parse()
        .map_err(|_| format!("bad {flag} value"))
}

fn parse_args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let command = it.next().ok_or(USAGE)?;
    let mut args = Args {
        command,
        targets: Vec::new(),
        res: (1920, 1080),
        fps: 60,
        out: PathBuf::from("renders"),
        format: RenderFormat::Mp4,
        silent: false,
        narration: None,
        threads: None,
        encoder: "libx264".to_owned(),
        quality: VideoQuality::default(),
        max_frames: None,
    };
    while let Some(a) = it.next() {
        match a.as_str() {
            "--res" => {
                let r: String = value(&mut it, "--res")?;
                let (w, h) = r.split_once('x').ok_or("--res needs WxH")?;
                args.res = (
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                );
            }
            "--fps" => args.fps = value(&mut it, "--fps")?,
            "--out" => args.out = value(&mut it, "--out")?,
            "--format" => {
                args.format = match value::<String>(&mut it, "--format")?.as_str() {
                    "mp4" => RenderFormat::Mp4,
                    "png" => RenderFormat::PngSequence,
                    "y4m" => RenderFormat::Y4m,
                    _ => return Err("--format is mp4, png or y4m".into()),
                }
            }
            "--silent" => args.silent = true,
            "--narration" => args.narration = Some(value(&mut it, "--narration")?),
            // The encoder/quality pair is admitted (or refused) by the
            // facade before any frame is drawn.
            "--encoder" => args.encoder = value(&mut it, "--encoder")?,
            "--crf" => args.quality.crf = Some(value(&mut it, "--crf")?),
            "--bitrate" => args.quality.bitrate = Some(value(&mut it, "--bitrate")?),
            "--preset" => {
                let preset: String = value(&mut it, "--preset")?;
                args.quality.preset = Some(
                    preset
                        .parse::<EncoderPreset>()
                        .map_err(|_| "unknown x264/x265 preset")?,
                );
            }
            "--max-frames" => args.max_frames = Some(value(&mut it, "--max-frames")?),
            "--threads" => args.threads = Some(value(&mut it, "--threads")?),
            other => args.targets.push(other.to_owned()),
        }
    }
    // Rate control on a software encoder: tune it for flat-shaded animation.
    if (args.quality.crf.is_some() || args.quality.preset.is_some()) && args.encoder == "libx264" {
        args.quality.tune = Some(EncoderTune::Animation);
    }
    Ok(args)
}

fn options(
    path: PathBuf,
    format: RenderFormat,
    res: (u32, u32),
    fps: u32,
) -> Result<RenderOptions, Box<dyn std::error::Error>> {
    let mut options = RenderOptions::with_format(path, format)?;
    options.config.camera.resolution = res;
    options.config.camera.fps = fps;
    options.config.camera.background_color = BACKGROUND.to_owned();
    if matches!(format, RenderFormat::Mp4) {
        // The host's ffmpeg, under the boundary's process protocol. The
        // facade's `ffmpeg` feature installs the same capability itself.
        options.ffmpeg = Some(FfmpegCapability {
            runner: Arc::new(StdProcessRunner),
            locator: Arc::new(StdFfmpegLocator::from_host_path()),
            workdir_root: std::env::temp_dir(),
        });
    }
    Ok(options)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    stats::self_check();
    let args = parse_args()?;
    match args.command.as_str() {
        "stats" => print_stats(),
        "script" => {
            for (id, text) in narration::SCRIPT {
                println!("{id}\t{text}");
            }
        }
        "glyphs" => {
            let out = args
                .targets
                .first()
                .cloned()
                .unwrap_or_else(|| "glyphs".into());
            let mut scene = chapters::glyphs::Glyphs { kit: Kit::new()? };
            let report = render(
                &mut scene,
                options(out.into(), RenderFormat::PngSequence, args.res, 1)?,
            )?;
            println!("glyph sheet -> {}", report.artifact.path.display());
        }
        "render" => render_chapters(&args)?,
        other => return Err(format!("unknown command {other}\n{USAGE}").into()),
    }
    Ok(())
}

fn render_chapters(args: &Args) -> Result<(), Box<dyn std::error::Error>> {
    let all = chapters::registry();
    let wanted: Vec<&chapters::Entry> =
        if args.targets.iter().any(|t| t == "all") || args.targets.is_empty() {
            all.iter().collect()
        } else {
            args.targets
                .iter()
                .map(|t| {
                    all.iter()
                        .find(|e| e.key == t)
                        .ok_or_else(|| format!("unknown chapter {t}"))
                })
                .collect::<Result<_, _>>()?
        };
    std::fs::create_dir_all(&args.out)?;
    let audio = args.out.join("audio");
    std::fs::create_dir_all(&audio)?;
    let narrator = match &args.narration {
        Some(dir) => {
            let n = narration::Narrator::load(dir)?;
            println!(
                "narration: {} lines, {:.1}s of speech from {}",
                narration::SCRIPT.len(),
                n.total_seconds(),
                dir.display()
            );
            Some(n)
        }
        None => None,
    };
    let extension = match args.format {
        RenderFormat::Mp4 => ".mp4",
        RenderFormat::Y4m => ".y4m",
        _ => "",
    };
    let total = Instant::now();
    let mut frames = 0;
    for entry in wanted {
        let path = args.out.join(format!("{}{extension}", entry.key));
        let started = Instant::now();
        // macOS: a transient Darwin killpg EPERM after ffmpeg exits fails the
        // whole render (fm-darwin-killpg-eperm-race-7aae). Publication is
        // atomic, so nothing partial exists; render the chapter again.
        let mut attempt = 1;
        let report = loop {
            let mut kit = Kit::new()?;
            if !args.silent {
                kit.score = Some(sound::Score {
                    dir: audio.clone(),
                    under_voice: narrator.is_some(),
                });
                if let Some(n) = &narrator {
                    n.reset();
                }
                kit.narrator = narrator.clone();
            }
            let mut scene = (entry.make)(kit);
            let mut opts = options(path.clone(), args.format, args.res, args.fps)?;
            if let Some(n) = args.threads {
                opts.config.render.threads = fmn::config::config::ThreadPolicy::Fixed(n);
            }
            if matches!(args.format, RenderFormat::Mp4) {
                opts.config.file_writer.video_codec = args.encoder.clone();
                opts.video_quality = args.quality;
            }
            if let Some(n) = args.max_frames {
                // Profiling runs: stop after n frames (the render then fails
                // by design and publishes nothing).
                opts.max_frames = n;
            }
            match render(scene.as_mut(), opts) {
                Ok(report) => break report,
                Err(error)
                    if attempt < 3
                        && format!("{error:?}").contains(
                            "process-tree completion kill failed: Operation not permitted",
                        ) =>
                {
                    eprintln!(
                        "{}: transient Darwin EPERM after ffmpeg exit (attempt {attempt}); rendering again",
                        entry.key
                    );
                    attempt += 1;
                }
                Err(error) => return Err(error.into()),
            }
        };
        let secs = started.elapsed().as_secs_f64();
        frames += report.artifact.frame_count;
        println!(
            "{:<14} {:>5} frames  {:>6.1}s  {:>6.1} fps  -> {}{}",
            entry.key,
            report.artifact.frame_count,
            secs,
            report.artifact.frame_count as f64 / secs,
            report.artifact.path.display(),
            if attempt > 1 {
                format!("  (attempt {attempt})")
            } else {
                String::new()
            }
        );
    }
    let secs = total.elapsed().as_secs_f64();
    println!(
        "total          {frames:>5} frames  {secs:>6.1}s  {:>6.1} fps",
        frames as f64 / secs
    );
    Ok(())
}

fn print_stats() {
    use stats::Shape::*;
    for shape in [Line, Parabola, Ring, Cross, Wave, Noise] {
        let pts = stats::shape_points(shape, chapters::gallery::N, chapters::gallery::seed(shape));
        let m = stats::measures(&pts);
        let q = stats::null_q99(&pts, chapters::gallery::TRIALS, 99);
        println!(
            "{:10} r={:+.3} (q99 {:.3})  rho={:+.3} (q99 {:.3})  tau={:+.3} (q99 {:.3})  D={:+.4} (q99 {:.4})",
            shape.label(),
            m.pearson,
            q.pearson,
            m.spearman,
            q.spearman,
            m.kendall,
            q.kendall,
            m.hoeffding,
            q.hoeffding
        );
    }
    let h = stats::hoeffding(&stats::HEIGHTS, &stats::WEIGHTS);
    println!(
        "worked example: R={:?} S={:?} Q={:?} D1={} D2={} D3={} D={}",
        h.r, h.s, h.q, h.d1, h.d2, h.d3, h.d
    );
    println!("C(5000,4) = {}", stats::choose(5000, 4));
}
