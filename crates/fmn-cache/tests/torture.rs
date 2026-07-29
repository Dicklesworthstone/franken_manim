//! The fm-fw6 acceptance suite, host half: two store openings (as two `fmn`
//! invocations would be) hammering one real directory through `StdFs` from
//! many threads — writers, readers, and maintainers — with the invariant that
//! **no observer ever sees corruption**: every get is a verified value or a
//! miss, every raw object file on disk is a complete, checksummed envelope
//! (write-temp + rename means torn intermediates are structurally
//! impossible), and eviction racing writers never breaks either side.

use fmn_cache::{
    CacheClearAuthorization, CacheClearOutcome, CacheError, CacheKey, EvictOutcome, KeyBuilder,
    NamespacePolicy, Store, StoreConfig,
};
use fmn_hash::{Limits, Reader, Schema, UnknownPolicy};
use fmn_platform::clock::StdClock;
use fmn_platform::fs::{FileSystem, StdFs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

/// The entry-envelope schema, re-declared from outside the crate: raw disk
/// bytes are validated against the *published* format, so an accidental
/// format change breaks this test deliberately.
const ENTRY_SCHEMA: Schema = Schema::new(*b"FMNC", 1, 1, 0);

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cache_{name}"));
    // Re-runnable: previous runs' state would perturb the assertions.
    let _ = StdFs.remove_dir_all(&dir);
    dir
}

fn open_result(root: &std::path::Path) -> Result<Store, CacheError> {
    Store::open(
        Arc::new(StdFs),
        Arc::new(StdClock::new()),
        root.to_path_buf(),
        StoreConfig::default(),
    )
}

fn open(root: &std::path::Path) -> Store {
    open_result(root).expect("open store")
}

fn key(i: usize) -> CacheKey {
    KeyBuilder::new("torture")
        .push_u64(i as u64)
        .finish()
        .expect("key")
}

/// The deterministic payload for key `i`: any cross-key contamination shows
/// up as a value mismatch even before checksums fire.
fn payload(i: usize) -> Vec<u8> {
    let mut v = vec![0u8; 64 + i];
    for (j, b) in v.iter_mut().enumerate() {
        *b = (i.wrapping_mul(31).wrapping_add(j) & 0xff) as u8;
    }
    v
}

#[test]
fn clear_retains_owned_root_and_stale_handles_only_recreate_fresh_content() {
    let root = scratch("clear_lifecycle");
    let sibling = root.with_extension("sibling");
    std::fs::write(&sibling, b"keep").expect("sibling sentinel");
    let store = open(&root);
    let namespace = store
        .namespace("shared", 1, NamespacePolicy::default())
        .expect("namespace");
    let k = key(7);
    namespace.put(&k, &payload(7)).expect("seed cache");

    let authorization = CacheClearAuthorization::authorize(&root).expect("owned root authorizes");
    assert_eq!(authorization.root(), root);
    assert_eq!(
        authorization.clear().expect("clear"),
        CacheClearOutcome::Cleared
    );
    assert!(root.join("STORE_OWNER").is_file());
    assert!(root.join("STORE_FORMAT").is_file());
    assert_eq!(std::fs::read(&sibling).expect("sibling survives"), b"keep");
    assert_eq!(namespace.get(&k).expect("stale read is a miss"), None);

    namespace
        .put(&k, b"recreated")
        .expect("stale handle recreates only fresh ns content");
    let reopened = open(&root);
    let fresh = reopened
        .namespace("shared", 1, NamespacePolicy::default())
        .expect("fresh namespace");
    assert_eq!(
        fresh.get(&k).expect("fresh read").as_deref(),
        Some(&b"recreated"[..])
    );
}

#[test]
fn clear_migrates_an_owned_old_format_even_when_namespaces_are_absent() {
    let root = scratch("clear_empty_old_format");
    drop(open(&root));
    std::fs::write(root.join("STORE_FORMAT"), b"fmn-cache 999\n").expect("write old format");
    assert!(matches!(
        open_result(&root),
        Err(CacheError::FormatUnsupported { .. })
    ));

    assert_eq!(
        CacheClearAuthorization::authorize(&root)
            .expect("owned old-format root authorizes")
            .clear()
            .expect("clear migrates the format"),
        CacheClearOutcome::AlreadyAbsent
    );
    assert_eq!(
        std::fs::read(root.join("STORE_FORMAT")).expect("read refreshed format"),
        b"fmn-cache 1\n"
    );
    drop(open(&root));
}

