//! A native scene owns one real engine; disk failures never substitute ink.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fmn_config::Config;
use fmn_platform::clock::FakeClock;
use fmn_platform::fs::{FileSystem, VirtualFs};
use fmn_tex::{Mode, Style, TexError, TexSession, TypesetSessionReport};

fn root() -> &'static str {
    if cfg!(windows) {
        r"C:\typeset"
    } else {
        "/typeset"
    }
}

fn config() -> Config {
    let mut config = Config::resolve(&[], None).unwrap().config;
    config.directories.cache = root().to_owned();
    config
}

fn session(config: &Config, fs: &Arc<VirtualFs>) -> TexSession {
    TexSession::with_cache(config, fs.clone(), Arc::new(FakeClock::new()))
}

fn bytes(session: &TexSession, source: &str) -> Vec<u8> {
    session
        .engine()
        .unwrap()
        .typeset(Mode::Math(Style::Display), source)
        .unwrap()
        .to_bytes()
        .unwrap()
}

#[test]
fn construction_and_reports_are_lazy_and_the_engine_is_reused() {
    let fs = Arc::new(VirtualFs::new());
    let session = session(&config(), &fs);
    assert_eq!(session.report(), TypesetSessionReport::default());
    assert!(!fs.exists(Path::new(root())));
    let first = session.engine().unwrap();
    assert!(std::ptr::eq(first, session.engine().unwrap()));
    assert!(session.report().initialized);
    assert!(session.report().persistent);
    assert!(session.report().cache_error.is_none());
    assert!(fs.exists(Path::new(root())));
}

#[test]
fn a_fresh_session_uses_verified_disk_layouts_then_the_memory_front() {
    let fs = Arc::new(VirtualFs::new());
    let config = config();
    let source = r"\frac{x^2+1}{2}";
    let cold = session(&config, &fs);
    let expected = bytes(&cold, source);
    assert_eq!(cold.report().layout_computations, 1);
    assert_eq!(cold.report().persistent_hits, 0);
    drop(cold);

    let warm = session(&config, &fs);
    assert_eq!(bytes(&warm, source), expected);
    assert_eq!(warm.report().layout_computations, 0);
    assert_eq!(warm.report().persistent_hits, 1);
    assert_eq!(bytes(&warm, source), expected);
    assert_eq!(warm.report().memory.hits, 1);
    assert_eq!(warm.report().persistent_hits, 1);

    let memory = TexSession::default();
    assert_eq!(bytes(&memory, source), expected);
    assert!(!memory.report().persistent);
    assert_eq!(memory.report().layout_computations, 1);
}

#[test]
fn a_foreign_cache_directory_is_not_claimed_and_layout_still_succeeds() {
    let fs = Arc::new(VirtualFs::new());
    let foreign = Path::new(root()).join("unrelated.txt");
    fs.insert(&foreign, b"leave this alone".to_vec());
    let before = fs.list_dir(Path::new(root())).unwrap();
    let session = session(&config(), &fs);
    assert!(!bytes(&session, "x").is_empty());
    let report = session.report();
    assert!(report.initialized);
    assert!(!report.persistent);
    assert!(report.cache_error.is_some());
    assert_eq!(report.layout_computations, 1);
    assert_eq!(fs.read(&foreign).unwrap(), b"leave this alone");
    assert_eq!(fs.list_dir(Path::new(root())).unwrap(), before);
}

#[test]
fn injected_filesystems_never_resolve_ambient_cache_paths() {
    let escaping = PathBuf::from(root())
        .join("..")
        .join("escape")
        .to_string_lossy()
        .into_owned();
    for directory in [String::new(), "relative-cache".to_owned(), escaping] {
        let fs = Arc::new(VirtualFs::new());
        let mut config = config();
        config.directories.cache = directory;
        let session = session(&config, &fs);
        assert!(!bytes(&session, "x").is_empty());
        assert!(!session.report().persistent);
        assert!(session.report().cache_error.unwrap().contains("explicit absolute"));
        assert!(!fs.exists(Path::new(root())));
    }
}

#[test]
fn template_refusals_precede_cache_io_instead_of_falling_back_to_default() {
    let fs = Arc::new(VirtualFs::new());
    let mut config = config();
    config.tex.template = "not-a-native-template".to_owned();
    let session = session(&config, &fs);
    assert!(matches!(session.engine(), Err(TexError::Pack(_))));
    assert_eq!(session.report(), TypesetSessionReport::default());
    assert!(!fs.exists(Path::new(root())));
}

#[test]
fn the_selected_template_reaches_the_engine_and_empty_means_default() {
    for (template, pack) in [("basic", "fmd-math/pack/basic"), ("", "fmd-math/pack/default")] {
        let session = TexSession::memory(template);
        let reference = fmn_tex::TexEngine::new(pack, None).unwrap();
        let expected = reference
            .typeset(Mode::Math(Style::Display), "x+1")
            .unwrap()
            .to_bytes()
            .unwrap();
        assert_eq!(bytes(&session, "x+1"), expected);
        assert!(!session.report().persistent);
    }
}
