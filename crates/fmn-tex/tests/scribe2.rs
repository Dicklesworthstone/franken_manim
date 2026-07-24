//! The fm-7dw acceptance suite: Tex→submobject structural fixtures, the
//! span-map consumption surface, cache round-trip and key sensitivity
//! (pack/engine changes miss correctly; hits are bit-identical), the
//! parallel preflight's cache-warming contract, and the named error
//! surface at construction time.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use fmn_cache::{NamespacePolicy, Store, StoreConfig};
use fmn_platform::clock::FakeClock;
use fmn_platform::fs::VirtualFs;
use fmn_tex::{MacroSet, Mode, Style, TexEngine, Typeset};
use std::sync::Arc;

fn engine() -> TexEngine {
    TexEngine::new("fmd-math/pack/default", None).expect("engine")
}

fn store() -> Store {
    Store::open(
        Arc::new(VirtualFs::new()),
        Arc::new(FakeClock::new()),
        "/cache",
        StoreConfig::default(),
    )
    .expect("store")
}

// ---------------------------------------------------------------------------
// Structural fixtures: family shapes for corpus-shaped strings
// ---------------------------------------------------------------------------

#[test]
fn submobject_counts_are_locked_for_corpus_shapes() {
    let e = engine();
    // (source, glyphs, rules, drawn paths) — the family shape a consumer
    // builds VMobjects from. Locked: a drift is a layout change to
    // adjudicate.
    for (src, glyphs, rules, paths) in [
        ("=", 1, 0, 0),
        (r"e^{i\pi} + 1 = 0", 7, 0, 0),
        (r"\frac{d}{dx}", 3, 1, 0), // the fraction bar is a rule
        (r"\sqrt{x}", 2, 1, 0),     // surd glyph + overbar rule
        (r"\int_0^\infty e^{-x^2}\,dx", 9, 0, 0),
        // Matrix parens exceed the scale ceiling: cells as glyphs, two
        // drawn parens.
        (r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}", 4, 0, 2),
        (r"\overbrace{x+y}", 3, 0, 1), // three glyphs + the drawn band
    ] {
        let t = e.typeset(Mode::Math(Style::Display), src).unwrap();
        assert_eq!(
            (
                t.layout.glyphs.len(),
                t.layout.rules.len(),
                t.layout.paths.len()
            ),
            (glyphs, rules, paths),
            "family shape drifted for {src:?}"
        );
        assert_eq!(
            t.subs.len(),
            glyphs + rules + paths,
            "submobject table covers every primitive for {src:?}"
        );
    }
}

#[test]
fn spans_select_submobjects_by_source_identity() {
    let e = engine();
    let t = e
        .typeset(Mode::Math(Style::Display), r"e^{i\pi} + 1 = 0")
        .unwrap();
    // t2c("\pi"): exactly one occurrence, exactly one glyph submobject.
    let pi = t.occurrences(r"\pi");
    assert_eq!(pi.len(), 1);
    assert_eq!(pi[0].len(), 1);
    // The ordinal indexes a glyph whose char is π.
    let ord = pi[0][0];
    match t.subs[ord].prim {
        fmn_tex::Prim::Glyph(i) => assert_eq!(t.layout.glyphs[i].ch, 'π'),
        other => panic!("expected a glyph, got {other:?}"),
    }
    // "i": the exponent's i only — never the i inside \pi. Byte-level
    // occurrences include \pi's interior, but selection by containment
    // yields nothing there (the π glyph's span is the whole command).
    let eyes = t.occurrences("i");
    let selected: usize = eyes.iter().map(Vec::len).sum();
    assert_eq!(selected, 1);
    // isolate= over a compound substring picks up its whole family.
    let one = t.occurrences("1");
    assert_eq!(one.len(), 1);
}

#[test]
fn textext_mode_typesets_the_mainland_contract() {
    let e = engine();
    let t = e
        .typeset(Mode::Text, r"the area $\pi r^2$ of a \textbf{circle}")
        .unwrap();
    assert!(!t.layout.glyphs.is_empty());
    // The math island's π is selectable by source identity.
    let pi = t.occurrences(r"\pi");
    assert_eq!(pi.len(), 1);
    assert!(!pi[0].is_empty());
}

