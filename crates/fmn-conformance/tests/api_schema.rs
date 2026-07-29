//! The one API schema and its generated artifacts (fm-vn6, §16.2, §16.1).
//!
//! The schema lives in two committed files at the repo root — `API_SCHEMA.tsv`
//! (extracted from the pinned Reference by `scripts/gen_api_schema.py`) and
//! `API_OVERLAY.tsv` (authored). Everything generated from them is also
//! committed, and this suite is the reason that is safe: each artifact is
//! regenerated in memory and compared byte-for-byte with the committed file.
//!
//! **This is the "drift between the front doors is a build error" mechanism.**
//! Edit a generated signature by hand and the comparison fails; add a config
//! key without binding it and the coverage check fails; let the overlay go
//! stale against a re-extracted Reference and the merge fails naming the
//! dangling key.
//!
//! Regenerate deliberately with:
//!
//! ```text
//! UPDATE_API_ARTIFACTS=1 cargo test -p fmn-conformance --test api_schema
//! ```
//!
//! which rewrites the artifacts in the working tree and never commits them —
//! the same bless discipline as `UPDATE_GOLDENS=1` and `RATCHET_UPDATE=1`.

#![allow(clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use fmn_conformance::schema::{
    Schema, Status, SymbolKind, generate_cli_rs, generate_cli_table_md, generate_config_rs,
    generate_docs_md, generate_ledger_tsv,
};

/// Repo-root-relative path (`CARGO_MANIFEST_DIR` is `crates/fmn-conformance`).
fn repo_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name)
}

fn repo_file(name: &str) -> String {
    std::fs::read_to_string(repo_path(name)).unwrap_or_else(|e| panic!("reading {name}: {e}"))
}

fn schema() -> Schema {
    Schema::parse(&repo_file("API_SCHEMA.tsv"), &repo_file("API_OVERLAY.tsv"))
        .unwrap_or_else(|e| panic!("{e}"))
}

/// Whether this run rewrites artifacts instead of checking them.
fn blessing() -> bool {
    std::env::var("UPDATE_API_ARTIFACTS").is_ok_and(|v| v == "1")
}

/// Compare a generated artifact with its committed copy, or rewrite it.
#[track_caller]
fn artifact(name: &str, generated: &str) {
    let path = repo_path(name);
    if blessing() {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|e| panic!("mkdir {name}: {e}"));
        }
        std::fs::write(&path, generated).unwrap_or_else(|e| panic!("writing {name}: {e}"));
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "{name} is missing ({e}). Generate it with:\n  \
             UPDATE_API_ARTIFACTS=1 cargo test -p fmn-conformance --test api_schema"
        )
    });
    if committed == generated {
        return;
    }
    let (line, before, after) = first_difference(&committed, generated);
    panic!(
        "{name} has drifted from the schema at line {line}.\n\
         \x20 committed:  {before}\n\
         \x20 schema says: {after}\n\n\
         The schema is the source of truth: either the hand edit belongs in \
         API_SCHEMA.tsv/API_OVERLAY.tsv, or the artifact needs regenerating with\n  \
         UPDATE_API_ARTIFACTS=1 cargo test -p fmn-conformance --test api_schema"
    );
}

/// First differing line, 1-based, with both sides — a diff a human can act on
/// without reading two thousand rows.
fn first_difference(a: &str, b: &str) -> (usize, String, String) {
    for (index, (left, right)) in a.lines().zip(b.lines()).enumerate() {
        if left != right {
            return (index + 1, left.to_owned(), right.to_owned());
        }
    }
    let shared = a.lines().count().min(b.lines().count());
    let tail = |s: &str| s.lines().nth(shared).unwrap_or("<end of file>").to_owned();
    (shared + 1, tail(a), tail(b))
}

// ---------------------------------------------------------------------------
// The schema itself
// ---------------------------------------------------------------------------

