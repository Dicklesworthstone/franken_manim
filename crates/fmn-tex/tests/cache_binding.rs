//! Production cache binding: actual layout counts, not timing thresholds.

use fmn_cache::{NamespacePolicy, Store, StoreConfig};
use fmn_platform::clock::FakeClock;
use fmn_platform::fs::VirtualFs;
use fmn_tex::{Mode, Style, TYPESET_FORMAT_VERSION, TexEngine};
use std::sync::Arc;

fn store() -> Store {
    Store::open(
        Arc::new(VirtualFs::new()),
        Arc::new(FakeClock::new()),
        if cfg!(windows) { r"C:\cache" } else { "/cache" },
        StoreConfig::default(),
    )
    .unwrap()
}

fn engine() -> TexEngine {
    TexEngine::new("fmd-math/pack/default", None).unwrap()
}

#[test]
fn disk_retention_respects_pins_and_recomputed_layouts_keep_their_bytes() {
    let store = store();
    let cached = engine();
    cached.set_cache_with_ceiling(Some(&store), 0).unwrap();
    let namespace = store
        .namespace(
            "typeset",
            TYPESET_FORMAT_VERSION,
            NamespacePolicy::default(),
        )
        .unwrap();
    let mode = Mode::Math(Style::Display);
    let sources = [r"\frac{x^2+1}{2}", r"\sqrt{y}", "z+1"];
    let expected: Vec<_> = sources
        .iter()
        .map(|source| cached.typeset(mode, source).unwrap().to_bytes().unwrap())
        .collect();
    let pinned_key = cached.cache_key(mode, sources[0]).unwrap();
    let pin = namespace.pin(&pinned_key);
    assert!(namespace.usage().unwrap() > 0);
    cached.maintain_cache();
    let first = cached.cache_maintenance();
    assert_eq!(first.runs, 1);
    assert_eq!(first.evicted_entries, 2);
    assert_eq!(first.skipped_pinned, 1);
    assert_eq!(first.errors, 0);
    assert_eq!(first.retained_bytes, Some(namespace.usage().unwrap()));
    assert!(namespace.get(&pinned_key).unwrap().is_some());
    assert!(
        namespace
            .get(&cached.cache_key(mode, sources[1]).unwrap())
            .unwrap()
            .is_none()
    );

    let next = engine().with_cache(&store).unwrap();
    for (source, expected) in sources.iter().zip(expected) {
        assert_eq!(
            next.typeset(mode, source).unwrap().to_bytes().unwrap(),
            expected
        );
    }
    assert_eq!(next.persistent_cache_hits(), 1);
    assert_eq!(next.layout_computations(), 2);
    drop(pin);
    cached.maintain_cache();
    assert_eq!(namespace.usage().unwrap(), 0);
    assert_eq!(cached.cache_maintenance().retained_bytes, Some(0));
    assert_eq!(cached.persistent_cache_ceiling(), Some(0));
}

#[test]
fn maintenance_failure_is_diagnostic_and_does_not_replace_real_ink() {
    let fs = Arc::new(VirtualFs::new());
    let store = Store::open(
        fs.clone(),
        Arc::new(FakeClock::new()),
        if cfg!(windows) { r"C:\cache" } else { "/cache" },
        StoreConfig::default(),
    )
    .unwrap();
    let cached = engine();
    cached.set_cache_with_ceiling(Some(&store), 0).unwrap();
    let mode = Mode::Math(Style::Display);
    cached.typeset(mode, "x").unwrap();
    // A host can replace the owned root after configuration. Maintenance must
    // refuse that new node, and ordinary layout must still return actual ink.
    fs.insert(store.root(), b"foreign replacement".to_vec());
    cached.maintain_cache();
    let report = cached.cache_maintenance();
    assert_eq!(report.runs, 1);
    assert_eq!(report.errors, 1);
    assert!(report.last_error.is_some());
    assert_eq!(report.retained_bytes, None);
    assert_eq!(
        cached.typeset(mode, "y+1").unwrap().to_bytes().unwrap(),
        engine().typeset(mode, "y+1").unwrap().to_bytes().unwrap()
    );
    assert!(cached.persistent_store_errors() > 0);
}

#[test]
fn concurrent_readers_keep_pinned_layouts_complete_during_retention() {
    let store = store();
    let cached = engine();
    cached.set_cache_with_ceiling(Some(&store), 0).unwrap();
    let mode = Mode::Math(Style::Display);
    let source = r"\frac{x^2+1}{2}";
    let expected = cached.typeset(mode, source).unwrap().to_bytes().unwrap();
    let key = cached.cache_key(mode, source).unwrap();
    let namespace = store
        .namespace(
            "typeset",
            TYPESET_FORMAT_VERSION,
            NamespacePolicy::default(),
        )
        .unwrap();
    let pin = namespace.pin(&key);
    let barrier = std::sync::Barrier::new(5);
    let complete = std::sync::atomic::AtomicBool::new(true);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..16 {
                    barrier.wait();
                    if namespace.get(&key).ok().flatten().as_deref() != Some(expected.as_slice()) {
                        complete.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                    barrier.wait();
                }
            });
        }
        for _ in 0..16 {
            barrier.wait();
            cached.maintain_cache();
            barrier.wait();
        }
    });
    assert!(complete.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(cached.cache_maintenance().errors, 0);
    drop(pin);
    cached.maintain_cache();
    assert!(namespace.get(&key).unwrap().is_none());
}