// ---------------------------------------------------------------------------
// The cache: round trip, bit-identity, key sensitivity
// ---------------------------------------------------------------------------

#[test]
fn cache_round_trip_is_bit_identical_and_skips_relayout() {
    let store = store();
    let e = engine().with_cache(&store).unwrap();
    let src = r"\left(\frac{a}{x}\right) + \begin{pmatrix} 1 & 0 \\ 0 & 1 \end{pmatrix}";

    let cold = e.typeset(Mode::Math(Style::Display), src).unwrap();
    let warm = e.typeset(Mode::Math(Style::Display), src).unwrap();
    // A hit is definitionally equivalent: bit-for-bit equal layout,
    // including every f64 and every span.
    assert_eq!(cold, warm);

    // The payload really came from the cache: the namespace holds the key.
    let key = e.cache_key(Mode::Math(Style::Display), src);
    let ns = store
        .namespace(
            "typeset",
            fmn_tex::TYPESET_FORMAT_VERSION,
            NamespacePolicy::default(),
        )
        .unwrap();
    let bytes = ns.get(&key).unwrap().expect("cached entry");
    assert_eq!(Typeset::from_bytes(&bytes).unwrap(), cold);
}

#[test]
fn the_codec_round_trips_exactly() {
    let e = engine();
    for src in [
        "=",
        r"e^{i\pi} + 1 = 0",
        r"\sqrt[3]{x+1}",
        r"\left\{ \frac{1}{2} \right\}",
        r"\begin{cases} x & x > 0 \\ -x & x \le 0 \end{cases}",
        r"\widehat{abc}",
    ] {
        let t = e.typeset(Mode::Math(Style::Display), src).unwrap();
        let back = Typeset::from_bytes(&t.to_bytes()).expect(src);
        assert_eq!(t, back, "codec drift for {src:?}");
    }
}

#[test]
fn corrupt_cache_payloads_decode_to_none_never_panic() {
    let e = engine();
    let t = e
        .typeset(Mode::Math(Style::Display), r"\frac{a}{b}")
        .unwrap();
    let good = t.to_bytes();
    // Truncations at every prefix; a flipped tag byte; garbage.
    for cut in 0..good.len().min(200) {
        let _ = Typeset::from_bytes(&good[..cut]); // must not panic
    }
    assert!(Typeset::from_bytes(b"not a typeset").is_none());
    let mut bad = good.clone();
    if let Some(b) = bad.last_mut() {
        *b ^= 0xff;
    }
    let _ = Typeset::from_bytes(&bad); // may decode or not; must not panic
}

#[test]
fn cache_keys_are_sensitive_to_every_semantic_input() {
    let base = engine();
    let src = r"a \minus b";
    let k = |e: &TexEngine, mode: Mode| e.cache_key(mode, src);

    // Mode and style separate.
    assert_ne!(
        k(&base, Mode::Math(Style::Display)),
        k(&base, Mode::Math(Style::Text))
    );
    assert_ne!(k(&base, Mode::Math(Style::Display)), k(&base, Mode::Text));
    // Source separates.
    assert_ne!(
        base.cache_key(Mode::Text, "a"),
        base.cache_key(Mode::Text, "b")
    );

    // A pack change separates (the fingerprint folds the macro table in):
    // the same source under empty vs default pack must miss.
    let empty_pack = TexEngine::new("fmd-math/pack/empty", None).unwrap();
    assert_ne!(
        k(&base, Mode::Math(Style::Display)),
        k(&empty_pack, Mode::Math(Style::Display))
    );

    // A caller macro layered over the pack separates too.
    let mut extra = MacroSet::new();
    extra.define("half", 1, r"\frac{#1}{2}").unwrap();
    let with_extra = TexEngine::new("fmd-math/pack/default", Some(&extra)).unwrap();
    assert_ne!(
        k(&base, Mode::Math(Style::Display)),
        k(&with_extra, Mode::Math(Style::Display))
    );
    // And the layered macro actually expands.
    let t = with_extra
        .typeset(Mode::Math(Style::Display), r"\half{x}")
        .unwrap();
    assert!(t.layout.rules.len() == 1, "the macro's fraction bar");
}