#[test]
fn the_schema_parses_and_names_the_pinned_reference() {
    let schema = schema();
    let suite = repo_file("SUITE.lock");
    let pinned = suite
        .lines()
        .find_map(|l| l.strip_prefix("3b1b/manim\t"))
        .and_then(|rest| rest.split('\t').next())
        .expect("SUITE.lock [reference] pins 3b1b/manim");
    assert_eq!(
        schema.reference_commit(),
        pinned,
        "API_SCHEMA.tsv was extracted from a different Reference commit than \
         SUITE.lock pins — rerun scripts/gen_api_schema.py"
    );
}

#[test]
fn the_class_census_matches_the_plans_verified_count() {
    // Appendix A: "the complete Reference census (257 classes, verified
    // class-by-class against the pin)". The schema is extracted independently
    // of that audit, so agreement is a real cross-check on both — and a
    // Reference pin bump that changes the class count will say so here.
    let schema = schema();
    assert_eq!(
        schema.of_kind(SymbolKind::Class).len(),
        257,
        "the extracted class count no longer matches Appendix A's verified census"
    );
}

#[test]
fn the_wildcard_surface_is_enumerated_not_assumed() {
    // §1.6: `from manimlib import *` has no authoritative `__all__`, so the
    // export set is a computed closure. The extractor records its own count in
    // [meta]; this asserts the rows agree with it, which catches an extractor
    // that walked the closure one way and emitted rows another.
    let schema = schema();
    let declared: usize = schema
        .meta
        .get("wildcard_exports")
        .expect("[meta] wildcard_exports")
        .parse()
        .expect("a count");
    let names: std::collections::BTreeSet<&str> =
        schema.exported().iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names.len(),
        declared,
        "the exported rows and [meta] wildcard_exports disagree"
    );

    // The closure genuinely leaks third-party names — that is the finding, not
    // a bug, and the Ledger has to carry them.
    assert!(
        names.contains("np"),
        "`from manimlib import *` really does bind numpy as `np`; losing that \
         row means the extractor stopped modelling CPython's rules"
    );
    // ...while type-checking-only imports must NOT be in it.
    assert!(
        !names.contains("Vect3"),
        "`Vect3` is imported under `if TYPE_CHECKING:` and is not bound at \
         runtime; the extractor must skip those bodies"
    );
}

#[test]
fn every_class_of_the_census_carries_its_constructor_parameters() {
    let schema = schema();
    let classes: Vec<&str> = schema
        .of_kind(SymbolKind::Class)
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    // A class that declares `__init__` must have its parameters recorded;
    // otherwise the Python front door has nothing to reproduce.
    let inits: std::collections::BTreeSet<&str> = schema
        .of_kind(SymbolKind::Method)
        .iter()
        .filter(|s| s.name.ends_with(".__init__"))
        .map(|s| s.name.trim_end_matches(".__init__"))
        .collect();
    for class in &inits {
        assert!(
            classes.contains(class),
            "`{class}.__init__` has no class row"
        );
        let owner_suffix = format!(":{class}.__init__");
        assert!(
            schema
                .params
                .iter()
                .any(|p| p.owner.ends_with(&owner_suffix)),
            "`{class}.__init__` has no recorded parameters"
        );
    }
    assert!(inits.len() > 100, "suspiciously few constructors recorded");
}

#[test]
fn defaults_are_recorded_as_source_expressions() {
    // `Arc(angle=TAU / 4, arc_center=ORIGIN)`: resolving those would need the
    // import the extractor deliberately refuses, and the expression text is
    // what the ledger and the Python signatures both want.
    let schema = schema();
    let arc: Vec<(&str, Option<&str>)> = schema
        .params
        .iter()
        .filter(|p| p.owner == "manimlib.mobject.geometry:Arc.__init__")
        .map(|p| (p.name.as_str(), p.default.as_deref()))
        .collect();
    assert!(
        arc.contains(&("angle", Some("TAU / 4"))),
        "Arc.angle default lost its expression: {arc:?}"
    );
    assert!(
        arc.contains(&("arc_center", Some("ORIGIN"))),
        "Arc.arc_center default lost its expression: {arc:?}"
    );
}

