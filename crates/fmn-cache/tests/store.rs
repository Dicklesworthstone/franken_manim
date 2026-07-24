//! The fm-fw6 acceptance suite, deterministic half: every store behavior over
//! [`VirtualFs`] + [`FakeClock`] in the deterministic lab — atomicity under
//! simulated crashes, corruption injection, eviction/ceiling/pinning, the
//! cross-process advisory lock with staleness breaking, versioned-namespace
//! invalidation, cold-vs-warm equivalence, and the key-traversal fuzz.
//!
//! The host-filesystem half (real `StdFs`, threads, two store openings on one
//! directory) lives in `torture.rs`.

use fmn_cache::{
    CacheError, CacheKey, EvictOutcome, KeyBuilder, Namespace, NamespacePolicy, Store, StoreConfig,
};
use fmn_hash::sha256;
use fmn_platform::clock::{Clock, FakeClock};
use fmn_platform::fs::{FileSystem, FsError, VirtualFs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const ROOT: &str = "/cache";

fn open_store(fs: Arc<dyn FileSystem>, clock: Arc<dyn Clock>) -> Store {
    Store::open(fs, clock, ROOT, StoreConfig::default()).expect("open store")
}

fn fresh() -> (Arc<VirtualFs>, Arc<FakeClock>, Store) {
    let fs = Arc::new(VirtualFs::new());
    let clock = Arc::new(FakeClock::new());
    let store = open_store(fs.clone(), clock.clone());
    (fs, clock, store)
}

fn ns(store: &Store, ceiling: Option<u64>) -> Namespace {
    store
        .namespace(
            "t",
            1,
            NamespacePolicy {
                ceiling_bytes: ceiling,
            },
        )
        .expect("namespace")
}

fn key(material: &str) -> CacheKey {
    KeyBuilder::new("test")
        .push_str(material)
        .finish()
        .expect("key")
}

/// Every file under the store root, for structural assertions.
fn files_under(fs: &VirtualFs, root: &Path) -> Vec<PathBuf> {
    // VirtualFs is a flat map with implicit directories; walk it via list_dir.
    fn walk(fs: &VirtualFs, dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(children) = fs.list_dir(dir) {
            for child in children {
                if fs.read(&child).is_ok() {
                    out.push(child);
                } else {
                    walk(fs, &child, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(fs, root, &mut out);
    out
}

// ---------------------------------------------------------------------------
// Round trips, misses, blobs
// ---------------------------------------------------------------------------

#[test]
fn keyed_round_trip_and_miss() {
    let (_fs, _clock, store) = fresh();
    let n = ns(&store, None);
    let k = key("alpha");
    assert_eq!(n.get(&k).unwrap(), None, "cold store misses");
    n.put(&k, b"typeset result").unwrap();
    assert_eq!(n.get(&k).unwrap().as_deref(), Some(&b"typeset result"[..]));
    assert_eq!(n.get(&key("beta")).unwrap(), None, "other keys still miss");
}

#[test]
fn blob_round_trip_is_self_addressed() {
    let (_fs, _clock, store) = fresh();
    let n = ns(&store, None);
    let addr = n.put_blob(b"asset bytes").unwrap();
    assert_eq!(addr, sha256(b"asset bytes"), "address is the content hash");
    assert_eq!(
        n.get_blob(&addr).unwrap().as_deref(),
        Some(&b"asset bytes"[..])
    );
    // A keyed lookup at a blob address is a kind mismatch → corrupt → miss.
    assert_eq!(n.get(&CacheKey::from_digest(addr)).unwrap(), None);
    // …and that lookup evicted the mismatched entry (never trusted).
    assert_eq!(n.get_blob(&addr).unwrap(), None);
}

#[test]
fn oversized_entries_are_a_precise_refusal() {
    let fs: Arc<dyn FileSystem> = Arc::new(VirtualFs::new());
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new());
    let store = Store::open(
        fs,
        clock,
        ROOT,
        StoreConfig {
            max_entry_bytes: 16,
            ..StoreConfig::default()
        },
    )
    .unwrap();
    let n = ns(&store, None);
    match n.put(&key("big"), &[0u8; 17]) {
        Err(CacheError::EntryTooLarge {
            limit: 16,
            needed: 17,
        }) => {}
        other => panic!("expected EntryTooLarge, got {other:?}"),
    }
    // The refusal cached nothing and poisoned nothing.
    assert_eq!(n.get(&key("big")).unwrap(), None);
    n.put(&key("small"), &[0u8; 16]).unwrap();
}

// ---------------------------------------------------------------------------
// Cold-vs-warm equivalence (the determinism interaction)
// ---------------------------------------------------------------------------

#[test]
fn cold_and_warm_get_or_compute_are_identical() {
    let (_fs, _clock, store) = fresh();
    let n = ns(&store, None);
    let k = key("formula");
    let computed = AtomicBool::new(false);
    let cold: Vec<u8> = n
        .get_or_compute(&k, || {
            computed.store(true, Ordering::Relaxed);
            Ok::<_, std::convert::Infallible>(b"layout bytes".to_vec())
        })
        .unwrap();
    assert!(computed.load(Ordering::Relaxed), "cold path computes");

    computed.store(false, Ordering::Relaxed);
    let warm: Vec<u8> = n
        .get_or_compute(&k, || {
            computed.store(true, Ordering::Relaxed);
            Ok::<_, std::convert::Infallible>(b"layout bytes".to_vec())
        })
        .unwrap();
    assert!(!computed.load(Ordering::Relaxed), "warm path hits");
    assert_eq!(
        cold, warm,
        "a hit is definitionally equivalent to a recompute"
    );
}

#[test]
fn compute_errors_surface_and_cache_failures_do_not() {
    let (_fs, _clock, store) = fresh();
    let n = ns(&store, None);
    // The compute error is the only error that escapes.
    let err = n
        .get_or_compute(&key("failing"), || Err::<Vec<u8>, &str>("typeset failed"))
        .unwrap_err();
    assert_eq!(err, "typeset failed");
}

/// A filesystem that can be switched read-only: writes fail, reads pass
/// through. `get_or_compute` must still deliver the computed value.
struct ReadOnlyFs {
    inner: VirtualFs,
    frozen: AtomicBool,
}

impl ReadOnlyFs {
    fn deny(path: &Path) -> FsError {
        FsError::Io {
            path: path.to_path_buf(),
            err: std::io::Error::other("simulated read-only filesystem"),
        }
    }
}

impl FileSystem for ReadOnlyFs {
    fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        self.inner.read(path)
    }
    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), FsError> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(Self::deny(path));
        }
        self.inner.write_atomic(path, bytes)
    }
    fn create_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, FsError> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(Self::deny(path));
        }
        self.inner.create_new(path, bytes)
    }
    fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(Self::deny(path));
        }
        self.inner.remove_file(path)
    }
    fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        if self.frozen.load(Ordering::Relaxed) {
            return Err(Self::deny(path));
        }
        self.inner.remove_dir_all(path)
    }
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        self.inner.list_dir(path)
    }
}

