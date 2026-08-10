#![forbid(unsafe_code)]

use std::io::{BufRead as _, Read as _, Write as _};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use fmn_cli::{BUILTIN_SCENE_SOURCE, PYTHON_SOURCE_PORTAL_MESSAGE};
use fmn_core::rng::RngRoot;
#[cfg(feature = "batch")]
use fmn_output::{ManifestMode, ProvenanceManifest};
use fmn_scene::export_timeline_bundle;
use fmn_scene::studio_bridge::{Stage, Timeline};

static RUN_SUFFIX: AtomicU64 = AtomicU64::new(0);

fn run_clean(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fmn"))
        .args(args)
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .env("PATH", "")
        .output()
        .expect("launch the standalone fmn binary by absolute path")
}

fn output_root(label: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "fmn-cli-{label}-{}-{}",
        std::process::id(),
        RUN_SUFFIX.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&root).expect("create fresh CLI output root");
    root
}

fn compiled_wait_bundle(root: &std::path::Path) -> std::path::PathBuf {
    let path = root.join("compiled_wait.fmtl");
    let mut timeline = Timeline::new(8).expect("valid bundle fps");
    timeline.wait(0.25).expect("valid bundle wait");
    let bytes = export_timeline_bundle(timeline, &mut Stage::new(), &RngRoot::from_seed(0))
        .expect("compile wait artifact");
    std::fs::write(&path, bytes).expect("write compiled artifact");
    path
}

fn json_string_field<'a>(record: &'a str, field: &str) -> Option<&'a str> {
    let prefix = format!("\"{field}\":\"");
    record
        .split_once(&prefix)
        .and_then(|(_, tail)| tail.split_once('"'))
        .map(|(value, _)| value)
}

#[test]
fn standalone_binary_starts_without_a_python_runtime_on_path() {
    let output = run_clean(&["--version"]);

    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty());
    assert!(
        String::from_utf8(output.stdout)
            .expect("version output is UTF-8")
            .starts_with("fmn ")
    );
}

#[test]
fn python_source_is_refused_before_file_or_process_access() {
    for args in [
        &["--robot", "missing-scene.py", "Demo"][..],
        &["--robot", "studio", "missing-scene.PYW", "Demo"],
        &["--robot", "batch", "missing-scene.py", "Demo"],
    ] {
        let output = run_clean(args);
        let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

        assert_eq!(output.status.code(), Some(4), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        assert_eq!(stdout.lines().count(), 1, "{args:?}");
        assert!(stdout.contains("\"exit_name\":\"capability\""), "{args:?}");
        assert!(stdout.contains(PYTHON_SOURCE_PORTAL_MESSAGE), "{args:?}");
        assert!(!stdout.contains("composition is unavailable"), "{args:?}");
    }
}

#[cfg(unix)]
#[test]
fn studio_serves_a_real_worker_frame_and_shuts_down_on_stdin_eof() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fmn"))
        .args([
            "studio",
            "--robot",
            "--no-browser",
            "--resolution",
            "96x54",
            "--fps",
            "8",
            "--threads",
            "1",
            BUILTIN_SCENE_SOURCE,
            "circle_shift.v1",
        ])
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONPATH")
        .env("PATH", "")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("launch Studio front door");
    let mut stdout = std::io::BufReader::new(child.stdout.take().expect("Studio stdout"));
    let mut ready = String::new();
    stdout
        .read_line(&mut ready)
        .expect("read Studio readiness record");
    assert!(ready.contains("\"kind\":\"studio_ready\""), "{ready}");
    assert!(ready.contains("\"worker_generation\":1"), "{ready}");
    let url = json_string_field(&ready, "url").expect("readiness URL");
    let location = url.strip_prefix("http://").expect("loopback HTTP URL");
    let (authority, target) = location.split_once('/').expect("URL authority and target");
    let query = target.split_once('?').expect("capability query").1;

    let body = b"frame=1&commit=false";
    let mut scrub = std::net::TcpStream::connect(authority).expect("connect to Studio scrub API");
    scrub
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("scrub timeout");
    write!(
        scrub,
        "POST /api/scrub?{query} HTTP/1.1\r\nHost: {authority}\r\nOrigin: http://{authority}\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("write scrub request");
    scrub.write_all(body).expect("write scrub body");
    let mut scrub_response = Vec::new();
    scrub
        .read_to_end(&mut scrub_response)
        .expect("read scrub response");
    let _ = scrub.shutdown(std::net::Shutdown::Both);
    let scrub_response = String::from_utf8(scrub_response).expect("scrub response is UTF-8");
    assert!(
        scrub_response.starts_with("HTTP/1.1 200 OK"),
        "{scrub_response}"
    );
    assert!(
        scrub_response.contains("\"frame_index\":1"),
        "{scrub_response}"
    );

    let mut stream = std::net::TcpStream::connect(authority).expect("connect to Studio stream");
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(10)))
        .expect("stream timeout");
    write!(
        stream,
        "GET /stream?{query} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    )
    .expect("write stream request");
    let mut response = Vec::new();
    let mut chunk = [0_u8; 4096];
    let contains_frame_one_png = |bytes: &[u8]| {
        bytes
            .windows(20)
            .position(|window| window == b"X-FMN-Frame-Index: 1")
            .is_some_and(|header| {
                bytes[header..]
                    .windows(8)
                    .any(|window| window == b"\x89PNG\r\n\x1a\n")
            })
    };
    while !contains_frame_one_png(&response) {
        let read = stream
            .read(&mut chunk)
            .expect("read Studio multipart stream");
        assert_ne!(read, 0, "Studio stream closed before frame one's PNG");
        response.extend_from_slice(&chunk[..read]);
    }
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
    assert!(
        response
            .windows(20)
            .any(|window| window == b"X-FMN-Frame-Index: 1")
    );
    let _ = stream.shutdown(std::net::Shutdown::Both);
    drop(stream);
    drop(stdout);
    drop(child.stdin.take());
    let status = child.wait().expect("wait for graceful Studio shutdown");
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("Studio stderr")
        .read_to_string(&mut stderr)
        .expect("read Studio stderr");
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
}