#[test]
fn concurrent_first_openers_leave_one_valid_owned_root() {
    for attempt in 0..16 {
        let root = scratch(&format!("concurrent_first_open_{attempt}"));
        let barrier = Arc::new(Barrier::new(3));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                open_result(&root)
            }));
        }
        barrier.wait();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().expect("open thread"))
            .collect::<Vec<_>>();
        assert!(
            results.iter().any(Result::is_ok),
            "both first openers failed: {results:?}"
        );
        assert!(results.iter().all(|result| {
            result.is_ok() || matches!(result, Err(CacheError::RootRefused { .. }))
        }));
        let reopened = open(&root);
        assert_eq!(reopened.root(), root);
        assert!(root.join("STORE_OWNER").is_file());
        assert!(root.join("STORE_FORMAT").is_file());
    }
}

#[test]
fn concurrent_clearers_have_one_linearized_clear_and_one_miss() {
    let root = scratch("concurrent_clear");
    let store = open(&root);
    let namespace = store
        .namespace("shared", 1, NamespacePolicy::default())
        .expect("namespace");
    namespace.put(&key(3), &payload(3)).expect("seed cache");
    drop(namespace);
    drop(store);

    let first = CacheClearAuthorization::authorize(&root).expect("first authorization");
    let second = CacheClearAuthorization::authorize(&root).expect("second authorization");
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for authorization in [first, second] {
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            authorization.clear()
        }));
    }
    barrier.wait();
    let mut outcomes = handles
        .into_iter()
        .map(|handle| handle.join().expect("clear thread").expect("clear result"))
        .collect::<Vec<_>>();
    outcomes.sort_by_key(|outcome| match outcome {
        CacheClearOutcome::Cleared => 0,
        CacheClearOutcome::AlreadyAbsent => 1,
    });
    assert_eq!(
        outcomes,
        [CacheClearOutcome::Cleared, CacheClearOutcome::AlreadyAbsent]
    );
    assert!(root.join("STORE_OWNER").is_file());
    assert!(root.join("STORE_FORMAT").is_file());
}

#[test]
fn clear_racing_readers_and_writers_never_exposes_corruption() {
    const KEYS: usize = 12;
    const ROUNDS: usize = 32;

    let root = scratch("clear_racing_io");
    let store = open(&root);
    let namespace = Arc::new(
        store
            .namespace("shared", 1, NamespacePolicy::default())
            .expect("namespace"),
    );
    for i in 0..KEYS {
        namespace.put(&key(i), &payload(i)).expect("seed cache");
    }
    let authorization = CacheClearAuthorization::authorize(&root).expect("authorization");
    let barrier = Arc::new(Barrier::new(6));
    let mut handles = Vec::new();

    for writer in 0..4 {
        let namespace = Arc::clone(&namespace);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for round in 0..ROUNDS {
                let i = (round + writer * 5) % KEYS;
                let _ = namespace.put(&key(i), &payload(i));
            }
        }));
    }
    let reader_namespace = Arc::clone(&namespace);
    let reader_barrier = Arc::clone(&barrier);
    let reader = std::thread::spawn(move || {
        reader_barrier.wait();
        for round in 0..ROUNDS * 2 {
            let i = (round * 7) % KEYS;
            if let Some(found) = reader_namespace.get(&key(i)).expect("read is hit or miss") {
                assert_eq!(found, payload(i), "reader observed cross-key corruption");
            }
        }
    });

    barrier.wait();
    assert_eq!(
        authorization.clear().expect("clear"),
        CacheClearOutcome::Cleared
    );
    for handle in handles {
        handle.join().expect("writer thread");
    }
    reader.join().expect("reader thread");
    drop(namespace);
    drop(store);

    let reopened = open(&root);
    let fresh = reopened
        .namespace("shared", 1, NamespacePolicy::default())
        .expect("reopen namespace");
    for i in 0..KEYS {
        if let Some(found) = fresh.get(&key(i)).expect("reopened read") {
            assert_eq!(found, payload(i), "reopened cache contains corruption");
        }
    }
}

#[test]
fn clear_never_reuses_or_overwrites_a_guessed_quarantine_name() {
    let root = scratch("quarantine_collision");
    let store = open(&root);
    let namespace = store
        .namespace("shared", 1, NamespacePolicy::default())
        .expect("namespace");
    namespace.put(&key(4), &payload(4)).expect("seed cache");
    drop(namespace);
    drop(store);

    let mut occupied = Vec::new();
    for sequence in 1..=128 {
        let path = root.join(format!(".fmn-clear.{}.{}", std::process::id(), sequence));
        std::fs::create_dir(&path).expect("occupy guessed quarantine name");
        occupied.push(path);
    }

    assert_eq!(
        CacheClearAuthorization::authorize(&root)
            .expect("authorization")
            .clear()
            .expect("clear"),
        CacheClearOutcome::Cleared
    );
    assert!(
        occupied.iter().all(|path| path.is_dir()),
        "a pre-existing quarantine path was overwritten"
    );
}