#[test]
fn a_cache_that_cannot_write_never_fails_the_consumer() {
    let fs = Arc::new(ReadOnlyFs {
        inner: VirtualFs::new(),
        frozen: AtomicBool::new(false),
    });
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new());
    let store = open_store(fs.clone(), clock);
    let n = ns(&store, None);
    fs.frozen.store(true, Ordering::Relaxed);
    // put fails precisely…
    assert!(n.put(&key("k"), b"v").is_err());
    // …but the read-through contract still delivers the computed value.
    let out: Vec<u8> = n
        .get_or_compute(&key("k"), || {
            Ok::<_, std::convert::Infallible>(b"computed".to_vec())
        })
        .unwrap();
    assert_eq!(out, b"computed");
}

// ---------------------------------------------------------------------------
// Atomicity under simulated crashes (kill -9 mid-write)
// ---------------------------------------------------------------------------

/// Simulates a writer killed mid-`write_atomic`, at both crash points the
/// temp-then-rename protocol allows: before anything persists, or with an
/// orphaned temp left in the shard directory (the rename never happened).
struct CrashingFs {
    inner: VirtualFs,
    crash_writes: AtomicBool,
    leave_orphan: AtomicBool,
}

impl FileSystem for CrashingFs {
    fn read(&self, path: &Path) -> Result<Vec<u8>, FsError> {
        self.inner.read(path)
    }
    fn write_atomic(&self, path: &Path, bytes: &[u8]) -> Result<(), FsError> {
        if self.crash_writes.load(Ordering::Relaxed) {
            if self.leave_orphan.load(Ordering::Relaxed)
                && let Some(parent) = path.parent()
            {
                // The killed process persisted its unique temp but never
                // renamed it into place.
                self.inner.insert(
                    parent.join(".fmn-tmp.12345.orphan"),
                    bytes[..bytes.len() / 2].to_vec(),
                );
            }
            return Err(FsError::Io {
                path: path.to_path_buf(),
                err: std::io::Error::other("simulated kill -9 mid-write"),
            });
        }
        self.inner.write_atomic(path, bytes)
    }
    fn create_new(&self, path: &Path, bytes: &[u8]) -> Result<bool, FsError> {
        self.inner.create_new(path, bytes)
    }
    fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        self.inner.remove_file(path)
    }
    fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        self.inner.remove_dir_all(path)
    }
    fn exists(&self, path: &Path) -> bool {
        self.inner.exists(path)
    }
    fn list_dir(&self, path: &Path) -> Result<Vec<PathBuf>, FsError> {
        self.inner.list_dir(path)
    }
}