// ---------------------------------------------------------------------------
// C-9 — canonical names, exact-name aliases
// ---------------------------------------------------------------------------

#[test]
fn every_public_surface_typo_has_a_canonical_name() {
    // Appendix C, C-9. The plan names three examples; the extracted surface
    // shows the real extent. Whatever the extractor finds must be ruled on —
    // a partial rename would be worse than none, because callers could not
    // predict which spelling any given member takes.
    let schema = schema();
    let misspellings = ["listner", "cahced", "tickness"];
    let mut unruled = Vec::new();
    for symbol in &schema.symbols {
        let leaf = symbol.name.rsplit('.').next().unwrap_or(&symbol.name);
        if misspellings.iter().any(|m| leaf.contains(m))
            && !schema.renames.contains_key(&symbol.key())
        {
            unruled.push(symbol.key());
        }
    }
    assert!(
        unruled.is_empty(),
        "these public-surface typos have no canonical name in API_OVERLAY.tsv \
         [canonical]: {unruled:?}"
    );
    assert!(
        schema.renames.len() >= 26,
        "C-9 covers 26 symbols at this pin, found {}",
        schema.renames.len()
    );
}

#[test]
fn a_canonical_name_fixes_the_typo_and_keeps_the_owner() {
    let schema = schema();
    let listener = schema
        .symbols
        .iter()
        .find(|s| s.name == "Mobject.add_event_listner")
        .expect("the Reference's Mobject.add_event_listner");
    assert_eq!(
        schema.canonical_name(listener),
        "Mobject.add_event_listener"
    );

    let attribute = schema
        .symbols
        .iter()
        .find(|s| s.name == "Arrow.tickness_multiplier")
        .expect("the Reference's Arrow.tickness_multiplier");
    assert_eq!(
        schema.canonical_name(attribute),
        "Arrow.thickness_multiplier"
    );

    // Every ruling must actually change the spelling, and must only fix the
    // typo — a "canonical" name identical to the Reference's, or one that
    // renamed the concept, is a mistake in the overlay.
    for rename in schema.renames.values() {
        let leaf = rename
            .symbol
            .rsplit(['.', ':'])
            .next()
            .expect("a leaf name");
        assert_ne!(
            leaf, rename.canonical,
            "{} is listed as a rename but does not change the name",
            rename.symbol
        );
        // C-9 fixes spelling, it does not rename concepts. Every ruling must
        // be within two edits of the Reference's own name — enough for a
        // dropped letter (`listner`), a transposition (`cahced`), or a
        // doubled fix, and not enough to smuggle in an API redesign under a
        // typo ruling.
        let distance = edit_distance(leaf, &rename.canonical);
        assert!(
            distance <= 2,
            "{leaf} -> {} is {distance} edits apart; that is a rename, not a \
             typo fix, and needs its own ruling rather than a C-9 row",
            rename.canonical
        );
    }
}

/// Levenshtein distance, for holding C-9 rulings to actual typo repairs.
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[test]
fn the_reference_spelling_survives_as_the_python_alias() {
    // The alias is not a deprecation shim: it is the Reference's public
    // surface, and AGENTS.md's no-shims rule names the manim API as its
    // standing exception. fmn-python binds both names; nothing here may
    // suggest the old one goes away.
    let schema = schema();
    for rename in schema.renames.values() {
        let leaf = rename.symbol.rsplit(['.', ':']).next().expect("a leaf");
        assert!(
            schema
                .symbols
                .iter()
                .any(|s| s.key() == rename.symbol && s.name.ends_with(leaf)),
            "the Reference spelling `{leaf}` must remain in the schema as the alias"
        );
    }
}

// ---------------------------------------------------------------------------
// Config keys
// ---------------------------------------------------------------------------

#[test]
fn every_config_key_is_bound_exactly_once() {
    schema()
        .check_config_coverage()
        .unwrap_or_else(|e| panic!("{e}"));
}