#[test]
fn clear_revalidates_the_owner_marker_before_renaming_namespaces() {
    let root = scratch("owner_revalidation");
    let store = open(&root);
    let namespace = store
        .namespace("shared", 1, NamespacePolicy::default())
        .expect("namespace");
    let cache_key = key(5);
    namespace.put(&cache_key, &payload(5)).expect("seed cache");
    drop(namespace);
    drop(store);

    let authorization = CacheClearAuthorization::authorize(&root).expect("authorization");
    std::fs::write(root.join("STORE_OWNER"), b"foreign owner\n").expect("replace owner marker");
    assert!(matches!(
        authorization.clear(),
        Err(CacheError::RootRefused { .. })
    ));
    assert!(root.join("ns").is_dir(), "namespace tree must survive");
}

#[cfg(unix)]
#[test]
fn clear_removes_a_symlinked_namespace_without_following_its_target() {
    let root = scratch("symlinked_namespace");
    let victim = scratch("symlinked_namespace_victim");
    let store = open(&root);
    let namespace = store
        .namespace("shared", 1, NamespacePolicy::default())
        .expect("namespace");
    namespace.put(&key(6), &payload(6)).expect("seed cache");
    drop(namespace);
    drop(store);

    std::fs::remove_dir_all(root.join("ns")).expect("replace managed namespace tree");
    std::fs::create_dir_all(&victim).expect("create sentinel target");
    let sentinel = victim.join("important.txt");
    std::fs::write(&sentinel, b"keep").expect("write sentinel");
    std::os::unix::fs::symlink(&victim, root.join("ns")).expect("symlink managed namespace");

    assert_eq!(
        CacheClearAuthorization::authorize(&root)
            .expect("authorization")
            .clear()
            .expect("clear"),
        CacheClearOutcome::Cleared
    );
    assert_eq!(std::fs::read(&sentinel).expect("target survives"), b"keep");
    assert!(!root.join("ns").exists());
}

#[test]
fn clear_authorization_refuses_dangerous_and_foreign_roots() {
    for dangerous in [
        PathBuf::from(std::path::MAIN_SEPARATOR.to_string()),
        std::env::current_dir().expect("cwd"),
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .expect("test home"),
    ] {
        assert!(
            matches!(
                CacheClearAuthorization::authorize(&dangerous),
                Err(CacheError::RootRefused { .. })
            ),
            "dangerous root authorized: {}",
            dangerous.display()
        );
    }

    let foreign = scratch("foreign_root");
    std::fs::create_dir_all(&foreign).expect("foreign dir");
    std::fs::write(foreign.join("important.txt"), b"keep").expect("foreign sentinel");
    assert!(matches!(
        CacheClearAuthorization::authorize(&foreign),
        Err(CacheError::RootRefused { .. })
    ));
    assert_eq!(
        std::fs::read(foreign.join("important.txt")).expect("foreign data survives"),
        b"keep"
    );

    let copied_from = scratch("copied_owner_source");
    drop(open(&copied_from));
    let copied_to = scratch("copied_owner_target");
    std::fs::create_dir(&copied_to).expect("copied-marker target");
    std::fs::copy(
        copied_from.join("STORE_OWNER"),
        copied_to.join("STORE_OWNER"),
    )
    .expect("copy owner marker");
    std::fs::copy(
        copied_from.join("STORE_FORMAT"),
        copied_to.join("STORE_FORMAT"),
    )
    .expect("copy format marker");
    std::fs::write(copied_to.join("important.txt"), b"keep").expect("copied-root sentinel");
    assert!(matches!(
        CacheClearAuthorization::authorize(&copied_to),
        Err(CacheError::RootRefused { .. })
    ));
    assert_eq!(
        std::fs::read(copied_to.join("important.txt")).expect("copied-root sentinel survives"),
        b"keep"
    );

    let empty_foreign = scratch("empty_foreign_root");
    std::fs::create_dir_all(&empty_foreign).expect("empty foreign dir");
    assert!(matches!(
        open_result(&empty_foreign),
        Err(CacheError::RootRefused { .. })
    ));
    assert!(empty_foreign.is_dir());
    assert!(
        std::fs::read_dir(&empty_foreign)
            .expect("empty foreign root remains readable")
            .next()
            .is_none()
    );

    assert!(matches!(
        CacheClearAuthorization::authorize("relative/cache"),
        Err(CacheError::RootRefused { .. })
    ));

    let absent = scratch("absent_clear_root");
    assert_eq!(
        CacheClearAuthorization::authorize(&absent)
            .expect("absent root is a non-creating authorization")
            .clear()
            .expect("absent clear"),
        CacheClearOutcome::AlreadyAbsent
    );
    assert!(!absent.exists(), "absent clear must not create its target");

    let missing_parent = scratch("missing_store_parent");
    let nested = missing_parent.join("cache");
    assert!(matches!(
        open_result(&nested),
        Err(CacheError::RootRefused { .. })
    ));
    assert!(
        !missing_parent.exists(),
        "a refused missing parent must not be created"
    );
}