#[test]
fn a_killed_writer_leaves_a_consistent_store() {
    let fs = Arc::new(CrashingFs {
        inner: VirtualFs::new(),
        crash_writes: AtomicBool::new(false),
        leave_orphan: AtomicBool::new(false),
    });
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new());
    let store = open_store(fs.clone(), clock);
    let n = store
        .namespace(
            "t",
            1,
            NamespacePolicy {
                ceiling_bytes: Some(1 << 20),
            },
        )
        .unwrap();
    let stable = key("stable");
    n.put(&stable, b"old value").unwrap();

    // Crash point A: nothing persisted. The old entry is untouched, the new
    // key is a miss, and the put reported its failure.
    fs.crash_writes.store(true, Ordering::Relaxed);
    assert!(n.put(&stable, b"new value").is_err());
    assert!(n.put(&key("fresh"), b"v").is_err());
    fs.crash_writes.store(false, Ordering::Relaxed);
    assert_eq!(n.get(&stable).unwrap().as_deref(), Some(&b"old value"[..]));
    assert_eq!(n.get(&key("fresh")).unwrap(), None);

    // Crash point B: an orphaned temp file in a shard directory. Reads are
    // unaffected; the next eviction pass sweeps it.
    fs.crash_writes.store(true, Ordering::Relaxed);
    fs.leave_orphan.store(true, Ordering::Relaxed);
    assert!(n.put(&key("fresh"), b"v").is_err());
    fs.crash_writes.store(false, Ordering::Relaxed);
    assert_eq!(n.get(&stable).unwrap().as_deref(), Some(&b"old value"[..]));
    let report = match n.evict_to_ceiling().unwrap() {
        EvictOutcome::Done(report) => report,
        other => panic!("expected a pass, got {other:?}"),
    };
    assert_eq!(report.swept_unrecognized, 1, "the orphan was reclaimed");
    assert_eq!(report.evicted, 0);
    // After recovery the store works normally.
    n.put(&key("fresh"), b"v").unwrap();
    assert_eq!(n.get(&key("fresh")).unwrap().as_deref(), Some(&b"v"[..]));
}

// ---------------------------------------------------------------------------
// Corruption injection
// ---------------------------------------------------------------------------

#[test]
fn flipped_bytes_are_detected_evicted_and_recomputed() {
    let (fs, _clock, store) = fresh();
    let n = ns(&store, None);
    let k = key("victim");
    n.put(&k, b"good value").unwrap();

    // Find the one object file and flip a byte in the middle.
    let objects: Vec<PathBuf> = files_under(&fs, Path::new(ROOT))
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("/objects/"))
        .collect();
    assert_eq!(objects.len(), 1);
    let victim_path = &objects[0];
    let mut bytes = fs.read(victim_path).unwrap();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0x40;
    fs.write_atomic(victim_path, &bytes).unwrap();

    // Detected → evicted (file gone) → miss, never an error.
    assert_eq!(n.get(&k).unwrap(), None, "corrupt entry reads as a miss");
    assert!(!fs.exists(victim_path), "corrupt entry was evicted");

    // The recompute path repopulates cleanly.
    let out: Vec<u8> = n
        .get_or_compute(&k, || {
            Ok::<_, std::convert::Infallible>(b"good value".to_vec())
        })
        .unwrap();
    assert_eq!(out, b"good value");
    assert_eq!(n.get(&k).unwrap().as_deref(), Some(&b"good value"[..]));
}

