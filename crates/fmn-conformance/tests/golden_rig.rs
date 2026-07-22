//! End-to-end demonstration of the self-golden rig (fm-xb3 acceptance):
//! create → lock → match → mutate → CI-fail (with `.actual` sidecar) → bless.
//!
//! Runs against a scratch store under `CARGO_TARGET_TMPDIR`, so the committed
//! goldens are untouched; `tests/self_goldens.rs` is the rig's live use.
//! Modes are passed explicitly (never via the environment) so the tests are
//! parallel-safe.

use fmn_conformance::golden::{GoldenError, GoldenStore, Mode, Scope, Verdict, platform_key};
use std::path::PathBuf;

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("golden_rig_{name}"));
    // A fresh subdirectory per test; stale files from a previous run are
    // overwritten by the rig itself (bless), so no cleanup pass is needed.
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn full_lifecycle_create_lock_drift_bless() {
    let dir = scratch("lifecycle");
    let store = GoldenStore::new(&dir, "demo", Scope::PerPlatform).expect("store");
    // Locks are per-platform: the file name carries the platform key.
    let lock = store.lock_path();
    assert!(
        lock.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains(&platform_key()),
        "lock path {lock:?} must embed the platform key"
    );
    let _ = std::fs::remove_file(&lock); // reset from any previous run (scratch only)

    // 1. CREATE: an unlocked artifact fails in check mode and writes a sidecar.
    let v1 = b"artifact bytes, version 1".to_vec();
    let err = store
        .check_with_mode("trivial", &v1, Mode::Check)
        .expect_err("unlocked artifact must fail in check mode");
    let sidecar = match err {
        GoldenError::Drift {
            ref name,
            expected: None,
            ref sidecar,
            ..
        } => {
            assert_eq!(name, "trivial");
            sidecar.clone()
        }
        other => panic!("expected no-entry drift, got: {other}"),
    };
    assert_eq!(std::fs::read(&sidecar).expect("sidecar"), v1);

    // 2. LOCK: bless writes the lock entry; the rig never commits anything.
    let verdict = store
        .check_with_mode("trivial", &v1, Mode::Bless)
        .expect("bless");
    assert_eq!(verdict, Verdict::Blessed { previous: None });
    assert!(lock.is_file(), "bless must materialize the lock file");

    // 3. MATCH: the same bytes now pass in check mode.
    assert_eq!(
        store
            .check_with_mode("trivial", &v1, Mode::Check)
            .expect("relock match"),
        Verdict::Match
    );

    // 4. MUTATE → CI-FAIL: changed bytes drift, with both entries reported.
    let v2 = b"artifact bytes, version 2 (drifted)".to_vec();
    let err = store
        .check_with_mode("trivial", &v2, Mode::Check)
        .expect_err("drifted artifact must fail in check mode");
    match err {
        GoldenError::Drift {
            expected: Some(ref e),
            ref actual,
            ref sidecar,
            ..
        } => {
            assert_eq!(e.len, v1.len() as u64);
            assert_eq!(actual.len, v2.len() as u64);
            assert_ne!(e.sha256_hex, actual.sha256_hex);
            assert_eq!(std::fs::read(sidecar).expect("sidecar"), v2);
        }
        other => panic!("expected drift with previous entry, got: {other}"),
    }

    // 5. BLESS: deliberate re-lock accepts the new bytes and reports what it
    //    replaced; a subsequent check passes.
    let verdict = store
        .check_with_mode("trivial", &v2, Mode::Bless)
        .expect("re-bless");
    match verdict {
        Verdict::Blessed { previous: Some(p) } => assert_eq!(p.len, v1.len() as u64),
        other => panic!("expected replacing bless, got {other:?}"),
    }
    assert_eq!(
        store
            .check_with_mode("trivial", &v2, Mode::Check)
            .expect("post-bless match"),
        Verdict::Match
    );
}

#[test]
fn lock_file_bytes_are_deterministic() {
    let dir = scratch("deterministic");
    let store = GoldenStore::new(&dir, "det", Scope::Certified).expect("store");
    let _ = std::fs::remove_file(store.lock_path());
    // Bless in one order…
    store
        .check_with_mode("b-second", b"bb", Mode::Bless)
        .expect("bless b");
    store
        .check_with_mode("a-first", b"aa", Mode::Bless)
        .expect("bless a");
    let one = std::fs::read_to_string(store.lock_path()).expect("lock");
    // …then re-bless the identical content in the opposite order; the file
    // must be byte-identical (sorted rows, versioned header).
    let _ = std::fs::remove_file(store.lock_path());
    store
        .check_with_mode("a-first", b"aa", Mode::Bless)
        .expect("bless a");
    store
        .check_with_mode("b-second", b"bb", Mode::Bless)
        .expect("bless b");
    let two = std::fs::read_to_string(store.lock_path()).expect("lock");
    assert_eq!(one, two, "lock bytes must not depend on bless order");
    assert!(one.starts_with("# fmn-golden-lock v1 suite=det key=certified\n"));
    // Certified scope shares one lock file across the whole matrix.
    assert!(store.lock_path().ends_with("det.certified.lock"));
}

#[test]
fn names_are_path_components_never_traversal() {
    let dir = scratch("names");
    let store = GoldenStore::new(&dir, "names", Scope::PerPlatform).expect("store");
    for bad in ["../escape", "a/b", "", ".hidden", "UPPER", "sp ace"] {
        assert!(
            matches!(
                store.check_with_mode(bad, b"x", Mode::Check),
                Err(GoldenError::InvalidName(_))
            ),
            "name {bad:?} must be refused"
        );
    }
    assert!(GoldenStore::new(&dir, "../sneaky", Scope::PerPlatform).is_err());
}

#[test]
fn corrupt_lock_is_a_named_error_not_a_pass() {
    let dir = scratch("corrupt");
    let store = GoldenStore::new(&dir, "corrupt", Scope::PerPlatform).expect("store");
    std::fs::write(store.lock_path(), "not a lock header\n").expect("write");
    match store.check_with_mode("x", b"x", Mode::Check) {
        Err(GoldenError::Corrupt { line: 1, .. }) => {}
        other => panic!("expected corrupt-lock error, got {other:?}"),
    }
}
