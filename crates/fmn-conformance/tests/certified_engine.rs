//! The certified CPU engine's self-goldens and PG-5's per-commit sweep (fm-ig3).
//!
//! `self_goldens.rs` locks geometry and stage lifecycles and says, at its top,
//! "frame hashes join these artifacts once Lumen exists". Lumen exists; this is
//! that file.
//!
//! ## Why these locks are `Scope::Certified`
//!
//! `docs/INPUT_CLOSURE.md` §6: since §5 is frozen and the arithmetic is
//! portable, artifacts on the certified path get **one** lock shared by the
//! whole matrix rather than three per-platform ones. The consequence is the
//! point — a cross-platform lock **fails on whichever machine breaks it**,
//! instead of passing everywhere and waiting for someone to re-run a
//! three-platform sweep. G0-6 demonstrated the shape with a single frame
//! constant; this is the shape applied to a corpus.
//!
//! Locking the raw frame rather than a PNG is deliberate: §16.7's certified
//! artifact kinds start with *raw frames*, and the canonical PNG is one
//! table lookup away through a kernel that does no arithmetic at all
//! (`fmn_frame::convert::rgba16f_to_rgba8`). Locking the raw frame locks the
//! thing the engine actually decides.
//!
//! ## The three corpora, and who asked for them
//!
//! - **`caps_and_joins.v1`** — fm-oac's owed acceptance item. §10.3 asks for
//!   "cap/join golden fixtures (every combination, including miter-limit escapes
//!   to bevel)". The *geometry* of every combination is tested in
//!   `fmn_render::stroke`; a golden is a rasterized frame, and there was no
//!   engine to rasterize one, so fm-oac closed by handing the item here.
//! - **`fills.v1`** — the same for §10.2: flat and gradient fills, a winding
//!   hole, the hinted disc and rectangle routes, the **unhinted** general path,
//!   a self-intersecting pentagram, and the inner border.
//! - **`composite.v1`** — the frame that stands where G0-6's does. It carries
//!   what fm-zn9 named — gradient fills, a winding hole, joins, a tapered
//!   stroke, a sub-AA hairline, alpha compositing throughout — in the engine's
//!   own terms rather than the spike's IR. It is not the *same* frame: the spike
//!   is not a workspace member (ADR-0003's non-shipped tier), so its frame is not
//!   reachable from this build. What transfers is the property, not the digest.
//!
//! Re-blessing: `UPDATE_GOLDENS=1 cargo test -p fmn-conformance --test
//! certified_engine`, then commit the lock diff. GOVERNANCE §5 applies with full
//! force here — **a drift is a finding to adjudicate, never a number to
//! re-bless**. These bytes are the product promise.

use fmn_conformance::golden::{GoldenStore, Scope};
use fmn_core::color::Srgb;
use fmn_core::constants::{BLUE_C, GREEN_B, MAROON_C, RED_C, TEAL_B, WHITE, YELLOW_C};
use fmn_library::style::VStyle;
use fmn_library::{Circle, Dot, Line, Polygon, Rectangle};
use fmn_mobject::{JointType, Mob, Stage};
use fmn_render::bin::{Binning, ScreenMap, Tiling, Viewport};
use fmn_render::engine::{
    AaPolicy, EngineIdentity, FrameConfig, FrameJob, Tier, encode_frame, journal_digest,
};
use fmn_render::fill::MonoTable;
use fmn_render::plan::RenderPlan;
use std::path::PathBuf;

/// The frame every corpus renders into.
///
/// Small on purpose, for G0-6's reason: a golden nobody can afford to re-run is
/// a golden nobody re-runs. Bit-identity over a quarter of a million components
/// either holds or it does not.
const WIDTH: u32 = 320;
const HEIGHT: u32 = 180;

/// The declared certified configuration's tile dimensions (C10).
const TILING: Tiling = Tiling {
    macro_tile: 128,
    fine_tile: 16,
};

/// Pixels per scene unit. The Reference's is 135 at 1080p; this is the same
/// order, chosen so a default `Dot` takes the hinted radial kernel and a
/// two-unit `Circle` does not — the boundary §10.2 draws.
const SCALE: f64 = 60.0;