#[test]
fn truncation_and_garbage_files_are_misses() {
    let (fs, _clock, store) = fresh();
    let n = ns(&store, None);
    let k = key("victim");
    n.put(&k, b"value").unwrap();
    let objects: Vec<PathBuf> = files_under(&fs, Path::new(ROOT))
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("/objects/"))
        .collect();
    let victim_path = &objects[0];

    // Truncated file.
    let bytes = fs.read(victim_path).unwrap();
    fs.write_atomic(victim_path, &bytes[..bytes.len() - 7])
        .unwrap();
    assert_eq!(n.get(&k).unwrap(), None);
    assert!(!fs.exists(victim_path));

    // Arbitrary garbage at the right path.
    n.put(&k, b"value").unwrap();
    fs.write_atomic(victim_path, b"not an envelope at all")
        .unwrap();
    assert_eq!(n.get(&k).unwrap(), None);
    assert!(!fs.exists(victim_path));
}

#[test]
fn a_valid_envelope_at_the_wrong_address_is_corrupt() {
    let (fs, _clock, store) = fresh();
    let n = ns(&store, None);
    let a = key("a");
    let b = key("b");
    n.put(&a, b"value of a").unwrap();
    n.put(&b, b"value of b").unwrap();
    let objects: Vec<PathBuf> = files_under(&fs, Path::new(ROOT))
        .into_iter()
        .filter(|p| p.to_string_lossy().contains("/objects/"))
        .collect();
    assert_eq!(objects.len(), 2);
    // Copy a's bytes over b's file: the envelope is pristine but its recorded
    // address is a's, so reading b must reject it.
    let a_hex = a.digest().to_hex();
    let (a_path, b_path) = if objects[0].to_string_lossy().contains(&a_hex[2..]) {
        (&objects[0], &objects[1])
    } else {
        (&objects[1], &objects[0])
    };
    let a_bytes = fs.read(a_path).unwrap();
    fs.write_atomic(b_path, &a_bytes).unwrap();
    assert_eq!(n.get(&b).unwrap(), None, "mis-placed entry is a miss");
    assert!(!fs.exists(b_path), "mis-placed entry was evicted");
    assert_eq!(n.get(&a).unwrap().as_deref(), Some(&b"value of a"[..]));
}

// ---------------------------------------------------------------------------
// Eviction, ceilings, pinning, LRU order
// ---------------------------------------------------------------------------

/// Entry envelope overhead is fixed; measure it once so ceiling arithmetic in
/// the tests is exact rather than magic.
fn envelope_size(store: &Store, payload_len: usize) -> u64 {
    let n = store
        .namespace("sizing", 1, NamespacePolicy::default())
        .unwrap();
    n.put(&key("probe"), &vec![0u8; payload_len]).unwrap();
    n.usage().unwrap()
}

#[test]
fn eviction_is_lru_ordered_and_respects_the_ceiling() {
    let (_fs, _clock, store) = fresh();
    let per_entry = envelope_size(&store, 8);
    // Room for exactly three entries.
    let n = ns(&store, Some(3 * per_entry));
    let keys: Vec<CacheKey> = (0..5).map(|i| key(&format!("k{i}"))).collect();
    for k in &keys {
        n.put(k, &[7u8; 8]).unwrap();
    }
    // Touch k0 and k1 so k2 becomes the least recently used.
    assert!(n.get(&keys[0]).unwrap().is_some());
    assert!(n.get(&keys[1]).unwrap().is_some());

    let report = match n.evict_to_ceiling().unwrap() {
        EvictOutcome::Done(report) => report,
        other => panic!("expected a pass, got {other:?}"),
    };
    assert_eq!(report.examined, 5);
    assert_eq!(report.evicted, 2);
    assert_eq!(report.evicted_bytes, 2 * per_entry);
    assert_eq!(report.retained_bytes, 3 * per_entry);
    assert!(report.retained_bytes <= 3 * per_entry, "ceiling honored");

    // The least-recently-used entries (k2, k3 — put early, never re-touched)
    // are the ones gone; the touched and the freshest survive.
    assert_eq!(n.get(&keys[2]).unwrap(), None);
    assert_eq!(n.get(&keys[3]).unwrap(), None);
    assert!(n.get(&keys[0]).unwrap().is_some());
    assert!(n.get(&keys[1]).unwrap().is_some());
    assert!(n.get(&keys[4]).unwrap().is_some());
}

