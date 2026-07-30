//! Capability trait-suite tests (fm-x68 acceptance): the std implementations
//! against the real host, and the test doubles' contracts.
//!
//! The process tests exercise the D2 mechanism substrate for real: argv-only
//! spawning, the cleared-environment allowlist, timeout kill, output-cap
//! kill, and stdin plumbing — all against coreutils, Unix-only (`cfg`).

use fmn_platform::fs::{ATOMIC_DIRECTORY_COMPLETE_LEAF, FileSystem, FsNodeKind, StdFs, VirtualFs};
use fmn_platform::process::{
    FfmpegLocator, FfmpegLocatorError, ProcessCancellation, ProcessOutcome, ProcessRunner,
    ProcessSpec, ProcessStdinLimits, ProcessTermination, ScriptedRunner, StdFfmpegLocator,
};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("caps_{name}"));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn locator_scratch(name: &str) -> std::io::Result<PathBuf> {
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    for _ in 0..128 {
        let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "ffmpeg_locator_{name}_{}_{sequence}",
            std::process::id()
        ));
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok(dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not claim a unique ffmpeg locator scratch directory",
    ))
}

fn write_executable_bytes(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("write executable fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("mark fixture executable");
    }
}

fn native_executable_fixture() -> Vec<u8> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        return b"\x7fELFffmpeg locator fixture".to_vec();
    }
    #[cfg(target_os = "macos")]
    {
        return b"\xcf\xfa\xed\xfeffmpeg locator fixture".to_vec();
    }
    #[cfg(windows)]
    {
        let mut bytes = vec![0_u8; 0x84];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x80_u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        return bytes;
    }
    #[allow(unreachable_code)]
    b"unsupported host executable fixture".to_vec()
}

fn write_executable(path: &Path) {
    write_executable_bytes(path, &native_executable_fixture());
}

#[test]
fn ffmpeg_locator_canonicalizes_explicit_identity_without_consulting_path() {
    let root = locator_scratch("explicit").expect("claim explicit locator scratch");
    let tool = root.join("custom-tool-name");
    write_executable(&tool);
    let locator = StdFfmpegLocator::from_search_path(Some(OsString::from(":relative")));

    let resolved = locator
        .locate_ffmpeg(&tool)
        .expect("absolute configuration bypasses PATH");
    assert_eq!(
        resolved.canonical_path(),
        std::fs::canonicalize(&tool)
            .expect("canonical fixture")
            .as_path()
    );

    #[cfg(unix)]
    {
        let not_executable = root.join("not-executable");
        std::fs::write(&not_executable, b"plain file").expect("write plain file");
        assert!(matches!(
            locator.locate_ffmpeg(&not_executable),
            Err(FfmpegLocatorError::NotExecutable { .. })
        ));
    }
}

#[test]
fn ffmpeg_locator_validates_the_complete_search_policy_before_lookup() {
    let root = locator_scratch("policy").expect("claim policy locator scratch");
    let bin = root.join("bin");
    std::fs::create_dir(&bin).expect("create search directory");
    write_executable(&bin.join(if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    }));

    assert!(matches!(
        StdFfmpegLocator::default().locate_ffmpeg(Path::new("ffmpeg")),
        Err(FfmpegLocatorError::SearchPathUnavailable)
    ));

    let empty_cases = [
        (
            std::env::join_paths([Path::new(""), bin.as_path()]).expect("leading-empty PATH"),
            0,
        ),
        (
            std::env::join_paths([bin.as_path(), Path::new("")]).expect("trailing-empty PATH"),
            1,
        ),
        (
            std::env::join_paths([bin.as_path(), Path::new(""), bin.as_path()])
                .expect("middle-empty PATH"),
            1,
        ),
    ];
    for (search_path, expected_index) in empty_cases {
        assert!(matches!(
            StdFfmpegLocator::from_search_path(Some(search_path))
                .locate_ffmpeg(Path::new("ffmpeg")),
            Err(FfmpegLocatorError::EmptySearchEntry { index })
                if index == expected_index
        ));
    }

    let relative =
        std::env::join_paths([bin.as_path(), Path::new("relative")]).expect("relative-entry PATH");
    assert!(matches!(
        StdFfmpegLocator::from_search_path(Some(relative)).locate_ffmpeg(Path::new("ffmpeg")),
        Err(FfmpegLocatorError::RelativeSearchEntry { index: 1, .. })
    ));

    let parent_traversal = root.join("elsewhere").join("..");
    let parent = std::env::join_paths([bin.as_path(), parent_traversal.as_path()])
        .expect("parent-entry PATH");
    assert!(matches!(
        StdFfmpegLocator::from_search_path(Some(parent)).locate_ffmpeg(Path::new("ffmpeg")),
        Err(FfmpegLocatorError::ParentTraversalSearchEntry { index: 1, .. })
    ));
}

