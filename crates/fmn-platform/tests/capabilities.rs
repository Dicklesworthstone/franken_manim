//! Capability trait-suite tests (fm-x68 acceptance): the std implementations
//! against the real host, and the test doubles' contracts.
//!
//! The process tests exercise the D2 mechanism substrate for real: argv-only
//! spawning, the cleared-environment allowlist, timeout kill, output-cap
//! kill, and stdin plumbing — all against coreutils, Unix-only (`cfg`).

use fmn_platform::fs::{ATOMIC_DIRECTORY_COMPLETE_LEAF, FileSystem, FsNodeKind, StdFs, VirtualFs};
use fmn_platform::process::{
    ProcessCancellation, ProcessOutcome, ProcessRunner, ProcessSpec, ProcessStdinLimits,
    ProcessTermination, ScriptedRunner,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("caps_{name}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn std_fs_atomic_write_round_trips_and_lists_sorted() {
    let dir = scratch("stdfs");
    let fs = StdFs;
    let deep = dir.join("a/b/file.bin");
    fs.write_atomic(&deep, b"payload")
        .expect("atomic write creates parents");
    assert_eq!(fs.read(&deep).expect("read"), b"payload");
    fs.write_atomic(&deep, b"replaced").expect("atomic replace");
    assert_eq!(fs.read_to_string(&deep).expect("read"), "replaced");
    assert_eq!(
        fs.node_kind_no_follow(&deep).expect("classify"),
        Some(FsNodeKind::RegularFile)
    );
    fs.write_atomic(&dir.join("a/z.txt"), b"z").expect("write");
    fs.write_atomic(&dir.join("a/a.txt"), b"a").expect("write");
    let listed = fs.list_dir(&dir.join("a")).expect("list");
    let names: Vec<String> = listed
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    assert_eq!(names, vec!["a.txt", "b", "z.txt"], "sorted listing");
    assert!(fs.exists(&deep));
    assert!(!fs.exists(&dir.join("missing")));
    assert_eq!(
        fs.node_kind_no_follow(&dir.join("missing"))
            .expect("classify absence"),
        None
    );
}

#[test]
fn std_fs_create_new_and_removal_lifecycle() {
    let dir = scratch("stdfs_lifecycle");
    let fs = StdFs;
    // CARGO_TARGET_TMPDIR persists across runs; start from a clean slate so
    // create_new's create-if-absent assertions are re-runnable.
    let _ = fs.remove_dir_all(&dir);
    std::fs::create_dir(&dir).expect("create lifecycle root");

    let empty = dir.join("empty");
    assert!(fs.create_dir(&empty).expect("create exact directory"));
    assert!(
        !fs.create_dir(&empty)
            .expect("existing exact directory is stable")
    );
    assert_eq!(
        fs.node_kind_no_follow(&empty).expect("classify directory"),
        Some(FsNodeKind::Directory)
    );

    // create_new: create-if-absent with full contents, atomically visible.
    let lock = dir.join("locks/maintenance.lock");
    assert!(fs.create_new(&lock, b"holder-1").expect("create"));
    assert!(
        !fs.create_new(&lock, b"holder-2")
            .expect("second create loses")
    );
    assert_eq!(fs.read(&lock).expect("read"), b"holder-1");
    // No temp residue from either attempt.
    let residue: Vec<_> = fs
        .list_dir(&dir.join("locks"))
        .expect("list")
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(".fmn-new."))
        })
        .collect();
    assert!(residue.is_empty(), "temp residue: {residue:?}");

    // remove_file: gone, and a second removal is a precise NotFound.
    fs.remove_file(&lock).expect("remove");
    assert!(!fs.exists(&lock));
    assert!(matches!(
        fs.remove_file(&lock),
        Err(fmn_platform::fs::FsError::NotFound { .. })
    ));

    // remove_dir_all: subtree gone, siblings untouched.
    fs.write_atomic(&dir.join("ns/a/objects/x"), b"x").unwrap();
    fs.write_atomic(&dir.join("ns/b/objects/y"), b"y").unwrap();
    fs.remove_dir_all(&dir.join("ns/a")).expect("purge");
    assert!(!fs.exists(&dir.join("ns/a")));
    assert_eq!(fs.read(&dir.join("ns/b/objects/y")).unwrap(), b"y");
}