fn store() -> GoldenStore {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("goldens");
    GoldenStore::new(dir, "certified_engine", Scope::Certified).expect("store")
}

fn config() -> FrameConfig {
    FrameConfig::new(
        Viewport {
            width: WIDTH,
            height: HEIGHT,
        },
        ScreenMap {
            scale: SCALE,
            // Object-space origin at the frame centre, with +y downward: the
            // engine has no camera yet (that is fm-0gy), so the map carries the
            // placement and a positive scale keeps `screen_aabb` honest.
            origin: [f64::from(WIDTH) / 2.0, f64::from(HEIGHT) / 2.0],
        },
        // The Reference's #333333, decoded once here rather than per pixel.
        Srgb::from_rgb8(0x33, 0x33, 0x33).to_linear(1.0),
    )
}

/// Render a scene at an explicit tier and thread count.
///
/// The tier is threaded through [`FrameJob::with_identity`] rather than ignored,
/// so when fm-4wt makes the engine dispatch on it this harness needs no change:
/// the sweep below already renders the corpus once per registered tier and holds
/// every one of them to the scalar definition.
fn render(stage: &Stage, tier: Tier, threads: usize) -> Vec<u8> {
    render_with_aa(stage, tier, threads, AaPolicy::Adaptive)
}

/// [`render`] with an explicit A/B policy.
fn render_with_aa(stage: &Stage, tier: Tier, threads: usize, aa: AaPolicy) -> Vec<u8> {
    let cfg = config().with_aa_policy(aa);
    let mut plan = RenderPlan::new();
    plan.sync(stage, 0);
    let mono = MonoTable::build(&plan, cfg.map);
    let mut binning = Binning::build(&plan, cfg.viewport, TILING, cfg.map);
    binning.prune_occluded(&plan).expect("matching plan");
    let identity = EngineIdentity {
        tier,
        ..EngineIdentity::certified()
    };
    let job = FrameJob::with_identity(&plan, &mono, &binning, cfg, identity)
        .expect("matching frame artifacts");
    let frame = job.render(threads).expect("the engine renders the frame");
    encode_frame(&frame).expect("the frame encodes into its canonical document")
}

/// The scalar definition of a scene's frame, single-threaded.
fn definition(stage: &Stage) -> Vec<u8> {
    render(stage, Tier::Scalar, 1)
}

fn add(stage: &mut Stage, m: impl Into<fmn_mobject::Mobject>) -> Mob {
    let h = stage.add(m);
    stage.add_to_scene(h).expect("live");
    h
}

// ------------------------------------------------------------- the corpora

/// §10.3's cap/join corpus: every joint setting over three corner severities,
/// plus the open ends that carry round caps.
///
/// The three severities are chosen so the settings actually diverge. A shallow
/// corner puts bevel, miter and round within a pixel of one another; a sharp one
/// separates them; and the near-reversal is past `MITER_LIMIT = √10`, so a miter
/// must fall back to a bevel there rather than growing a spike of unbounded
/// length. A corpus without the third would lock the limit's absence.
fn caps_and_joins() -> Stage {
    let mut stage = Stage::new();
    let joints = [
        JointType::Auto,
        JointType::NoJoint,
        JointType::Bevel,
        JointType::Miter,
    ];
    // Three corner severities, given as the arms' half-separation over the
    // triangle's 0.4 vertical extent — deliberately as offsets rather than
    // angles, so the corpus needs no trigonometry and therefore cannot smuggle a
    // platform libm into a certified golden. The apex half-angles are
    // `atan(dx / 0.4)` = 40.4°, 15.4° and 2.9°, so the miter lengths `1/sin(θ/2)`
    // are **1.54, 3.77 and 20.02** half-widths. That straddles
    // `MITER_LIMIT = √10 ≈ 3.1623`: the first miters and the other two escape to
    // a bevel. A corpus without that third column would lock the limit's absence
    // rather than its behaviour.
    let severities = [0.34_f64, 0.11, 0.02];
    // The shapes are closed triangles rather than open Vs, which is more
    // coverage and not less: a closed outline has a **wrap join** at the point
    // where its last segment meets its first, and `join_wedges` names that as
    // the one corner a naive implementation leaves round under a miter setting.
    for (row, joint) in joints.into_iter().enumerate() {
        for (col, dx) in severities.into_iter().enumerate() {
            let x = -2.1 + 1.4 * col as f64;
            let y = -1.2 + 0.62 * row as f64;
            let m = add(
                &mut stage,
                Polygon::new([
                    [x - dx, y - 0.2, 0.0],
                    [x, y + 0.2, 0.0],
                    [x + dx, y - 0.2, 0.0],
                ])
                .color(WHITE),
            );
            // 14 width units is 8.4 px at this scale, so a miter extension of up
            // to `MITER_LIMIT` half-widths reaches ~13 px past the corner — a
            // difference a reviewer can see and a golden can lose. At the 6
            // this fixture started with, all four settings sat inside the AA
            // band and the corpus locked a picture the joint could not move.
            stage.set_stroke(m, Some(WHITE), Some(14.0), Some(1.0), None, true);
            stage.set_fill(m, None, Some(0.0), None, true);
            stage.uniforms_mut(m).expect("live").joint_type = joint;
        }
    }
    // Open ends, whose distance to a pixel is radial and whose level set is
    // therefore a semicircle — round caps, with nobody asking for one. Three
    // widths, because a cap is only visible at the width it is drawn at.
    for (i, w) in [3.0_f64, 10.0, 24.0].into_iter().enumerate() {
        let x = -2.1 + 1.4 * i as f64;
        let m = add(
            &mut stage,
            Line::new([x - 0.42, 1.25, 0.0], [x + 0.42, 1.25, 0.0]).color(TEAL_B),
        );
        stage.set_stroke(m, Some(TEAL_B), Some(w), Some(1.0), None, true);
    }
    stage
}