#[cfg(unix)]
#[test]
fn ffmpeg_locator_skips_present_non_executables_in_search_order() {
    use std::os::unix::fs::symlink;
    use std::os::unix::net::UnixListener;

    let root = locator_scratch("nonexec").expect("claim nonexec locator scratch");
    let broken = root.join("broken");
    let directory = root.join("directory");
    let special = root.join("special");
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir(&broken).expect("broken-link search directory");
    std::fs::create_dir(&directory).expect("directory-target search directory");
    std::fs::create_dir(&special).expect("special-file search directory");
    std::fs::create_dir(&first).expect("first search directory");
    std::fs::create_dir(&second).expect("second search directory");
    symlink(root.join("missing-target"), broken.join("ffmpeg")).expect("broken candidate link");
    symlink(&root, directory.join("ffmpeg")).expect("directory candidate link");
    let _socket =
        UnixListener::bind(special.join("ffmpeg")).expect("special-file candidate socket");
    std::fs::write(first.join("ffmpeg"), native_executable_fixture())
        .expect("non-executable native candidate");
    write_executable(&second.join("ffmpeg"));
    let search_path = std::env::join_paths([
        broken.as_path(),
        directory.as_path(),
        special.as_path(),
        first.as_path(),
        second.as_path(),
    ])
    .expect("ordered PATH");
    let locator = StdFfmpegLocator::from_search_path(Some(search_path));

    let resolved = locator
        .locate_ffmpeg(Path::new("ffmpeg"))
        .expect("later executable candidate");
    assert_eq!(
        resolved.canonical_path(),
        std::fs::canonicalize(second.join("ffmpeg"))
            .expect("canonical second candidate")
            .as_path()
    );

    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(first.join("ffmpeg"), std::fs::Permissions::from_mode(0o755))
        .expect("promote first candidate");
    assert_eq!(
        locator
            .locate_ffmpeg(Path::new("ffmpeg"))
            .expect("source-order candidate")
            .canonical_path(),
        std::fs::canonicalize(first.join("ffmpeg"))
            .expect("canonical first candidate")
            .as_path(),
        "the first valid candidate must win"
    );
    assert!(matches!(
        locator.locate_ffmpeg(Path::new("ffprobe")),
        Err(FfmpegLocatorError::UnsupportedConfiguredName { .. })
    ));
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn ffmpeg_locator_refuses_interpreter_wrappers_and_skips_them_during_search() {
    let root = locator_scratch("wrapper").expect("claim wrapper locator scratch");
    let wrapper_bin = root.join("wrapper-bin");
    let native_bin = root.join("native-bin");
    std::fs::create_dir(&wrapper_bin).expect("create wrapper search directory");
    std::fs::create_dir(&native_bin).expect("create native search directory");
    let wrapper = wrapper_bin.join("ffmpeg");
    write_executable_bytes(&wrapper, b"#!/bin/sh\nprintf 'ffmpeg version wrapper\\n'\n");
    let native = native_bin.join("ffmpeg");
    write_executable(&native);

    assert!(matches!(
        StdFfmpegLocator::default().locate_ffmpeg(&wrapper),
        Err(FfmpegLocatorError::UnsupportedExecutableFormat { .. })
    ));

    let search_path = std::env::join_paths([wrapper_bin.as_path(), native_bin.as_path()])
        .expect("ordered wrapper/native PATH");
    let resolved = StdFfmpegLocator::from_search_path(Some(search_path))
        .locate_ffmpeg(Path::new("ffmpeg"))
        .expect("search skips an interpreter wrapper");
    assert_eq!(
        resolved.canonical_path(),
        std::fs::canonicalize(native)
            .expect("canonical native fixture")
            .as_path()
    );
}

#[cfg(unix)]
#[test]
fn ffmpeg_locator_canonicalizes_symlinks_and_resists_later_retargeting() {
    use std::os::unix::fs::symlink;

    let root = locator_scratch("symlink").expect("claim symlink locator scratch");
    let bin = root.join("bin");
    std::fs::create_dir(&bin).expect("search directory");
    let first = root.join("ffmpeg-first");
    let second = root.join("ffmpeg-second");
    write_executable(&first);
    write_executable(&second);
    let searched = bin.join("ffmpeg");
    symlink(&first, &searched).expect("first ffmpeg symlink");
    let search_path = std::env::join_paths([bin.as_path()]).expect("search PATH");
    let locator = StdFfmpegLocator::from_search_path(Some(search_path));

    let first_resolution = locator
        .locate_ffmpeg(Path::new("ffmpeg"))
        .expect("resolve first target");
    let displaced = bin.join("ffmpeg-original-link");
    std::fs::rename(&searched, &displaced).expect("displace original symlink");
    symlink(&second, &searched).expect("retarget ffmpeg symlink");

    assert_eq!(
        first_resolution.canonical_path(),
        std::fs::canonicalize(&first)
            .expect("canonical first target")
            .as_path(),
        "retargeting the search symlink must not change an issued identity"
    );
    assert_eq!(
        locator
            .locate_ffmpeg(Path::new("ffmpeg"))
            .expect("resolve replacement target")
            .canonical_path(),
        std::fs::canonicalize(&second)
            .expect("canonical second target")
            .as_path()
    );
}

#[cfg(unix)]
#[test]
fn ffmpeg_locator_rejects_hostile_bytes_and_never_interprets_shell_text() {
    use std::os::unix::ffi::OsStringExt as _;

    let with_nul = OsString::from_vec(b"/tmp/fmn\0hostile".to_vec());
    assert!(matches!(
        StdFfmpegLocator::from_search_path(Some(with_nul)).locate_ffmpeg(Path::new("ffmpeg")),
        Err(FfmpegLocatorError::MalformedSearchPath { .. })
    ));

    let non_utf8 = OsString::from_vec(b"/tmp/fmn-\xff".to_vec());
    assert!(matches!(
        StdFfmpegLocator::from_search_path(Some(non_utf8)).locate_ffmpeg(Path::new("ffmpeg")),
        Err(FfmpegLocatorError::NonUtf8SearchEntry { index: 0 })
    ));

    let with_control = OsString::from("/tmp/fmn-\n-hostile");
    assert!(matches!(
        StdFfmpegLocator::from_search_path(Some(with_control)).locate_ffmpeg(Path::new("ffmpeg")),
        Err(FfmpegLocatorError::ControlSearchEntry { index: 0 })
    ));

    let oversized = OsString::from(format!("/tmp/{}", "x".repeat(70 * 1024)));
    assert!(matches!(
        StdFfmpegLocator::from_search_path(Some(oversized)).locate_ffmpeg(Path::new("ffmpeg")),
        Err(FfmpegLocatorError::SearchPathLimit {
            resource: "size",
            ..
        })
    ));

    let root = locator_scratch("shell_text").expect("claim hostile locator scratch");
    let marker = root.join("shell-was-interpreted");
    let hostile = OsString::from(format!("{};touch {}", root.display(), marker.display()));
    assert!(matches!(
        StdFfmpegLocator::from_search_path(Some(hostile)).locate_ffmpeg(Path::new("ffmpeg")),
        Err(FfmpegLocatorError::NotFound)
    ));
    assert!(
        !marker.exists(),
        "PATH bytes must be filesystem text, never a shell program"
    );

    let control_target = root.join("ffmpeg\ncontrol");
    write_executable(&control_target);
    assert!(matches!(
        StdFfmpegLocator::default().locate_ffmpeg(&control_target),
        Err(FfmpegLocatorError::InvalidCanonicalIdentity { .. })
    ));
}

#[cfg(windows)]
#[test]
fn windows_ffmpeg_search_uses_only_the_fixed_exe_leaf() {
    let root = locator_scratch("windows_name").expect("claim Windows locator scratch");
    let bin = root.join("bin");
    std::fs::create_dir(&bin).expect("search directory");
    let tool = bin.join("ffmpeg.exe");
    write_executable(&tool);
    let search_path = std::env::join_paths([bin.as_path()]).expect("search PATH");
    let locator = StdFfmpegLocator::from_search_path(Some(search_path));

    for configured in ["ffmpeg", "FFMPEG", "ffmpeg.exe", "FFMPEG.EXE"] {
        assert_eq!(
            locator
                .locate_ffmpeg(Path::new(configured))
                .expect("fixed Windows name")
                .canonical_path(),
            std::fs::canonicalize(&tool)
                .expect("canonical ffmpeg.exe")
                .as_path()
        );
    }
    assert!(matches!(
        locator.locate_ffmpeg(Path::new("ffmpeg.com")),
        Err(FfmpegLocatorError::UnsupportedConfiguredName { .. })
    ));

    let malformed = StdFfmpegLocator::from_search_path(Some(OsString::from("\"C:\\bin")));
    assert!(matches!(
        malformed.locate_ffmpeg(Path::new("ffmpeg")),
        Err(FfmpegLocatorError::MalformedSearchPath { .. })
    ));

    for extension in ["bat", "cmd"] {
        let wrapper = root.join(format!("custom-wrapper.{extension}"));
        write_executable_bytes(&wrapper, b"@echo off\r\necho ffmpeg version wrapper\r\n");
        assert!(matches!(
            locator.locate_ffmpeg(&wrapper),
            Err(FfmpegLocatorError::UnsupportedExecutableFormat { .. })
        ));
    }

    let truncated = root.join("truncated-mz.exe");
    write_executable_bytes(&truncated, b"MZ");
    assert!(matches!(
        locator.locate_ffmpeg(&truncated),
        Err(FfmpegLocatorError::UnsupportedExecutableFormat { .. })
    ));

    let out_of_file = root.join("out-of-file-pe.exe");
    let mut out_of_file_bytes = vec![0_u8; 64];
    out_of_file_bytes[..2].copy_from_slice(b"MZ");
    out_of_file_bytes[0x3c..0x40].copy_from_slice(&u32::MAX.to_le_bytes());
    write_executable_bytes(&out_of_file, &out_of_file_bytes);
    assert!(matches!(
        locator.locate_ffmpeg(&out_of_file),
        Err(FfmpegLocatorError::UnsupportedExecutableFormat { .. })
    ));

    let bad_signature = root.join("bad-pe-signature.exe");
    let mut bad_signature_bytes = native_executable_fixture();
    bad_signature_bytes[0x80..0x84].copy_from_slice(b"PX\0\0");
    write_executable_bytes(&bad_signature, &bad_signature_bytes);
    assert!(matches!(
        locator.locate_ffmpeg(&bad_signature),
        Err(FfmpegLocatorError::UnsupportedExecutableFormat { .. })
    ));
}

#[cfg(target_os = "macos")]
#[test]
fn macos_ffmpeg_locator_accepts_every_supported_mach_o_container_magic() {
    let root = locator_scratch("mach_o_magics").expect("claim Mach-O locator scratch");
    let magics = [
        [0xfe, 0xed, 0xfa, 0xce],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe],
        [0xbe, 0xba, 0xfe, 0xca],
        [0xca, 0xfe, 0xba, 0xbf],
        [0xbf, 0xba, 0xfe, 0xca],
    ];
    for (index, magic) in magics.into_iter().enumerate() {
        let candidate = root.join(format!("ffmpeg-magic-{index}"));
        write_executable_bytes(&candidate, &magic);
        assert_eq!(
            StdFfmpegLocator::default()
                .locate_ffmpeg(&candidate)
                .expect("accepted Mach-O container magic")
                .canonical_path(),
            std::fs::canonicalize(candidate)
                .expect("canonical Mach-O fixture")
                .as_path()
        );
    }
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