#[test]
fn streaming_atomic_capabilities_separate_prepare_from_publication() {
    let fs = Arc::new(VirtualFs::new());
    let destination = PathBuf::from("/artifacts/frame.png");
    fs.insert(&destination, b"old".to_vec());

    let mut writer = fs
        .clone()
        .begin_atomic_file(&destination)
        .expect("begin private file");
    writer.write(b"new-").expect("first file chunk");
    writer.write(b"bytes").expect("second file chunk");
    let prepared = writer.prepare().expect("prepare private file");
    assert_eq!(fs.read(&destination).expect("old destination"), b"old");
    drop(prepared);
    assert_eq!(
        fs.read(&destination).expect("drop preserves destination"),
        b"old"
    );

    let mut writer = fs
        .clone()
        .begin_atomic_file(&destination)
        .expect("begin replacement file");
    writer.write(b"replacement").expect("replacement bytes");
    let prepared = writer.prepare().expect("prepare replacement");
    assert_eq!(fs.read(&destination).expect("still old"), b"old");
    prepared.commit().expect("commit replacement");
    assert_eq!(
        fs.read(&destination).expect("published replacement"),
        b"replacement"
    );
}

#[test]
fn std_streaming_atomic_file_and_directory_publish_only_at_commit() {
    let root = scratch("stdfs_streaming_atomic");
    let _ = StdFs.remove_dir_all(&root);
    std::fs::create_dir(&root).expect("fresh streaming root");
    let fs = Arc::new(StdFs);

    let file = root.join("artifact.bin");
    std::fs::write(&file, b"old").expect("old artifact");
    let mut writer = fs
        .clone()
        .begin_atomic_file(&file)
        .expect("begin host private file");
    writer.write(b"new").expect("host private bytes");
    let prepared = writer.prepare().expect("prepare host private file");
    assert_eq!(std::fs::read(&file).expect("still old"), b"old");
    prepared.commit().expect("publish host file");
    assert_eq!(std::fs::read(&file).expect("host replacement"), b"new");

    let generation = root.join("frames");
    let mut writer = fs
        .clone()
        .begin_atomic_directory(&generation)
        .expect("begin host generation");
    writer
        .write_file(Path::new("frame_0001.png"), b"one")
        .expect("first host child");
    writer
        .write_file(Path::new("frame_0002.png"), b"two")
        .expect("second host child");
    let prepared = writer.prepare().expect("prepare host generation");
    assert!(!generation.exists());
    prepared.commit().expect("publish host generation");
    assert_eq!(
        std::fs::read(generation.join(ATOMIC_DIRECTORY_COMPLETE_LEAF)).expect("completion marker"),
        b"fmn-atomic-directory-v1\n"
    );
    assert_eq!(
        std::fs::read(generation.join("frame_0001.png")).expect("first host child"),
        b"one"
    );
    assert_eq!(
        std::fs::read(generation.join("frame_0002.png")).expect("second host child"),
        b"two"
    );

    let mut contender = fs
        .clone()
        .begin_atomic_directory(&generation)
        .expect("begin host contender");
    contender
        .write_file(Path::new("frame_0001.png"), b"replacement")
        .expect("private host contender");
    assert!(
        contender
            .prepare()
            .expect("prepare host contender")
            .commit()
            .is_err(),
        "host generation must be no-clobber"
    );
    assert_eq!(
        std::fs::read(generation.join("frame_0001.png")).expect("stable host child"),
        b"one"
    );
}

#[test]
fn std_atomic_directory_concurrent_commit_is_no_clobber() {
    let root = scratch("stdfs_atomic_directory_race");
    let _ = StdFs.remove_dir_all(&root);
    std::fs::create_dir(&root).expect("fresh race root");
    let fs = Arc::new(StdFs);
    let destination = root.join("frames");

    let prepare = |bytes: &'static [u8]| {
        let mut writer = fs
            .clone()
            .begin_atomic_directory(&destination)
            .expect("begin contender");
        writer
            .write_file(Path::new("frame.png"), bytes)
            .expect("stage contender");
        writer.prepare().expect("prepare contender")
    };
    let first = prepare(b"first");
    let second = prepare(b"second");
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let spawn = |prepared: Box<dyn fmn_platform::fs::PreparedAtomicDirectory>| {
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            prepared.commit()
        })
    };
    let first = spawn(first);
    let second = spawn(second);
    barrier.wait();
    let first = first.join().expect("first contender thread");
    let second = second.join().expect("second contender thread");

    assert_ne!(
        first.is_ok(),
        second.is_ok(),
        "exactly one no-clobber commit must win"
    );
    assert!(destination.join(ATOMIC_DIRECTORY_COMPLETE_LEAF).exists());
    let frame = std::fs::read(destination.join("frame.png")).expect("winner frame");
    assert!(
        frame == b"first" || frame == b"second",
        "generation mixed unexpected bytes: {frame:?}"
    );
}