/// §10.2's fill corpus: flat, gradient, a winding hole, both hinted routes, and
/// the inner border.
fn fills() -> Stage {
    let mut stage = Stage::new();

    // A flat opaque disc, and a translucent one overlapping it: the composite.
    let a = add(
        &mut stage,
        Circle::new().radius(0.62).arc_center([-1.85, -0.5, 0.0]),
    );
    stage.set_fill(a, Some(BLUE_C), Some(1.0), Some(0.0), true);
    stage.set_stroke(a, None, Some(0.0), Some(0.0), None, true);
    let b = add(
        &mut stage,
        Circle::new().radius(0.52).arc_center([-1.45, -0.2, 0.0]),
    );
    stage.set_fill(b, Some(RED_C), Some(0.55), Some(0.0), true);
    stage.set_stroke(b, None, Some(0.0), Some(0.0), None, true);

    // A gradient fill: the ramp's two ends differ, so the mean-value field runs.
    let g = add(
        &mut stage,
        Circle::new().radius(0.7).arc_center([-0.35, 0.35, 0.0]),
    );
    stage.set_fill(g, Some(YELLOW_C), Some(0.9), Some(0.0), true);
    stage.set_stroke(g, None, Some(0.0), Some(0.0), None, true);
    gradient(&mut stage, g, RED_C);

    // The same, with an inner border: the seam goes crisp inside the band and
    // the silhouette does not move (ADR-0011).
    let bordered = add(
        &mut stage,
        Circle::new().radius(0.7).arc_center([1.1, 0.35, 0.0]),
    );
    stage.set_fill(bordered, Some(YELLOW_C), Some(0.9), Some(2.0), true);
    stage.set_stroke(bordered, None, Some(0.0), Some(0.0), None, true);
    gradient(&mut stage, bordered, RED_C);

    // The hinted routes: a Dot takes the radial kernel, a Rectangle the box one.
    let d = add(
        &mut stage,
        Dot::new().point([-0.35, -0.9, 0.0]).radius(0.16),
    );
    stage.set_fill(d, Some(GREEN_B), Some(1.0), Some(0.0), true);
    let r = add(
        &mut stage,
        Rectangle::new().width(1.1).height(0.55).color(MAROON_C),
    );
    stage.shift(r, [1.15, -0.85, 0.0]);
    stage.set_fill(r, Some(MAROON_C), Some(1.0), Some(0.0), true);
    stage.set_stroke(r, None, Some(0.0), Some(0.0), None, true);

    // A **self-intersecting** fill through the general quadratic machinery.
    //
    // Without this the corpus barely exercises §10.2's centrepiece: at 60 px per
    // unit every `Circle` here is inside `HINT_BUDGET_PX`, so it takes the disc
    // kernel, and the general path was left to the annuli alone. A pentagram is
    // the right shape for the gap — its centre pentagon has winding **2**, so it
    // fills under the nonzero rule and would be a hole under even-odd, which
    // makes it a discriminator for the rule rather than merely a workout for the
    // evaluator. §10.2 names "winding-consistent self-intersection behavior"
    // explicitly, and this is the only fixture that can fail it.
    //
    // The vertices are literals rather than a trig loop, for the reason the
    // cap/join grid gives: a certified golden must not compute its own geometry
    // with a platform libm.
    let star = add(
        &mut stage,
        Polygon::new([
            [-1.85, 1.27, 0.0],
            [-2.173282, 0.275041, 0.0],
            [-1.326919, 0.889959, 0.0],
            [-2.373081, 0.889959, 0.0],
            [-1.526718, 0.275041, 0.0],
        ])
        .color(GREEN_B),
    );
    stage.set_fill(star, Some(GREEN_B), Some(0.85), Some(0.0), true);
    stage.set_stroke(star, None, Some(0.0), Some(0.0), None, true);

    // A winding hole. It has to be **one** mobject with two counter-wound
    // subpaths — an `Annulus`, which is exactly that — because two overlapping
    // mobjects test compositing rather than winding, and a second shape filled
    // at zero opacity tests nothing at all. A crossing-order or orientation bug
    // shows up here as a solid disc.
    let ring = add(
        &mut stage,
        fmn_library::Annulus::new()
            .inner_radius(0.3)
            .outer_radius(0.62)
            .center([2.1, -0.2, 0.0]),
    );
    stage.set_fill(ring, Some(TEAL_B), Some(1.0), Some(0.0), true);
    stage.set_stroke(ring, None, Some(0.0), Some(0.0), None, true);

    stage
}