#[cfg(unix)]
#[test]
fn clear_authorization_refuses_a_symlinked_root() {
    let root = scratch("symlink_target");
    let _store = open(&root);
    let alias = root.with_extension("alias");
    let _ = std::fs::remove_file(&alias);
    std::os::unix::fs::symlink(&root, &alias).expect("symlink alias");
    assert!(matches!(
        CacheClearAuthorization::authorize(&alias),
        Err(CacheError::RootRefused { .. })
    ));
    match open_result(&alias) {
        Err(CacheError::RootRefused { reason, .. }) => {
            assert!(reason.contains("symlinked path component"));
        }
        other => panic!("expected Store::open symlink refusal, got {other:?}"),
    }

    let real_parent = scratch("symlink_ancestor_parent");
    std::fs::create_dir(&real_parent).expect("create real parent");
    let nested_root = real_parent.join("cache");
    let _store = open(&nested_root);
    let parent_alias = real_parent.with_extension("alias");
    let _ = std::fs::remove_file(&parent_alias);
    std::os::unix::fs::symlink(&real_parent, &parent_alias).expect("symlink parent alias");
    match CacheClearAuthorization::authorize(parent_alias.join("cache")) {
        Err(CacheError::RootRefused { reason, .. }) => {
            assert!(reason.contains("symlinked path component"));
        }
        other => panic!("expected symlink-ancestor refusal, got {other:?}"),
    }
    match open_result(&parent_alias.join("new-cache")) {
        Err(CacheError::RootRefused { reason, .. }) => {
            assert!(reason.contains("symlinked path component"));
        }
        other => panic!("expected Store::open symlink-ancestor refusal, got {other:?}"),
    }
}

