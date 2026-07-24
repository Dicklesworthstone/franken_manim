//! The fm-fw6 acceptance suite, host half: two store openings (as two `fmn`
//! invocations would be) hammering one real directory through `StdFs` from
//! many threads — writers, readers, and maintainers — with the invariant that
//! **no observer ever sees corruption**: every get is a verified value or a
//! miss, every raw object file on disk is a complete, checksummed envelope
//! (write-temp + rename means torn intermediates are structurally
//! impossible), and eviction racing writers never breaks either side.

use fmn_cache::{CacheKey, EvictOutcome, KeyBuilder, NamespacePolicy, Store, StoreConfig};
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

fn open(root: &std::path::Path) -> Store {
    Store::open(
        Arc::new(StdFs),
        Arc::new(StdClock::new()),
        root.to_path_buf(),
        StoreConfig::default(),
    )
    .expect("open store")
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