#[test]
fn built_in_corpus_renders_through_the_real_png_and_y4m_sinks() {
    for format in ["png_sequence", "y4m"] {
        let root = output_root(format);
        let root_text = root.to_str().expect("output path is UTF-8");
        let output = run_clean(&[
            "--robot",
            "--write_all",
            "--format",
            format,
            "--resolution",
            "96x54",
            "--fps",
            "8",
            "--threads",
            "1",
            "--video_dir",
            root_text,
            BUILTIN_SCENE_SOURCE,
        ]);
        let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

        assert_eq!(output.status.code(), Some(0), "{format}: {stdout}");
        assert!(output.stderr.is_empty(), "{format}");
        assert_eq!(stdout.lines().count(), 25, "{format}");
        assert!(
            stdout.lines().all(|line| {
                line.contains("\"kind\":\"render\"")
                    && line.contains(&format!("\"format\":\"{format}\""))
                    && line.contains("\"frames\":3")
                    && line.contains("\"engine\":\"fast-cpu")
            }),
            "{format}: {stdout}"
        );

        let entries = std::fs::read_dir(&root)
            .expect("list published corpus root")
            .collect::<Result<Vec<_>, _>>()
            .expect("read published corpus entries");
        assert_eq!(entries.len(), 25, "{format}");
        if format == "png_sequence" {
            let sequence = root.join("circle_shift.v1");
            assert!(sequence.join("FMN_COMPLETE").is_file());
            let frame =
                std::fs::read(sequence.join("frame_000000.png")).expect("read first canonical PNG");
            assert_eq!(&frame[..8], b"\x89PNG\r\n\x1a\n");
        } else {
            let artifact =
                std::fs::read(root.join("circle_shift.y4m")).expect("read native y4m artifact");
            assert!(artifact.starts_with(b"YUV4MPEG2 W96 H54 F8:1"));
            assert!(artifact.windows(6).any(|window| window == b"FRAME\n"));
        }
    }
}