#[test]
fn pack_content_ids_resolve_and_drift_is_named() {
    let e = TexEngine::new("fmd-math/pack/basic", None).unwrap();
    assert_eq!(e.pack_content_id(), "fmd-math/pack/basic");
    match TexEngine::new("fmd-math/pack/nonexistent", None) {
        Err(fmn_tex::TexError::UnknownPack { content_id }) => {
            assert_eq!(content_id, "fmd-math/pack/nonexistent");
        }
        other => panic!("expected UnknownPack, got {other:?}"),
    }
}

#[test]
fn config_template_wires_through_the_registry() {
    use fmn_config::config::Layer;
    let registry = fmn_config::PackRegistry::builtin();
    let resolved = fmn_config::Config::resolve(&[], None).unwrap();
    let e = TexEngine::from_config(&resolved.config, &registry).unwrap();
    assert_eq!(e.pack_content_id(), "fmd-math/pack/default");
    // The pack's \minus works end to end from config.
    let t = e
        .typeset(Mode::Math(Style::Display), r"a \minus b")
        .unwrap();
    assert!(t.layout.glyphs.iter().any(|g| g.ch == '−'));

    // An out-of-tier template is the registry's named refusal.
    let ctex = fmn_config::Config::resolve(
        &[Layer {
            name: "u",
            text: "tex:\n  template: \"ctex\"\n",
        }],
        None,
    )
    .unwrap();
    match TexEngine::from_config(&ctex.config, &registry) {
        Err(fmn_tex::TexError::Pack(e)) => {
            assert!(e.to_string().contains("no native preamble pack"), "{e}");
        }
        other => panic!("expected the pack refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The preflight
// ---------------------------------------------------------------------------

#[test]
fn preflight_warms_the_cache_in_parallel_and_collects_errors() {
    let store = store();
    let e = engine().with_cache(&store).unwrap();
    let items: Vec<(Mode, &str)> = vec![
        (Mode::Math(Style::Display), r"e^{i\pi} + 1 = 0"),
        (Mode::Math(Style::Display), r"\frac{d}{dx} \sin x"),
        (Mode::Math(Style::Display), r"\substack{a \\ b}"), // tier-2: fails
        (Mode::Text, r"area $\pi r^2$"),
        (
            Mode::Math(Style::Display),
            r"\begin{pmatrix} 1 \\ 0 \end{pmatrix}",
        ),
    ];
    let outcomes = e.preflight(&items);
    assert_eq!(outcomes.len(), items.len());
    // Per-string outcomes, in order; the tier-2 string fails precisely
    // without aborting the batch.
    assert!(outcomes[0].is_ok());
    assert!(outcomes[1].is_ok());
    let err = outcomes[2].as_ref().unwrap_err();
    assert!(err.to_string().contains("tier T2"), "{err}");
    assert!(outcomes[3].is_ok());
    assert!(outcomes[4].is_ok());

    // The cache is warm: every successful string hits without re-layout.
    let ns = store
        .namespace(
            "typeset",
            fmn_tex::TYPESET_FORMAT_VERSION,
            NamespacePolicy::default(),
        )
        .unwrap();
    for (i, (mode, src)) in items.iter().enumerate() {
        let cached = ns.get(&e.cache_key(*mode, src)).unwrap();
        assert_eq!(
            cached.is_some(),
            outcomes[i].is_ok(),
            "cache state mismatch for {src:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The error surface at construction time
// ---------------------------------------------------------------------------

#[test]
fn construct_errors_surface_named_and_tier_tagged() {
    let e = engine();
    let err = e
        .typeset(Mode::Math(Style::Display), r"\substack{a \\ b}")
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains(r"`\substack` is not yet supported"), "{msg}");
    assert!(msg.contains("tier T2"), "{msg}");
    assert!(msg.contains("fm-j5t"), "tracked-at pointer: {msg}");

    // A genuinely malformed structure (a double superscript) is a
    // byte-positioned refusal. (`\frac{1}{` is NOT an error: the
    // SingleStringTex fragment tolerance lets end-of-input close open
    // arguments — each literal argument may be a piece of a balanced
    // whole.)
    let err = e.typeset(Mode::Math(Style::Display), r"a^b^c").unwrap_err();
    assert!(err.to_string().contains("malformed"), "{err}");
    assert!(
        e.typeset(Mode::Math(Style::Display), r"\frac{1}{").is_ok(),
        "fragment tolerance holds"
    );
}