#[test]
fn atomic_directory_generation_is_complete_no_clobber_and_leaf_safe() {
    let fs = Arc::new(VirtualFs::new());
    let destination = PathBuf::from("/frames");
    let mut writer = fs
        .clone()
        .begin_atomic_directory(&destination)
        .expect("begin generation");
    for leaf in [
        "../escape",
        "nested/frame.png",
        "bad\\name",
        "C:",
        "bad:name",
        "_leading",
        "trailing.",
        "trailing ",
        "CON",
        "con.txt",
        "COM1.png",
        "LPT9.gif",
        ATOMIC_DIRECTORY_COMPLETE_LEAF,
    ] {
        assert!(
            writer.write_file(Path::new(leaf), b"x").is_err(),
            "unsafe cross-platform leaf was accepted: {leaf:?}"
        );
    }
    writer
        .write_file(Path::new("frame_0001.png"), b"one")
        .expect("first child");
    assert!(
        writer
            .write_file(Path::new("frame_0001.png"), b"duplicate")
            .is_err()
    );
    writer
        .write_file(Path::new("frame_0002.png"), b"two")
        .expect("second child");
    let prepared = writer.prepare().expect("prepare generation");
    assert!(!fs.exists(&destination));
    prepared.commit().expect("publish complete generation");
    assert_eq!(
        fs.list_dir(&destination).expect("generation listing"),
        vec![
            destination.join(ATOMIC_DIRECTORY_COMPLETE_LEAF),
            destination.join("frame_0001.png"),
            destination.join("frame_0002.png"),
        ]
    );

    let mut contender = fs
        .clone()
        .begin_atomic_directory(&destination)
        .expect("begin competing generation");
    contender
        .write_file(Path::new("frame_0001.png"), b"new")
        .expect("private competing child");
    let contender = contender.prepare().expect("prepare competitor");
    assert!(contender.commit().is_err(), "existing generation wins");
    assert_eq!(
        fs.read(&destination.join("frame_0001.png"))
            .expect("original child"),
        b"one"
    );
    assert_eq!(
        fs.read(&destination.join("frame_0002.png"))
            .expect("original tail"),
        b"two"
    );
}

#[cfg(unix)]
#[test]
fn std_fs_classifies_links_without_following_them() {
    let dir = scratch("stdfs_no_follow_kind");
    let _ = StdFs.remove_dir_all(&dir);
    std::fs::create_dir(&dir).expect("create root");
    let target = dir.join("target");
    std::fs::write(&target, b"target").expect("write target");
    let link = dir.join("link");
    std::os::unix::fs::symlink(&target, &link).expect("create link");

    assert_eq!(
        StdFs.node_kind_no_follow(&link).expect("classify link"),
        Some(FsNodeKind::Link)
    );
    assert!(
        StdFs.create_dir(&link).is_err(),
        "exact directory creation must not accept a link winner"
    );
    assert_eq!(
        StdFs.node_kind_no_follow(&target).expect("classify target"),
        Some(FsNodeKind::RegularFile)
    );
}

fn spec(program: &str, argv: &[&str]) -> ProcessSpec {
    ProcessSpec {
        program: PathBuf::from(program),
        argv: argv.iter().map(ToString::to_string).collect(),
        env: Vec::new(),
        cwd: None,
        stdin: None,
        timeout: Duration::from_secs(10),
        max_output_bytes: 1 << 20,
    }
}

#[cfg(unix)]
mod std_runner {
    use super::*;
    use fmn_platform::process::StdProcessRunner;
    use std::sync::mpsc;
    use std::thread;

    #[test]
    fn argv_only_echo_succeeds() {
        let out = StdProcessRunner
            .run(&spec("/usr/bin/echo", &["hello", "argv world"]))
            .expect("run");
        assert!(out.success());
        // The two argv entries arrive as two arguments — no shell splitting.
        assert_eq!(out.stdout, b"hello argv world\n");
        assert!(matches!(out.termination, ProcessTermination::Exited(_)));
    }

    #[test]
    fn nonzero_exit_is_an_outcome_not_an_error() {
        let out = StdProcessRunner
            .run(&spec("/usr/bin/false", &[]))
            .expect("run");
        assert_eq!(out.termination, ProcessTermination::Exited(Some(1)));
        assert!(!out.success());
    }