#[test]
fn pinned_entries_survive_eviction_until_unpinned() {
    let (_fs, _clock, store) = fresh();
    let per_entry = envelope_size(&store, 8);
    let n = ns(&store, Some(2 * per_entry));
    let oldest = key("oldest");
    n.put(&oldest, &[1u8; 8]).unwrap();
    for i in 0..3 {
        n.put(&key(&format!("younger{i}")), &[2u8; 8]).unwrap();
    }

    // The oldest entry is LRU-first, but a pin holds it in place.
    let pin = n.pin(&oldest);
    let report = match n.evict_to_ceiling().unwrap() {
        EvictOutcome::Done(report) => report,
        other => panic!("expected a pass, got {other:?}"),
    };
    assert!(
        report.skipped_pinned >= 1,
        "the pin was honored: {report:?}"
    );
    assert!(n.get(&oldest).unwrap().is_some(), "pinned entry survived");

    // Unpinned, the same entry is evictable again. Its recent get bumped it,
    // so age it below fresher writes first.
    drop(pin);
    for i in 0..4 {
        n.put(&key(&format!("newest{i}")), &[3u8; 8]).unwrap();
    }
    match n.evict_to_ceiling().unwrap() {
        EvictOutcome::Done(report) => {
            assert_eq!(report.skipped_pinned, 0);
            assert!(report.evicted > 0);
        }
        other => panic!("expected a pass, got {other:?}"),
    }
    assert_eq!(n.get(&oldest).unwrap(), None, "unpinned entry evicted");
}

#[test]
fn manual_policy_never_evicts() {
    let (_fs, _clock, store) = fresh();
    let n = ns(&store, None); // the journal-namespace policy
    for i in 0..10 {
        n.put(&key(&format!("segment{i}")), &[0u8; 128]).unwrap();
    }
    assert!(matches!(
        n.evict_to_ceiling().unwrap(),
        EvictOutcome::Unlimited
    ));
    for i in 0..10 {
        assert!(n.get(&key(&format!("segment{i}"))).unwrap().is_some());
    }
}

#[test]
fn a_lost_index_degrades_lru_never_correctness() {
    let (fs, _clock, store) = fresh();
    let per_entry = envelope_size(&store, 8);
    let n = ns(&store, Some(2 * per_entry));
    for i in 0..4 {
        n.put(&key(&format!("k{i}")), &[9u8; 8]).unwrap();
    }
    // Retire the handle first (drop flushes bookkeeping), then destroy the
    // index outright.
    drop(n);
    let index_path = Path::new(ROOT).join("ns/t/v1/index");
    assert!(fs.exists(&index_path), "index exists after puts");
    fs.remove_file(&index_path).unwrap();

    // A fresh handle (no index, no in-memory log) must still evict to the
    // ceiling by rebuilding from disk truth.
    let n2 = ns(&store, Some(2 * per_entry));
    let report = match n2.evict_to_ceiling().unwrap() {
        EvictOutcome::Done(report) => report,
        other => panic!("expected a pass, got {other:?}"),
    };
    assert_eq!(report.examined, 4);
    assert_eq!(report.evicted, 2);
    assert!(report.retained_bytes <= 2 * per_entry);
    // Every surviving entry still round-trips (correctness intact).
    let survivors = (0..4)
        .filter(|i| n2.get(&key(&format!("k{i}"))).unwrap().is_some())
        .count();
    assert_eq!(survivors, 2);
}

// ---------------------------------------------------------------------------
// The advisory maintenance lock
// ---------------------------------------------------------------------------

/// A lock token as another fmn process would write it (the on-disk lock
/// format is deliberately exercised from outside the crate: a change to it
/// must be a conscious, test-breaking act).
fn foreign_lock_token(wall_nanos: u64) -> Vec<u8> {
    let mut w = fmn_hash::Writer::new(fmn_hash::Schema::new(*b"FMNC", 4, 1, 0));
    w.put_u64(99_999); // the foreign process id
    w.put_u64(7); // its store-instance id
    w.put_u64(wall_nanos); // when it acquired
    w.finish().expect("lock token")
}