#[test]
fn attaching_after_a_layout_populates_disk_and_a_new_engine_reads_it() {
    let store = store();
    let first = engine();
    let mode = Mode::Math(Style::Display);
    let source = r"\frac{x^2+1}{2}";
    let expected = first.typeset(mode, source).unwrap().to_bytes().unwrap();
    assert_eq!(first.layout_computations(), 1);
    first.set_cache(Some(&store)).unwrap();
    assert!(first.persistent_cache_enabled());
    assert_eq!(
        first.typeset(mode, source).unwrap().to_bytes().unwrap(),
        expected
    );
    let second = engine().with_cache(&store).unwrap();
    assert_eq!(
        second.typeset(mode, source).unwrap().to_bytes().unwrap(),
        expected
    );
    assert_eq!(
        second.layout_computations(),
        0,
        "disk hit must bypass the layout engine"
    );
    assert_eq!(second.persistent_cache_hits(), 1);
    second.typeset(mode, source).unwrap();
    assert_eq!(second.memory_cache_stats().hits, 1);
    assert_eq!(second.persistent_cache_hits(), 1);
}

#[test]
fn switching_stores_and_detaching_do_not_change_layout_or_write_to_old_store() {
    let old = store();
    let new = store();
    let engine = engine().with_cache(&old).unwrap();
    let mode = Mode::Math(Style::Display);
    let before = engine.typeset(mode, "x").unwrap().to_bytes().unwrap();
    engine.set_cache(Some(&new)).unwrap();
    assert_eq!(
        engine.typeset(mode, "x").unwrap().to_bytes().unwrap(),
        before
    );
    engine.typeset(mode, "y").unwrap();
    let old_ns = old
        .namespace(
            "typeset",
            TYPESET_FORMAT_VERSION,
            NamespacePolicy::default(),
        )
        .unwrap();
    let new_ns = new
        .namespace(
            "typeset",
            TYPESET_FORMAT_VERSION,
            NamespacePolicy::default(),
        )
        .unwrap();
    let y = engine.cache_key(mode, "y").unwrap();
    assert!(old_ns.get(&y).unwrap().is_none());
    assert!(new_ns.get(&y).unwrap().is_some());
    engine.set_cache(None).unwrap();
    assert!(!engine.persistent_cache_enabled());
    engine.typeset(mode, "z").unwrap();
    assert!(
        new_ns
            .get(&engine.cache_key(mode, "z").unwrap())
            .unwrap()
            .is_none()
    );
    assert!(
        old_ns
            .get(&engine.cache_key(mode, "x").unwrap())
            .unwrap()
            .is_some()
    );
}

#[test]
fn persisted_math_modes_and_preamble_expansions_keep_independent_keys() {
    let store = store();
    let first = engine().with_cache(&store).unwrap();
    let display = Mode::Math(Style::Display);
    let inline = Mode::Math(Style::Text);
    let source = r"\frac{1}{2}";
    let a = first.typeset(display, source).unwrap().to_bytes().unwrap();
    let b = first.typeset(inline, source).unwrap().to_bytes().unwrap();
    assert_ne!(a, b);
    let macro_source = r"\answer";
    let p = r"\newcommand{\answer}{x}";
    let q = r"\newcommand{\answer}{y}";
    let x = first
        .typeset_with_preamble(display, macro_source, p)
        .unwrap()
        .to_bytes()
        .unwrap();
    let y = first
        .typeset_with_preamble(display, macro_source, q)
        .unwrap()
        .to_bytes()
        .unwrap();
    assert_ne!(x, y);
    let second = engine().with_cache(&store).unwrap();
    assert_eq!(
        second.typeset(display, source).unwrap().to_bytes().unwrap(),
        a
    );
    assert_eq!(
        second.typeset(inline, source).unwrap().to_bytes().unwrap(),
        b
    );
    assert_eq!(
        second
            .typeset_with_preamble(display, macro_source, p)
            .unwrap()
            .to_bytes()
            .unwrap(),
        x
    );
    assert_eq!(
        second
            .typeset_with_preamble(display, macro_source, q)
            .unwrap()
            .to_bytes()
            .unwrap(),
        y
    );
    assert_eq!(second.layout_computations(), 0);
}

#[test]
fn concurrent_layouts_and_cache_rebinding_preserve_owned_results() {
    let old = store();
    let new = store();
    let engine = engine().with_cache(&old).unwrap();
    let mode = Mode::Math(Style::Display);
    let expected = engine.typeset(mode, "x^2+1").unwrap().to_bytes().unwrap();
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let engine = &engine;
            let expected = &expected;
            scope.spawn(move || {
                for _ in 0..8 {
                    assert_eq!(
                        &engine.typeset(mode, "x^2+1").unwrap().to_bytes().unwrap(),
                        expected
                    );
                }
            });
        }
        engine.set_cache(Some(&new)).unwrap();
        engine.set_cache(None).unwrap();
    });
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn refused_host_configuration_detaches_instead_of_retaining_the_previous_directory() {
    let store = store();
    let engine = engine().with_cache(&store).unwrap();
    assert!(
        engine
            .configure_host_cache(Some(if cfg!(windows) { r"C:\" } else { "/" }))
            .is_err()
    );
    assert!(!engine.persistent_cache_enabled());
    assert!(engine.typeset(Mode::Math(Style::Display), "x+1").is_ok());
}