#[cfg(feature = "batch")]
#[test]
fn batch_renders_multiple_scenes_and_reports_in_request_order() {
    let root = output_root("batch-positive");
    let output = run_clean(&[
        "batch",
        "--robot",
        "--format",
        "png_sequence",
        "--resolution",
        "96x54",
        "--fps",
        "8",
        "--threads",
        "1",
        "--max-scenes",
        "2",
        "--video_dir",
        root.to_str().expect("output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
        "rectangle_shift.v1",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(lines.len(), 3, "{stdout}");
    assert!(lines[0].contains("\"kind\":\"render\""));
    assert!(lines[0].contains("\"scene\":\"circle_shift.v1\""));
    assert!(lines[1].contains("\"kind\":\"render\""));
    assert!(lines[1].contains("\"scene\":\"rectangle_shift.v1\""));
    assert!(lines[2].contains("\"kind\":\"batch\""));
    assert!(lines[2].contains("\"status\":\"ok\""));
    assert!(lines[2].contains("\"succeeded\":2"));
    assert!(root.join("circle_shift.v1/FMN_COMPLETE").is_file());
    assert!(root.join("rectangle_shift.v1/FMN_COMPLETE").is_file());
}

#[cfg(feature = "batch")]
#[test]
fn batch_budget_cancels_before_publication() {
    let budget_root = output_root("batch-budget");
    let budget = run_clean(&[
        "batch",
        "--robot",
        "--budget-ms",
        "0",
        "--max-scenes",
        "2",
        "--format",
        "png_sequence",
        "--resolution",
        "96x54",
        "--video_dir",
        budget_root.to_str().expect("output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
        "rectangle_shift.v1",
    ]);
    let budget_stdout = String::from_utf8(budget.stdout).expect("robot output is UTF-8");

    assert_eq!(budget.status.code(), Some(8), "{budget_stdout}");
    assert!(budget.stderr.is_empty());
    assert_eq!(budget_stdout.lines().count(), 3, "{budget_stdout}");
    assert_eq!(budget_stdout.matches("\"status\":\"cancelled\"").count(), 2);
    assert!(budget_stdout.contains("\"cancelled\":2"));
    assert_eq!(
        std::fs::read_dir(&budget_root)
            .expect("list unpublished budget root")
            .count(),
        0
    );
}

#[cfg(feature = "batch")]
#[test]
fn batch_publishes_complete_fmnp_manifests_and_preflights_no_clobber() {
    fn render_with_manifests(
        label: &str,
        threads: &str,
        format: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf, String) {
        let artifact_root = output_root(&format!("{label}-artifacts"));
        let manifest_root = output_root(&format!("{label}-manifests"));
        let output = run_clean(&[
            "batch",
            "--robot",
            "--reproducible",
            "--manifest-dir",
            manifest_root.to_str().expect("manifest path is UTF-8"),
            "--format",
            format,
            "--resolution",
            "96x54",
            "--fps",
            "8",
            "--threads",
            threads,
            "--max-scenes",
            "2",
            "--video_dir",
            artifact_root.to_str().expect("output path is UTF-8"),
            BUILTIN_SCENE_SOURCE,
            "circle_shift.v1",
            "rectangle_shift.v1",
        ]);
        let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");
        assert_eq!(output.status.code(), Some(0), "{stdout}");
        assert!(output.stderr.is_empty());
        assert_eq!(stdout.lines().count(), 3, "{stdout}");
        (artifact_root, manifest_root, stdout)
    }

    let (artifact_root, manifest_root, stdout) =
        render_with_manifests("fmnp-one", "1", "png_sequence");
    let mut first_closure = None;
    let mut first_outputs = Vec::new();
    for (index, scene) in ["circle_shift.v1", "rectangle_shift.v1"]
        .into_iter()
        .enumerate()
    {
        let record = stdout.lines().nth(index).expect("ordered render record");
        assert!(record.contains(&format!("\"scene\":\"{scene}\"")));
        assert!(record.contains("\"manifest\":{"));
        let generation = manifest_root.join(scene);
        assert!(generation.join("FMN_COMPLETE").is_file());
        let bytes = std::fs::read(generation.join("manifest.fmnp")).expect("read FMNP");
        let manifest = ProvenanceManifest::from_bytes(&bytes).expect("verify FMNP");
        assert_eq!(manifest.mode, ManifestMode::Certified);
        assert!((1..=10).all(|id| manifest.items.iter().any(|item| item.item_id == id)));
        assert_eq!(manifest.outputs.len(), 1);
        assert!(manifest.outputs[0].certified);
        assert_eq!(
            manifest.outputs[0].digest.to_hex(),
            json_string_field(record, "artifact_digest").expect("artifact digest robot field")
        );
        assert_eq!(
            manifest.closure_digest.to_hex(),
            json_string_field(record, "closure_digest").expect("closure digest robot field")
        );
        let text = std::fs::read_to_string(generation.join("manifest.txt"))
            .expect("read human FMNP rendering");
        for id in 1..=10 {
            assert!(text.contains(&format!("id = {id}")));
        }
        if index == 0 {
            first_closure = Some(manifest.closure_digest);
        }
        first_outputs.push(manifest.outputs[0].digest);
        assert!(artifact_root.join(scene).join("FMN_COMPLETE").is_file());
    }

    let (_four_artifacts, four_manifests, four_stdout) =
        render_with_manifests("fmnp-four", "4", "png_sequence");
    let four_manifest = ProvenanceManifest::from_bytes(
        &std::fs::read(four_manifests.join("circle_shift.v1/manifest.fmnp"))
            .expect("read four-thread FMNP"),
    )
    .expect("verify four-thread FMNP");
    assert_eq!(first_closure, Some(four_manifest.closure_digest));
    assert_eq!(first_outputs[0], four_manifest.outputs[0].digest);
    assert!(four_stdout.contains("\"render_threads\":4"));

    let (_still_artifacts, still_manifests, _still_stdout) =
        render_with_manifests("fmnp-still", "1", "png");
    let still_manifest = ProvenanceManifest::from_bytes(
        &std::fs::read(still_manifests.join("circle_shift.v1/manifest.fmnp"))
            .expect("read final-state PNG FMNP"),
    )
    .expect("verify final-state PNG FMNP");
    assert_ne!(first_closure, Some(still_manifest.closure_digest));
    assert_eq!(still_manifest.outputs[0].kind, "canonical_png");

    let blocked_output_root = output_root("fmnp-preflight-blocked");
    let blocked = run_clean(&[
        "batch",
        "--robot",
        "--manifest-dir",
        manifest_root.to_str().expect("manifest path is UTF-8"),
        "--format",
        "png_sequence",
        "--video_dir",
        blocked_output_root
            .to_str()
            .expect("blocked output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
    ]);
    let blocked_stdout = String::from_utf8(blocked.stdout).expect("robot output is UTF-8");

    assert_eq!(blocked.status.code(), Some(70), "{blocked_stdout}");
    assert!(blocked.stderr.is_empty());
    assert!(blocked_stdout.contains("already exists"));
    assert_eq!(
        std::fs::read_dir(blocked_output_root)
            .expect("list output root after preflight refusal")
            .count(),
        0
    );
}

#[cfg(feature = "batch")]
#[test]
fn batch_fail_fast_cancels_jobs_after_the_first_publication_failure() {
    let root = output_root("batch-fail-fast");
    std::fs::create_dir(root.join("circle_shift.v1"))
        .expect("reserve the first scene destination to force no-clobber failure");
    let output = run_clean(&[
        "batch",
        "--robot",
        "--fail-fast",
        "--max-scenes",
        "1",
        "--format",
        "png_sequence",
        "--resolution",
        "96x54",
        "--threads",
        "1",
        "--video_dir",
        root.to_str().expect("output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
        "rectangle_shift.v1",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();

    assert_eq!(output.status.code(), Some(6), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(lines.len(), 3, "{stdout}");
    assert!(lines[0].contains("\"scene\":\"circle_shift.v1\""));
    assert!(lines[0].contains("\"status\":\"failed\""));
    assert!(lines[1].contains("\"scene\":\"rectangle_shift.v1\""));
    assert!(lines[1].contains("\"status\":\"cancelled\""));
    assert!(lines[1].contains("\"reason\":\"fail_fast\""));
    assert!(lines[2].contains("\"failed\":1"));
    assert!(lines[2].contains("\"cancelled\":1"));
    assert!(!root.join("rectangle_shift.v1").exists());
}

#[test]
fn native_png_publishes_the_final_frame_and_gif_streams_the_schedule() {
    let root = output_root("native-png-gif");
    let sequence_root = root.join("sequence");
    let still_root = root.join("still");
    let gif_root = root.join("gif");
    for directory in [&sequence_root, &still_root, &gif_root] {
        std::fs::create_dir(directory).expect("create native-format output root");
    }

    let render = |format: &str, output: &std::path::Path| {
        run_clean(&[
            "--robot",
            "--format",
            format,
            "--resolution",
            "96x54",
            "--fps",
            "8",
            "--threads",
            "1",
            "--video_dir",
            output.to_str().expect("output path is UTF-8"),
            BUILTIN_SCENE_SOURCE,
            "circle_shift.v1",
        ])
    };

    let sequence = render("png_sequence", &sequence_root);
    assert_eq!(sequence.status.code(), Some(0));
    assert!(sequence.stderr.is_empty());

    let still = render("png", &still_root);
    let still_stdout = String::from_utf8(still.stdout).expect("robot output is UTF-8");
    assert_eq!(still.status.code(), Some(0), "{still_stdout}");
    assert!(still.stderr.is_empty());
    assert!(still_stdout.contains("\"format\":\"png\""));
    assert!(still_stdout.contains("\"frames\":1"));
    let still_bytes =
        std::fs::read(still_root.join("circle_shift.png")).expect("read final-state PNG");
    let final_sequence_frame =
        std::fs::read(sequence_root.join("circle_shift.v1/frame_000002.png"))
            .expect("read final PNG-sequence frame");
    assert_eq!(still_bytes, final_sequence_frame);

    let gif = render("gif", &gif_root);
    let gif_stdout = String::from_utf8(gif.stdout).expect("robot output is UTF-8");
    assert_eq!(gif.status.code(), Some(0), "{gif_stdout}");
    assert!(gif.stderr.is_empty());
    assert!(gif_stdout.contains("\"format\":\"gif\""));
    assert!(gif_stdout.contains("\"frames\":3"));
    let gif_bytes = std::fs::read(gif_root.join("circle_shift.gif")).expect("read native GIF");
    assert!(gif_bytes.starts_with(b"GIF89a"));
    assert_eq!(gif_bytes.last(), Some(&b';'));
}

#[test]
fn certified_gif_is_refused_before_publication() {
    let root = output_root("certified-gif-refusal");
    let output = run_clean(&[
        "--robot",
        "--reproducible",
        "--format",
        "gif",
        "--video_dir",
        root.to_str().expect("output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(output.stderr.is_empty());
    assert!(stdout.contains("\"exit_name\":\"capability\""));
    assert!(stdout.contains("GIF is outside the certified artifact set"));
    assert_eq!(
        std::fs::read_dir(root)
            .expect("list untouched output root")
            .count(),
        0
    );
}

#[test]
fn skip_mode_publishes_one_final_frame_to_a_streaming_native_sink() {
    let root = output_root("skip-final-gif");
    let output = run_clean(&[
        "--robot",
        "--skip_animations",
        "--format",
        "gif",
        "--resolution",
        "96x54",
        "--fps",
        "8",
        "--threads",
        "1",
        "--video_dir",
        root.to_str().expect("output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert!(stdout.contains("\"format\":\"gif\""));
    assert!(stdout.contains("\"frames\":1"));
    let gif = std::fs::read(root.join("circle_shift.gif")).expect("read final-state GIF");
    assert!(gif.starts_with(b"GIF89a"));
    assert_eq!(gif.last(), Some(&b';'));
}

#[test]
fn compiled_fmtl_renders_through_the_standalone_binary() {
    let root = output_root("compiled-fmtl");
    let source = compiled_wait_bundle(&root);
    let output_dir = root.join("output");
    std::fs::create_dir(&output_dir).expect("create compiled output root");
    let output = run_clean(&[
        "--robot",
        "--format",
        "png_sequence",
        "--resolution",
        "96x54",
        "--threads",
        "1",
        "--video_dir",
        output_dir.to_str().expect("output path is UTF-8"),
        source.to_str().expect("source path is UTF-8"),
        "CompiledWait",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(output.stderr.is_empty());
    assert_eq!(stdout.lines().count(), 1);
    assert!(stdout.contains("\"source\":\"compiled\""));
    assert!(stdout.contains("\"source_artifact\":"));
    assert!(stdout.contains("\"scene\":\"CompiledWait\""));
    assert!(stdout.contains("\"frames\":2"));
    let sequence = output_dir.join("CompiledWait");
    assert!(sequence.join("FMN_COMPLETE").is_file());
    let pngs = std::fs::read_dir(&sequence)
        .expect("list compiled output")
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "png"))
        .count();
    assert_eq!(pngs, 2);
    let frame = std::fs::read(sequence.join("frame_000000.png")).expect("read compiled PNG");
    assert!(frame.starts_with(b"\x89PNG\r\n\x1a\n"));
}

#[test]
fn compiled_fmtl_refuses_schedule_changes_and_malformed_input_before_publication() {
    let root = output_root("compiled-refusals");
    let source = compiled_wait_bundle(&root);
    let output_dir = root.join("output");
    std::fs::create_dir(&output_dir).expect("create refusal output root");

    let fps = run_clean(&[
        "--robot",
        "--format",
        "png_sequence",
        "--fps",
        "9",
        "--video_dir",
        output_dir.to_str().expect("output path is UTF-8"),
        source.to_str().expect("source path is UTF-8"),
    ]);
    let fps_stdout = String::from_utf8(fps.stdout).expect("robot output is UTF-8");
    assert_eq!(fps.status.code(), Some(3), "{fps_stdout}");
    assert!(fps.stderr.is_empty());
    assert!(fps_stdout.contains("fixed 8 fps schedule"));
    assert_eq!(
        std::fs::read_dir(&output_dir).expect("list output").count(),
        0
    );

    let malformed = root.join("malformed.fmtl");
    std::fs::write(&malformed, b"not an FMTL document").expect("write malformed artifact");
    let bad = run_clean(&[
        "--robot",
        "--format",
        "png_sequence",
        "--video_dir",
        output_dir.to_str().expect("output path is UTF-8"),
        malformed.to_str().expect("source path is UTF-8"),
    ]);
    let bad_stdout = String::from_utf8(bad.stdout).expect("robot output is UTF-8");
    assert_eq!(bad.status.code(), Some(5), "{bad_stdout}");
    assert!(bad.stderr.is_empty());
    assert!(bad_stdout.contains("malformed FMTL/1 container"));
    assert_eq!(
        std::fs::read_dir(output_dir).expect("list output").count(),
        0
    );
}

#[test]
fn reproducible_cli_render_is_byte_identical_across_fresh_publications() {
    let mut outputs = Vec::new();
    for label in ["certified-a", "certified-b"] {
        let root = output_root(label);
        let output = run_clean(&[
            "--robot",
            "--reproducible",
            "--format",
            "png_sequence",
            "--resolution",
            "96x54",
            "--fps",
            "8",
            "--threads",
            "1",
            "--video_dir",
            root.to_str().expect("output path is UTF-8"),
            BUILTIN_SCENE_SOURCE,
            "circle_shift.v1",
        ]);
        let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");
        assert_eq!(output.status.code(), Some(0), "{stdout}");
        assert!(output.stderr.is_empty());
        assert!(stdout.contains("\"engine\":\"certified-cpu:scalar:"));
        let sequence = root.join("circle_shift.v1");
        outputs.push(
            (0..3)
                .map(|frame| {
                    std::fs::read(sequence.join(format!("frame_{frame:06}.png")))
                        .expect("read certified PNG")
                })
                .collect::<Vec<_>>(),
        );
    }
    assert_eq!(outputs[0], outputs[1]);
}

#[test]
fn unknown_built_in_scene_fails_before_publication() {
    let root = output_root("unknown-scene");
    let output = run_clean(&[
        "--robot",
        "--format",
        "png_sequence",
        "--video_dir",
        root.to_str().expect("output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "NotARealScene",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

    assert_eq!(output.status.code(), Some(5));
    assert!(output.stderr.is_empty());
    assert!(stdout.contains("\"exit_name\":\"scene\""));
    assert!(stdout.contains("unknown built-in scene"));
    assert!(!stdout.contains(PYTHON_SOURCE_PORTAL_MESSAGE));
    assert_eq!(
        std::fs::read_dir(root)
            .expect("list untouched output root")
            .count(),
        0
    );
}

#[test]
fn video_render_names_the_missing_optional_tool_and_native_alternative() {
    let root = output_root("missing-ffmpeg");
    let output = run_clean(&[
        "--robot",
        "--format",
        "video",
        "--resolution",
        "96x54",
        "--fps",
        "8",
        "--video_dir",
        root.to_str().expect("output path is UTF-8"),
        BUILTIN_SCENE_SOURCE,
        "circle_shift.v1",
    ]);
    let stdout = String::from_utf8(output.stdout).expect("robot output is UTF-8");

    assert_eq!(output.status.code(), Some(4), "{stdout}");
    assert!(output.stderr.is_empty());
    assert!(stdout.contains("\"exit_name\":\"capability\""));
    assert!(stdout.contains("ffmpeg is unavailable"));
    assert!(stdout.contains("native outputs need no ffmpeg"));
    assert!(stdout.contains("y4m, PNG sequences, and GIF"));
    assert_eq!(
        std::fs::read_dir(root)
            .expect("list untouched output root")
            .count(),
        0
    );
}