    #[test]
    fn environment_is_cleared_then_allowlisted() {
        let mut s = spec("/usr/bin/env", &[]);
        s.env = vec![("FMN_ALLOWED".to_string(), "yes".to_string())];
        let out = StdProcessRunner.run(&s).expect("run");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains("FMN_ALLOWED=yes"), "{text}");
        // Nothing ambient leaks: PATH/HOME are gone.
        assert!(!text.contains("PATH="), "ambient PATH leaked: {text}");
        assert!(!text.contains("HOME="), "ambient HOME leaked: {text}");
    }

    #[test]
    fn stdin_bytes_flow_through() {
        let mut s = spec("/usr/bin/cat", &[]);
        s.stdin = Some(b"through the capability".to_vec());
        let out = StdProcessRunner.run(&s).expect("run");
        assert!(out.success());
        assert_eq!(out.stdout, b"through the capability");
    }

    #[test]
    fn timeout_kills_the_complete_process_group() {
        let script = scratch("process_tree").join("ffmpeg-with-descendant.sh");
        std::fs::write(&script, "sleep 5\n").expect("write descendant fixture");
        let script = script.to_str().expect("UTF-8 fixture path");
        let mut s = spec("/bin/sh", &[script]);
        s.timeout = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let out = StdProcessRunner.run(&s).expect("run");
        assert_eq!(out.termination, ProcessTermination::TimedOut);
        assert!(!out.success());
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "process-tree kill was not prompt: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn exited_group_leader_cannot_disarm_descendant_timeout() {
        let script = scratch("process_tree").join("ffmpeg-with-background-descendant.sh");
        std::fs::write(&script, "sleep 5 &\nexit 0\n")
            .expect("write background-descendant fixture");
        let script = script.to_str().expect("UTF-8 fixture path");
        let mut s = spec("/bin/sh", &[script]);
        s.timeout = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let out = StdProcessRunner.run(&s).expect("run");
        assert_eq!(out.termination, ProcessTermination::TimedOut);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "exited leader disarmed process-tree kill: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn successful_exit_kills_redirected_descendants_before_return() {
        let dir = scratch("process_tree");
        let script = dir.join("ffmpeg-with-redirected-descendant.sh");
        let marker = dir.join(format!(
            "redirected-descendant-leak-{}.txt",
            std::process::id()
        ));
        assert!(!marker.exists(), "test marker must start absent");
        std::fs::write(
            &script,
            "(sleep 1; printf leaked > \"$1\") >/dev/null 2>&1 &\nexit 0\n",
        )
        .expect("write redirected-descendant fixture");
        let script = script.to_str().expect("UTF-8 fixture path");
        let marker_arg = marker.to_str().expect("UTF-8 marker path");
        let started = std::time::Instant::now();
        let out = StdProcessRunner
            .run(&spec("/bin/sh", &[script, marker_arg]))
            .expect("run");
        assert!(out.success(), "group leader should retain its zero exit");
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "successful process-tree cleanup was not prompt: {:?}",
            started.elapsed()
        );
        // A leaked descendant would create the marker after its delayed write.
        std::thread::sleep(Duration::from_millis(1_200)); // ubs:ignore bounded process-lifecycle probe
        assert!(
            !marker.exists(),
            "redirected descendant outlived successful group leader"
        );
    }

    #[test]
    fn output_cap_kills_and_truncates() {
        // `yes` produces unbounded output; the cap must stop it long before
        // the timeout.
        let mut s = spec("/usr/bin/yes", &[]);
        s.timeout = Duration::from_secs(30);
        s.max_output_bytes = 64 * 1024;
        let started = std::time::Instant::now();
        let out = StdProcessRunner.run(&s).expect("run");
        assert_eq!(out.termination, ProcessTermination::OutputLimitExceeded);
        assert!(out.stdout.len() as u64 <= s.max_output_bytes);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "cap kill was not prompt: {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn spawn_failure_is_a_named_mechanism_error() {
        let err = StdProcessRunner
            .run(&spec("/nonexistent/fmn-no-such-program", &[]))
            .unwrap_err();
        assert!(err.to_string().contains("fmn-no-such-program"));
    }

    #[test]
    fn external_cancellation_unblocks_a_full_stdin_pipe_and_reaps_the_child() {
        let cancellation = ProcessCancellation::new();
        let process = StdProcessRunner
            .start(
                &spec("/usr/bin/sleep", &["30"]),
                cancellation.clone(),
                ProcessStdinLimits::new(8 << 20, 8 << 20),
            )
            .expect("start sleeping child");
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let mut process = process;
            started_tx.send(()).expect("start signal");
            let write = process.write_stdin(&vec![0u8; 8 << 20]);
            let finish = process.finish();
            (write, finish)
        });

        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("writer started");
        thread::sleep(Duration::from_millis(50));
        // ubs:ignore - elapsed-time assertion, not security-token generation.
        let started = std::time::Instant::now();
        cancellation.cancel();
        let (write, finish) = worker.join().expect("writer thread");
        assert!(write.is_err(), "the killed child's pipe must close");
        assert_eq!(
            finish.expect("supervisor outcome").termination,
            ProcessTermination::Cancelled
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "cancellation did not promptly unblock and reap"
        );
    }

    #[test]
    fn dropping_a_live_session_cancels_and_reaps_before_returning() {
        // ubs:ignore - elapsed-time assertion, not security-token generation.
        let started = std::time::Instant::now();
        let process = StdProcessRunner
            .start(
                &spec("/usr/bin/sleep", &["30"]),
                ProcessCancellation::new(),
                ProcessStdinLimits::new(1, 1),
            )
            .expect("start sleeping child");
        drop(process);
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "drop did not promptly cancel and reap"
        );
    }
}