#[test]
fn a_fresh_foreign_lock_skips_and_a_stale_one_is_broken() {
    let fs = Arc::new(VirtualFs::new());
    let clock = Arc::new(FakeClock::new());
    // Advance off the epoch so lock timestamps are nonzero.
    clock.advance(Duration::from_secs(1000));
    let store = open_store(fs.clone(), clock.clone());
    let per_entry = envelope_size(&store, 8);
    let n = ns(&store, Some(per_entry));
    n.put(&key("a"), &[0u8; 8]).unwrap();
    n.put(&key("b"), &[0u8; 8]).unwrap();

    // Another fmn invocation holds a fresh lock: this maintainer skips —
    // non-blocking, no waiting, no corruption.
    let lock_path = Path::new(ROOT).join("ns/t/v1/lock");
    let now_nanos = 1000u64 * 1_000_000_000;
    fs.write_atomic(&lock_path, &foreign_lock_token(now_nanos))
        .unwrap();
    assert!(matches!(
        n.evict_to_ceiling().unwrap(),
        EvictOutcome::SkippedLockHeld
    ));
    assert!(fs.exists(&lock_path), "a fresh foreign lock is left alone");

    // Time passes beyond the staleness horizon (the holder crashed): the
    // next maintainer breaks the lock and completes the pass.
    clock.advance(Duration::from_secs(120));
    match n.evict_to_ceiling().unwrap() {
        EvictOutcome::Done(report) => assert_eq!(report.evicted, 1),
        other => panic!("expected a pass after breaking stale lock, got {other:?}"),
    }
    assert!(!fs.exists(&lock_path), "the pass released its own lock");
}