#[test]
fn the_shipped_config_covers_the_references_key_surface() {
    // The Reference's own default_config.yml is the parity check, not the
    // source: we ship its key shapes so existing custom_config.yml files
    // overlay onto familiar names, plus native sections it never had.
    let schema = schema();
    let shared = schema.config.iter().filter(|c| c.in_reference).count();
    assert!(
        shared > 90,
        "only {shared} keys are shared with the Reference — the shipped \
         defaults have drifted off its key surface"
    );
    for native in ["determinism.mode", "determinism.seed", "render.engine"] {
        let key = schema
            .config
            .iter()
            .find(|c| c.path == native)
            .unwrap_or_else(|| panic!("native key `{native}` missing"));
        assert!(
            !key.in_reference,
            "`{native}` is FrankenManim-native and must not claim Reference provenance"
        );
    }
}

// ---------------------------------------------------------------------------
// The generated artifacts — the drift gate
// ---------------------------------------------------------------------------

#[test]
fn the_generated_config_extraction_matches_the_schema() {
    artifact(
        "crates/fmn-config/src/generated.rs",
        &generate_config_rs(&schema()),
    );
}

#[test]
fn the_generated_ledger_matches_the_schema() {
    artifact("docs/api/ledger.tsv", &generate_ledger_tsv(&schema()));
}

#[test]
fn the_generated_cli_table_matches_the_schema() {
    artifact("docs/api/cli_flags.md", &generate_cli_table_md(&schema()));
}

#[test]
fn the_generated_cli_parser_contract_matches_the_schema() {
    artifact(
        "crates/fmn-cli/src/generated.rs",
        &generate_cli_rs(&schema()),
    );
}

#[test]
fn the_generated_docs_match_the_schema() {
    artifact("docs/api/schema.md", &generate_docs_md(&schema()));
}

#[test]
fn a_hand_edited_signature_fails_the_drift_gate() {
    // The negative test the acceptance criteria ask for: mutate a generated
    // binding and prove the comparison catches it. Generated in memory rather
    // than read from the tree, so it neither races the blessing run nor
    // depends on the artifact already existing.
    let generated = generate_config_rs(&schema());
    let mutated = generated.replace(
        "fps: cx.u32(\"camera.fps\")?,",
        "fps: cx.u64(\"camera.fps\")?,",
    );
    assert_ne!(
        generated, mutated,
        "the fixture line vanished from the generated extraction; update this test"
    );
    let (line, before, after) = first_difference(&mutated, &generated);
    assert!(line > 0, "a mutated artifact must differ somewhere");
    assert!(
        before.contains("u64") && after.contains("u32"),
        "the drift report must name both sides: {before} / {after}"
    );

    // And the same for a hand-edited *config key*: the accessor is only half
    // the contract, the path is the other half.
    let repathed = generated.replace("\"camera.fps\"", "\"camera.framerate\"");
    let (_, before, after) = first_difference(&repathed, &generated);
    assert!(
        before.contains("camera.framerate") && after.contains("camera.fps"),
        "{before} / {after}"
    );
}

#[test]
fn the_ledger_carries_every_symbol_and_its_tier() {
    let schema = schema();
    let ledger = generate_ledger_tsv(&schema);
    let rows = ledger
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .count();
    assert_eq!(
        rows,
        schema.symbols.len(),
        "the Ledger must carry one row per symbol"
    );
    assert!(
        ledger.contains("\timproved\tBN-11"),
        "the seeded BN-11 rulings must reach the Ledger"
    );
    // Unreviewed is the honest default and must be the bulk of the surface
    // today; the Ledger's own bead ratchets it down.
    let unreviewed = schema
        .symbols
        .iter()
        .filter(|s| schema.status(s) == Status::Unreviewed)
        .count();
    assert!(
        unreviewed > schema.symbols.len() / 2,
        "the tier seeding claims more adjudication than has happened"
    );
}