#[test]
fn relative_program_paths_are_refused_by_contract() {
    // Both runners enforce it — PATH resolution is unreachable through the
    // capability (D2: the boundary resolves and fingerprints its one tool).
    let s = spec("echo", &["hi"]);
    let err = ScriptedRunner::new().run(&s).unwrap_err();
    assert!(err.to_string().contains("not absolute"), "{err}");
    #[cfg(unix)]
    {
        use fmn_platform::process::StdProcessRunner;
        assert!(StdProcessRunner.run(&s).is_err());
    }
}

#[test]
fn scripted_runner_replays_and_logs() {
    let mut r = ScriptedRunner::new();
    r.script(
        "/fake/ffmpeg",
        ProcessOutcome {
            termination: ProcessTermination::Exited(Some(0)),
            stdout: b"frame=  1".to_vec(),
            stderr: Vec::new(),
        },
    );
    let s = spec("/fake/ffmpeg", &["-i", "-", "out.mp4"]);
    let out = r.run(&s).expect("scripted");
    assert!(out.success());
    assert_eq!(out.stdout, b"frame=  1");
    assert!(r.run(&spec("/fake/other", &[])).is_err());
    let runs = r.runs();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].argv, vec!["-i", "-", "out.mp4"]);
    assert!(runs[0].stdin.is_none());
}

#[test]
fn scripted_stream_preserves_chunk_order_and_enforces_both_input_bounds() {
    let mut runner = ScriptedRunner::new();
    runner.script(
        "/fake/ffmpeg",
        ProcessOutcome {
            termination: ProcessTermination::Exited(Some(0)),
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
    );
    let runner = Arc::new(runner);
    let stream_spec = spec("/fake/ffmpeg", &["-i", "-", "out.mp4"]);
    let mut process = runner
        .start(
            &stream_spec,
            ProcessCancellation::new(),
            ProcessStdinLimits::new(4, 6),
        )
        .expect("start scripted stream");
    process.write_stdin(b"ab").expect("first chunk");
    process.write_stdin(b"cdef").expect("second chunk");
    assert!(process.finish().expect("finish").success());
    assert_eq!(
        runner.runs()[0].stdin.as_deref(),
        Some(b"abcdef".as_slice())
    );

    let mut process = runner
        .start(
            &stream_spec,
            ProcessCancellation::new(),
            ProcessStdinLimits::new(3, 10),
        )
        .expect("start chunk-bound stream");
    assert!(matches!(
        process.write_stdin(b"four"),
        Err(fmn_platform::process::ProcessError::StdinChunkLimit {
            attempted: 4,
            max: 3,
            ..
        })
    ));
    process.cancel().expect("cancel chunk-bound stream");

    let mut process = runner
        .start(
            &stream_spec,
            ProcessCancellation::new(),
            ProcessStdinLimits::new(4, 5),
        )
        .expect("start total-bound stream");
    process.write_stdin(b"abc").expect("first bounded chunk");
    assert!(matches!(
        process.write_stdin(b"def"),
        Err(fmn_platform::process::ProcessError::StdinTotalLimit {
            attempted: 6,
            max: 5,
            ..
        })
    ));
    process.cancel().expect("cancel total-bound stream");
}