#[test]
fn two_stores_many_threads_no_observer_ever_sees_corruption() {
    const KEYS: usize = 24;
    const ROUNDS: usize = 120;

    let root = scratch("torture");
    let store_a = open(&root);
    let store_b = open(&root);
    let policy = NamespacePolicy {
        // Small enough that eviction churns constantly under the writers.
        ceiling_bytes: Some(6 * 1024),
    };
    let ns_a = Arc::new(store_a.namespace("shared", 1, policy).unwrap());
    let ns_b = Arc::new(store_b.namespace("shared", 1, policy).unwrap());

    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(7));
    let mut handles = Vec::new();

    // Four writers, two per store opening, all cycling over the same keys.
    for (w, ns) in [(0, &ns_a), (1, &ns_a), (2, &ns_b), (3, &ns_b)] {
        let ns = Arc::clone(ns);
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            for round in 0..ROUNDS {
                let i = (round * 7 + w * 3) % KEYS;
                // Storage failures under racing eviction are legal (a lost
                // cache write is a future recompute); corruption is not.
                let _ = ns.put(&key(i), &payload(i));
            }
        }));
    }

    // Two readers: every hit must be the exact expected payload.
    for ns in [&ns_a, &ns_b] {
        let ns = Arc::clone(ns);
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            while !stop.load(Ordering::Relaxed) {
                for i in 0..KEYS {
                    match ns.get(&key(i)) {
                        Ok(Some(v)) => {
                            assert_eq!(v, payload(i), "cross-contamination at key {i}");
                        }
                        Ok(None) => {}
                        Err(err) => panic!("reader hit a hard error: {err}"),
                    }
                }
            }
        }));
    }

    // One maintainer per… one is plenty; the second store's maintainer runs
    // implicitly via the skip path in the sibling test below. Here: evict in
    // a loop while writers churn.
    {
        let ns = Arc::clone(&ns_a);
        let barrier = Arc::clone(&barrier);
        let stop = Arc::clone(&stop);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            while !stop.load(Ordering::Relaxed) {
                match ns.evict_to_ceiling() {
                    Ok(EvictOutcome::Done(_) | EvictOutcome::SkippedLockHeld) => {}
                    Ok(EvictOutcome::Unlimited) => panic!("policy has a ceiling"),
                    Err(err) => panic!("maintainer hit a hard error: {err}"),
                }
            }
        }));
    }

    // Writers finish first; then release the loopers.
    let mut writer_handles = handles;
    let looper_handles = writer_handles.split_off(4);
    for h in writer_handles {
        h.join().expect("writer thread");
    }
    stop.store(true, Ordering::Relaxed);
    for h in looper_handles {
        h.join().expect("looper thread");
    }

    // Post-conditions. Every raw object file on disk is a complete, valid
    // envelope — write-temp + rename left nothing torn, eviction left no
    // half-deleted state.
    let objects_dir = root.join("ns/shared/v1/objects");
    let fs = StdFs;
    if fs.exists(&objects_dir) {
        for shard in fs.list_dir(&objects_dir).expect("list shards") {
            for file in fs.list_dir(&shard).expect("list shard") {
                let name = file.file_name().unwrap().to_string_lossy().into_owned();
                if name.starts_with(".fmn-") {
                    // In-flight temp from the final instants of the run;
                    // invisible to the store, swept by future maintenance.
                    continue;
                }
                let bytes = fs.read(&file).expect("read object");
                let mut r =
                    Reader::open(&bytes, ENTRY_SCHEMA, Limits::DEFAULT, UnknownPolicy::Strict)
                        .unwrap_or_else(|err| panic!("torn or corrupt object {name}: {err}"));
                let _kind = r.get_u8().expect("kind");
                let _address = r.get_digest().expect("address");
                let _payload = r.get_bytes().expect("payload");
                r.finish().expect("clean tail");
            }
        }
    }

    // And the store still works end-to-end from both openings.
    for i in 0..KEYS {
        ns_a.put(&key(i), &payload(i)).expect("final put");
    }
    for i in 0..KEYS {
        assert_eq!(
            ns_b.get(&key(i)).expect("final get").as_deref(),
            Some(payload(i).as_slice()),
            "opening B reads what opening A wrote"
        );
    }
}

#[test]
fn same_key_racing_writers_last_wins_and_readers_see_whole_values() {
    const ROUNDS: usize = 200;

    let root = scratch("same_key");
    let store_a = open(&root);
    let store_b = open(&root);
    let ns_a = Arc::new(
        store_a
            .namespace("race", 1, NamespacePolicy::default())
            .unwrap(),
    );
    let ns_b = Arc::new(
        store_b
            .namespace("race", 1, NamespacePolicy::default())
            .unwrap(),
    );

    let k = key(0);
    let value_a = payload(1);
    let value_b = payload(2);

    let stop = Arc::new(AtomicBool::new(false));
    let barrier = Arc::new(Barrier::new(3));

    let wa = {
        let ns = Arc::clone(&ns_a);
        let (k, v, barrier) = (k, value_a.clone(), Arc::clone(&barrier));
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..ROUNDS {
                ns.put(&k, &v).expect("put a");
            }
        })
    };
    let wb = {
        let ns = Arc::clone(&ns_b);
        let (k, v, barrier) = (k, value_b.clone(), Arc::clone(&barrier));
        std::thread::spawn(move || {
            barrier.wait();
            for _ in 0..ROUNDS {
                ns.put(&k, &v).expect("put b");
            }
        })
    };
    let reader = {
        let ns = Arc::clone(&ns_a);
        let (k, va, vb) = (k, value_a.clone(), value_b.clone());
        let (barrier, stop) = (Arc::clone(&barrier), Arc::clone(&stop));
        std::thread::spawn(move || {
            barrier.wait();
            while !stop.load(Ordering::Relaxed) {
                match ns.get(&k) {
                    Ok(Some(v)) => {
                        assert!(v == va || v == vb, "reader saw a torn or mixed value");
                    }
                    Ok(None) => {}
                    Err(err) => panic!("reader hit a hard error: {err}"),
                }
            }
        })
    };

    wa.join().expect("writer a");
    wb.join().expect("writer b");
    stop.store(true, Ordering::Relaxed);
    reader.join().expect("reader");

    // Last writer won with a complete value.
    let last = ns_b.get(&k).expect("final get").expect("present");
    assert!(last == value_a || last == value_b);
}