/// Give a mobject a fill ramp by writing the last record's colour.
///
/// The IR carries the ramp's two ends, which is how the Reference expresses a
/// gradient along a path; setting only the last record is what makes the two
/// ends differ and therefore what turns the interior field on.
fn gradient(stage: &mut Stage, mob: Mob, end: Srgb) {
    let Some(entry) = stage.get_mut(mob) else {
        return;
    };
    let n = entry.buffer.len();
    if n == 0 {
        return;
    }
    entry.buffer.write(
        n - 1,
        "fill_rgba",
        &[end.r as f32, end.g as f32, end.b as f32, 0.9],
    );
}

/// The frame that stands where G0-6's does: everything fm-zn9 named, in the
/// engine's own terms.
fn composite() -> Stage {
    let mut stage = Stage::new();

    // A gradient-filled disc.
    let disc = add(
        &mut stage,
        Circle::new().radius(0.85).arc_center([-1.6, -0.15, 0.0]),
    );
    stage.set_fill(disc, Some(BLUE_C), Some(0.85), Some(0.0), true);
    stage.set_stroke(disc, None, Some(0.0), Some(0.0), None, true);
    gradient(&mut stage, disc, TEAL_B);

    // A gradient-filled ring whose hole depends on the winding rule — the same
    // structure G0-6's frame carried, so an orientation bug moves this hash.
    let ring = add(
        &mut stage,
        fmn_library::Annulus::new()
            .inner_radius(0.28)
            .outer_radius(0.75)
            .center([1.4, 0.1, 0.0]),
    );
    stage.set_fill(ring, Some(YELLOW_C), Some(0.9), Some(0.0), true);
    stage.set_stroke(ring, None, Some(0.0), Some(0.0), None, true);
    gradient(&mut stage, ring, RED_C);

    // A sharp zigzag with a real joint override, so joins reach pixels.
    let zig = add(
        &mut stage,
        Polygon::new([
            [-2.2, 1.05, 0.0],
            [-1.6, 0.55, 0.0],
            [-1.0, 1.05, 0.0],
            [-0.4, 0.55, 0.0],
            [0.2, 1.05, 0.0],
        ])
        .color(MAROON_C),
    );
    stage.set_fill(zig, None, Some(0.0), None, true);
    stage.set_stroke(zig, Some(MAROON_C), Some(7.0), Some(1.0), None, true);
    stage.uniforms_mut(zig).expect("live").joint_type = JointType::Miter;

    // A tapered gradient stroke over the fills, exercising the arc-length ramp.
    let taper = add(
        &mut stage,
        Line::new([-2.3, -1.15, 0.0], [2.3, -0.85, 0.0]).color(GREEN_B),
    );
    stage.set_stroke(taper, Some(GREEN_B), Some(13.0), Some(0.95), None, true);
    taper_stroke(&mut stage, taper, BLUE_C, 1.0);

    // A stroke drawn behind its own translucent fill (R-5's other branch).
    let behind = add(
        &mut stage,
        Circle::new().radius(0.5).arc_center([0.0, -0.1, 0.0]),
    );
    stage.set_fill(behind, Some(WHITE), Some(0.45), Some(0.0), true);
    stage.set_stroke(behind, Some(RED_C), Some(14.0), Some(1.0), Some(true), true);

    // A hairline finer than the AA band, so the sub-pixel regime is in the hash.
    let hair = add(
        &mut stage,
        Line::new([-2.4, 1.35, 0.0], [2.4, 1.28, 0.0]).color(WHITE),
    );
    stage.set_stroke(hair, Some(WHITE), Some(0.45), Some(1.0), None, true);

    stage
}