#[test]
fn an_unparseable_lock_is_treated_as_abandoned() {
    let (fs, _clock, store) = fresh();
    let per_entry = envelope_size(&store, 8);
    let n = ns(&store, Some(per_entry));
    n.put(&key("a"), &[0u8; 8]).unwrap();
    n.put(&key("b"), &[0u8; 8]).unwrap();
    // Garbage where the lock token should be: it can never renew or expire,
    // so it must not deadlock maintenance forever.
    fs.write_atomic(&Path::new(ROOT).join("ns/t/v1/lock"), b"garbage")
        .unwrap();
    match n.evict_to_ceiling().unwrap() {
        EvictOutcome::Done(report) => assert_eq!(report.evicted, 1),
        other => panic!("expected a pass, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Versioned namespaces
// ---------------------------------------------------------------------------

#[test]
fn a_version_bump_is_a_cold_start_and_purges_only_its_own_stale_versions() {
    let (fs, _clock, store) = fresh();
    let k = key("entry");

    let v1 = store.namespace("t", 1, NamespacePolicy::default()).unwrap();
    v1.put(&k, b"v1 value").unwrap();
    let unrelated = store
        .namespace("other", 1, NamespacePolicy::default())
        .unwrap();
    unrelated.put(&k, b"unrelated value").unwrap();
    drop(v1);

    // The bump: v2 is cold, and opening it reclaimed the abandoned v1.
    let v2 = store.namespace("t", 2, NamespacePolicy::default()).unwrap();
    assert_eq!(
        v2.get(&k).unwrap(),
        None,
        "version bump invalidates cleanly"
    );
    assert!(
        !fs.exists(&Path::new(ROOT).join("ns/t/v1")),
        "stale version reclaimed on open"
    );
    // The unrelated namespace was never touched.
    assert_eq!(
        unrelated.get(&k).unwrap().as_deref(),
        Some(&b"unrelated value"[..])
    );
    // And v2 fills independently.
    v2.put(&k, b"v2 value").unwrap();
    assert_eq!(v2.get(&k).unwrap().as_deref(), Some(&b"v2 value"[..]));
}

// ---------------------------------------------------------------------------
// clear() — the --clear-cache flag
// ---------------------------------------------------------------------------

#[test]
fn clear_is_safe_at_any_moment() {
    let (fs, _clock, store) = fresh();
    let n = ns(&store, None);
    let k = key("entry");
    n.put(&k, b"value").unwrap();
    let pin = n.pin(&k); // even a live pin does not make clear unsafe

    store.clear().unwrap();
    assert_eq!(n.get(&k).unwrap(), None, "readers see misses after clear");
    n.put(&k, b"recreated").unwrap();
    assert_eq!(n.get(&k).unwrap().as_deref(), Some(&b"recreated"[..]));
    drop(pin);

    // The store re-stamped itself and reopens cleanly.
    let reopened = open_store(fs.clone(), Arc::new(FakeClock::new()));
    let n2 = ns(&reopened, None);
    assert_eq!(n2.get(&k).unwrap().as_deref(), Some(&b"recreated"[..]));
}

#[test]
fn a_foreign_format_stamp_is_a_precise_refusal() {
    let fs: Arc<dyn FileSystem> = {
        let v = VirtualFs::new();
        v.insert(
            Path::new(ROOT).join("STORE_FORMAT"),
            b"fmn-cache 999\n".to_vec(),
        );
        Arc::new(v)
    };
    match Store::open(fs, Arc::new(FakeClock::new()), ROOT, StoreConfig::default()) {
        Err(CacheError::FormatUnsupported { found }) => {
            assert_eq!(found, "fmn-cache 999");
        }
        other => panic!("expected FormatUnsupported, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Traversal protection
// ---------------------------------------------------------------------------

#[test]
fn hostile_namespace_names_are_rejected() {
    let (_fs, _clock, store) = fresh();
    for hostile in [
        "..",
        "../../etc",
        "a/b",
        "a\\b",
        ".",
        "",
        ".hidden",
        "name.ext",
        "UPPER",
        "space name",
        "null\0byte",
        "über",
        "-flag",
    ] {
        match store.namespace(hostile, 1, NamespacePolicy::default()) {
            Err(CacheError::InvalidNamespace { name, .. }) => assert_eq!(name, hostile),
            other => panic!("{hostile:?} unexpectedly accepted: {other:?}"),
        }
    }
}

#[test]
fn arbitrary_key_bytes_never_map_outside_the_root() {
    // The traversal fuzz from the acceptance list: adversarial and random key
    // material must only ever produce object files under
    // <root>/ns/<name>/v<n>/objects/<hex>/<hex>. Keys never touch paths —
    // only their digests do — so this holds by construction; the fuzz
    // verifies the construction.
    let (fs, _clock, store) = fresh();
    let n = ns(&store, None);

    let mut hostile: Vec<Vec<u8>> = vec![
        b"../../../etc/passwd".to_vec(),
        b"..\\..\\windows\\system32".to_vec(),
        b"/absolute/path".to_vec(),
        b"nul\0byte".to_vec(),
        b"".to_vec(),
        vec![0xff; 1024],
        b"a/../b/../c".to_vec(),
    ];
    // Deterministic pseudo-random keys (LCG, no external deps).
    let mut state: u64 = 0x2545_f491_4f6c_dd1d;
    for len in 0..64usize {
        let mut buf = vec![0u8; len * 4];
        for b in &mut buf {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            *b = (state >> 33) as u8;
        }
        hostile.push(buf);
    }

    for material in &hostile {
        let k = KeyBuilder::new("fuzz")
            .push_bytes(material)
            .finish()
            .unwrap();
        n.put(&k, b"payload").unwrap();
        assert_eq!(n.get(&k).unwrap().as_deref(), Some(&b"payload"[..]));
    }

    let objects_root = Path::new(ROOT).join("ns/t/v1/objects");
    for file in files_under(&fs, Path::new("/")) {
        let s = file.to_string_lossy();
        if s.contains("/objects/") {
            // Under the one sanctioned objects directory…
            let rel = file
                .strip_prefix(&objects_root)
                .unwrap_or_else(|_| panic!("object escaped the namespace: {s}"));
            // …with exactly <2-hex>/<62-hex> components.
            let parts: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            assert_eq!(parts.len(), 2, "unexpected shape: {s}");
            assert_eq!(parts[0].len(), 2);
            assert_eq!(parts[1].len(), 62);
            assert!(
                parts
                    .iter()
                    .all(|p| p.bytes().all(|b| b.is_ascii_hexdigit())),
                "non-hex path component: {s}"
            );
        } else {
            // Everything else the store wrote is one of its fixed artifacts.
            assert!(
                s == format!("{ROOT}/STORE_FORMAT")
                    || s.ends_with("/index")
                    || s.ends_with("/lock"),
                "unexpected file: {s}"
            );
        }
    }
}