/// Give a stroke a width and colour taper by writing the last record.
fn taper_stroke(stage: &mut Stage, mob: Mob, end: Srgb, end_width: f64) {
    let Some(entry) = stage.get_mut(mob) else {
        return;
    };
    let n = entry.buffer.len();
    if n == 0 {
        return;
    }
    entry.buffer.write(
        n - 1,
        "stroke_rgba",
        &[end.r as f32, end.g as f32, end.b as f32, 0.4],
    );
    entry
        .buffer
        .write(n - 1, "stroke_width", &[end_width as f32]);
}

/// The corpus, by locked artifact name.
fn corpus() -> Vec<(&'static str, Stage)> {
    vec![
        ("caps_and_joins.v1", caps_and_joins()),
        ("fills.v1", fills()),
        ("composite.v1", composite()),
    ]
}

// ---------------------------------------------------------------- the gates

#[test]
fn the_corpus_is_bit_locked_across_the_certified_matrix() {
    let mut failures = Vec::new();
    for (name, stage) in corpus() {
        let doc = definition(&stage);
        assert!(
            doc.len() > 1000,
            "{name} encoded to {} bytes — that is not a frame",
            doc.len()
        );
        dump(name, &doc);
        // Every entry is checked before any is reported. A loop that panicked on
        // the first drift would hide the shape of a regression — one frame moving
        // is a bug in one kernel, three moving is a bug in the composite — and it
        // would write only the first `.actual` sidecar, which is the one a
        // reviewer needs least.
        if let Err(e) = store().check(name, &doc) {
            failures.push(e.to_string());
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Write a frame's canonical document to `$FMN_DUMP_FRAMES` when it is set.
///
/// The bytes a golden locks are the bytes a reviewer needs when it moves, and
/// `.actual` sidecars only appear on drift. This makes them available on demand
/// — for a Look Gallery panel, a repro bundle, or an eyeball before a bless —
/// without a second, private path to the pixels.
fn dump(name: &str, doc: &[u8]) {
    let Ok(dir) = std::env::var("FMN_DUMP_FRAMES") else {
        return;
    };
    let dir = PathBuf::from(dir);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join(format!("{name}.frame")), doc);
    }
}

#[test]
fn every_locked_frame_is_thread_count_invariant() {
    // PG-5, per commit: "bit-identical raw frames across runs, {1,4,16} threads".
    // Thread count is one of the things §16.7 declares *outside* the input
    // closure — "proven inert under §10.5" — and this is the proof.
    for (name, stage) in corpus() {
        let one = definition(&stage);
        for threads in [4usize, 16] {
            assert!(
                render(&stage, Tier::Scalar, threads) == one,
                "{name} moved at {threads} threads"
            );
        }
    }
}

#[test]
fn every_certified_frame_ignores_adaptive_and_forced_aa_selection() {
    // §10.4: classification, forced 2× and forced 4× are standard-quality
    // choices. A certified run executes the canonical analytic path under every
    // requested A/B policy and therefore produces the same raw-frame document.
    for (name, stage) in corpus() {
        let analytic = definition(&stage);
        for aa in [AaPolicy::Adaptive, AaPolicy::Ssaa2x, AaPolicy::Ssaa4x] {
            assert_eq!(
                render_with_aa(&stage, Tier::Scalar, 4, aa),
                analytic,
                "{name} moved under certified {aa:?}"
            );
        }
    }
}

#[test]
fn every_tier_reproduces_the_scalar_definition() {
    // §10.5: "within the certified engine the scalar path is the definition and
    // every SIMD tier must match it bit-for-bit."
    //
    // Today `Tier::ALL` holds one entry, so this sweep compares scalar with
    // itself — and saying so plainly is better than implying a coverage that
    // does not exist. What the harness buys is that fm-4wt's first tier is
    // checked by construction: it lands in `Tier::ALL`, this sweep renders the
    // whole corpus through it, and a tier whose arithmetic diverges fails here
    // rather than in a Look Gallery review. ADR-0010 names the SIMD tiers as the
    // remaining risk; this is the tripwire it asked for.
    assert_eq!(
        Tier::ALL.first(),
        Some(&Tier::Scalar),
        "the scalar tier must be first — it is the definition, not a member"
    );
    for (name, stage) in corpus() {
        let scalar = definition(&stage);
        for &tier in Tier::ALL {
            assert!(
                render(&stage, tier, 1) == scalar,
                "{name} differs between the scalar definition and the {} tier",
                tier.name()
            );
        }
    }
}

#[test]
fn a_frame_is_reproducible_within_a_run() {
    // If this fails, the engine has in-process nondeterminism and every
    // cross-platform claim above it is meaningless.
    for (name, stage) in corpus() {
        assert!(
            definition(&stage) == definition(&stage),
            "{name} is unstable"
        );
    }
}

#[test]
fn the_corpus_actually_exercises_what_it_claims() {
    // A golden corpus that quietly stopped drawing half its content would keep
    // passing forever. Each entry is checked for the structure its name promises.
    let cfg = config();

    let mut plan = RenderPlan::new();
    plan.sync(&caps_and_joins(), 0);
    assert_eq!(
        plan.shapes().instances().len(),
        15,
        "four joint settings x three severities, plus three round-capped ends"
    );
    let joints: std::collections::BTreeSet<i64> = plan
        .styles()
        .rows()
        .iter()
        .map(|s| s.joint_type.to_code() as i64)
        .collect();
    assert_eq!(joints.len(), 4, "not every joint setting is in the corpus");

    // Present in the style table is not the same as visible in the frame. If the
    // corners were too shallow or the stroke too thin, all four settings would
    // render inside the antialiasing band and the golden would be locking a
    // picture no joint setting could move — a fixture that passes forever and
    // guards nothing. So: force every joint to `Auto` (the round field the
    // distance function already produces) and require the frame to change.
    let all_joints = |joint: JointType| {
        let mut s = caps_and_joins();
        let mobs: Vec<Mob> = s.roots().to_vec();
        for m in mobs {
            if let Some(u) = s.uniforms_mut(m) {
                u.joint_type = joint;
            }
        }
        definition(&s)
    };
    assert_ne!(
        definition(&caps_and_joins()),
        all_joints(JointType::Auto),
        "no joint setting reaches a pixel: the cap/join corpus guards nothing"
    );

    // ADR-0012's ruling, at the frame level: `Auto` and `NoJoint` both render the
    // round join the distance field already produces, so `join_wedges` returns
    // nothing for either and the two must be **byte**-identical — not similar.
    // `fmn_render::stroke` asserts this per pixel on one corner; here it is the
    // whole corpus, including the wrap joins of twelve closed outlines.
    assert_eq!(
        all_joints(JointType::Auto),
        all_joints(JointType::NoJoint),
        "the round settings diverged, so the join machinery is not inert for them"
    );
    // And the two real overrides each move the picture, in different ways.
    assert_ne!(all_joints(JointType::Auto), all_joints(JointType::Bevel));
    assert_ne!(all_joints(JointType::Auto), all_joints(JointType::Miter));
    assert_ne!(all_joints(JointType::Bevel), all_joints(JointType::Miter));

    let mut plan = RenderPlan::new();
    plan.sync(&fills(), 0);
    assert!(
        plan.styles()
            .rows()
            .iter()
            .any(|s| !fmn_render::fill::fill_is_flat(s)),
        "no gradient fill, so the interior field is untested"
    );
    assert!(
        plan.styles()
            .rows()
            .iter()
            .any(|s| s.fill_border_width > 0.0),
        "no inner border, so ADR-0011's band is untested"
    );
    let hinted = plan
        .shapes()
        .shapes()
        .iter()
        .filter(|s| !s.hint.is_general())
        .count();
    assert!(hinted >= 2, "the hinted fill routes are not exercised");
    // And the general machinery, which is easy to lose: at this scale every
    // `Circle` is inside the hint budget, so without a deliberately unhinted
    // shape the corpus would exercise §10.2's centrepiece almost not at all.
    let general = plan
        .shapes()
        .shapes()
        .iter()
        .filter(|s| s.hint.is_general())
        .count();
    assert!(
        general >= 2,
        "the general quadratic fill is barely exercised: {general} unhinted shapes"
    );

    let mut plan = RenderPlan::new();
    plan.sync(&composite(), 0);
    assert!(
        plan.styles().rows().iter().any(|s| s.stroke_behind),
        "no backstroke, so R-5's other branch is untested"
    );
    assert!(
        plan.styles()
            .rows()
            .iter()
            .any(|s| s.stroke_width != s.stroke_width_end),
        "no width taper, so the arc-length ramp is untested"
    );
    // And every corpus frame must be binned somewhere: a scene that misses the
    // viewport renders a background nobody would notice was empty.
    for (name, stage) in corpus() {
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let mono = MonoTable::build(&plan, cfg.map);
        let binning = Binning::build(&plan, cfg.viewport, TILING, cfg.map);
        assert!(!binning.draws().is_empty(), "{name} binned to nothing");
        assert!(!mono.is_empty(), "{name} compiled no fill geometry");
    }
}

#[test]
fn the_engine_identity_reaches_the_input_closure() {
    // §10.5(f): "every engine/backend identity is journaled into the input
    // closure". C7 is what that becomes, and this is the value it contributes.
    let cfg = config();
    let certified = journal_digest(EngineIdentity::certified(), &cfg, TILING);
    assert_eq!(
        certified,
        journal_digest(EngineIdentity::certified(), &cfg, TILING),
        "the identity digest is not stable"
    );
    assert!(EngineIdentity::certified().engine.certifiable());

    // The declared configuration is in it, so a run that changed the tile
    // dimensions cannot present a manifest claiming the same inputs.
    let other = Tiling {
        macro_tile: 128,
        fine_tile: 32,
    };
    assert_ne!(
        certified,
        journal_digest(EngineIdentity::certified(), &cfg, other)
    );

    // A frame's job reports the same value the free function does, so nothing
    // downstream can journal an identity the render did not use.
    let stage = fills();
    let mut plan = RenderPlan::new();
    plan.sync(&stage, 0);
    let mono = MonoTable::build(&plan, cfg.map);
    let binning = Binning::build(&plan, cfg.viewport, TILING, cfg.map);
    let job = FrameJob::new(&plan, &mono, &binning, cfg).expect("matching frame artifacts");
    assert_eq!(job.journal_digest(), certified);
    assert_eq!(job.identity(), EngineIdentity::certified());
}

#[test]
fn the_background_is_the_references_and_is_decoded_once() {
    // The corpus renders on #333333, and 0x33 is emphatically not 0.2 in linear
    // light. Getting this wrong is the failure mode BN-04's decode step exists
    // to prevent, and it would be invisible in a golden that had never been
    // right.
    let bg = config().background;
    let expected = fmn_core::color::srgb_eotf(f64::from(0x33u8) / 255.0);
    assert!((bg.r - expected).abs() < 1e-15);
    assert!(
        bg.r < 0.05,
        "0x33 decoded to {} — that is an encoded value, not linear light",
        bg.r
    );
    assert!(
        (bg.a - 1.0).abs() < 1e-15,
        "the corpus background is opaque"
    );
}
