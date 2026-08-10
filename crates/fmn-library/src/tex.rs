//! The Scribe bridge, math half (fm-p5d): a [`fmn_tex::Typeset`]
//! becomes a [`VMobject`] family — **one child per `Sub`** — with the
//! span map intact.
//!
//! The contract (§11.3–11.5): child `i` is `typeset.subs[i]`, so the
//! ordinals [`Typeset::occurrences`] returns are the family's child
//! indices — `isolate=`, `tex_to_color_map`, and
//! `TransformMatchingTex` consume them by source identity, and the
//! Reference's render-twice-and-align hack stays dead. Resolving a
//! whole layout through `fmd_math::paths::resolve_paths` would flatten
//! the per-`Sub` grouping, so each primitive resolves on its own via
//! [`TexEngine::resolve_prim`]: glyphs through the engine's pinned
//! size/upm transform, rules as rectangles, drawn paths (extensible
//! delimiters, radicals, stretchy bands) positioned already.
//!
//! Style follows the Reference's tex mobject — the same defaults as
//! text ([`text_style`]): `stroke_width=0`, `fill_opacity=1.0`,
//! `fill_border_width=0.5`, color WHITE. Scale is calibrated the
//! Reference's way (`tex_mobject.py::get_tex_mob_scale_factor`):
//! typeset a reference "0" and scale so its height is
//! `font_size / font_size_for_unit_height` manim units.

use fmn_core::color::Srgb;
use fmn_core::types::Vec3;
use fmn_geom::QuadPath;
use fmn_mobject::Mobject;
use fmn_tex::{Mode, PathContour, PathSeg, Style as MathStyle, TexEngine, TexError, Typeset};

use crate::style::Style;
use crate::text::{DEFAULT_FONT_SIZE, DEFAULT_FONT_SIZE_FOR_UNIT_HEIGHT, text_style};
use crate::vmobject::VMobject;

/// A tex-bridge failure: fmd-math's precise, tier-tagged construct
/// errors pass through untouched (never a blank render); the bridge
/// itself only names its own two faults.
#[derive(Debug)]
pub enum TexMobjectError {
    /// The fmn-tex pipeline's precise error, verbatim.
    Tex(TexError),
    /// The calibration probe found no measurable "0" — the math face
    /// roster maps no digit zero (build corruption).
    Calibration,
    /// A contour could not be committed to a [`QuadPath`] — unreachable
    /// in practice (a subpath always starts before any segment is
    /// appended).
    Geometry {
        /// The geometry kernel's report.
        what: String,
    },
}

impl core::fmt::Display for TexMobjectError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Tex(e) => e.fmt(f),
            Self::Calibration => write!(
                f,
                "the math face roster maps no measurable \"0\" glyph; \
                 tex scale calibration is impossible"
            ),
            Self::Geometry { what } => write!(f, "math contour commit failed: {what}"),
        }
    }
}

impl std::error::Error for TexMobjectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Tex(e) => Some(e),
            _ => None,
        }
    }
}

impl From<TexError> for TexMobjectError {
    fn from(e: TexError) -> Self {
        Self::Tex(e)
    }
}

/// A built tex mobject: the [`VMobject`] family plus the typeset that
/// produced it — the span map `isolate=` / `tex_to_color_map` /
/// `TransformMatchingTex` consume (§11.3).
#[derive(Debug, Clone)]
pub struct TexMobject {
    /// The family: child `i` is `typeset.subs[i]` — one child per glyph,
    /// per rule, per drawn path, in emission order.
    pub vmob: VMobject,
    /// The typeset: the source, the layout, and the submobject table,
    /// intact.
    pub typeset: Typeset,
}

impl TexMobject {
    /// The `isolate=` surface: the child ordinals selected by each
    /// occurrence of `needle` in the source, by source identity.
    #[must_use]
    pub fn occurrences(&self, needle: &str) -> Vec<Vec<usize>> {
        self.typeset.occurrences(needle)
    }

    /// The number of submobjects (`len(Tex(...))`).
    #[must_use]
    pub fn len(&self) -> usize {
        self.typeset.subs.len()
    }

    /// True when the typeset produced no primitives.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl From<TexMobject> for Mobject {
    fn from(t: TexMobject) -> Self {
        t.vmob.into()
    }
}

/// `Tex` (Appendix A `mobject/svg/tex_mobject`): the math builder over
/// [`TexEngine`]. `TexText` (the same module's sibling) is the
/// text-mainland mode.
#[derive(Debug, Clone)]
pub struct Tex<'a> {
    source: &'a str,
    mode: Mode,
    font_size: f64,
    font_size_for_unit_height: f64,
    style: Style,
    t2c: &'a [(&'a str, Srgb)],
}

impl<'a> Tex<'a> {
    /// A `Tex` with the Reference's defaults: text-style mathematics,
    /// font size 48 over 144-per-unit, the tex style, no color map.
    #[must_use]
    pub fn new(source: &'a str) -> Self {
        Self {
            source,
            mode: Mode::Math(MathStyle::Text),
            font_size: DEFAULT_FONT_SIZE,
            font_size_for_unit_height: DEFAULT_FONT_SIZE_FOR_UNIT_HEIGHT,
            style: text_style(),
            t2c: &[],
        }
    }

    /// Display-style mathematics (`\displaystyle`).
    #[must_use]
    pub fn display(mut self) -> Self {
        self.mode = Mode::Math(MathStyle::Display);
        self
    }

    /// The outer math style explicitly.
    #[must_use]
    pub fn math_style(mut self, style: MathStyle) -> Self {
        self.mode = Mode::Math(style);
        self
    }

    /// The `font_size=` surface.
    #[must_use]
    pub fn font_size(mut self, font_size: f64) -> Self {
        self.font_size = font_size;
        self
    }

    /// The config's `tex.font_size_for_unit_height` — the font size at
    /// which "0" stands one manim unit tall.
    #[must_use]
    pub fn font_size_for_unit_height(mut self, fsuh: f64) -> Self {
        self.font_size_for_unit_height = fsuh;
        self
    }

    /// Replace the base style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// `tex_to_color_map` (`t2c`), applied by source identity through
    /// the span map; later entries win within the map.
    #[must_use]
    pub fn t2c(mut self, t2c: &'a [(&'a str, Srgb)]) -> Self {
        self.t2c = t2c;
        self
    }

    /// Typeset and build the family.
    ///
    /// # Errors
    ///
    /// [`TexMobjectError::Tex`]: an unsupported construct is fmd-math's
    /// precise, named, tier-tagged error at construction time — never
    /// silence, never garbage. [`TexMobjectError::Calibration`]: the
    /// math face roster maps no measurable "0".
    pub fn build(&self, engine: &TexEngine) -> Result<TexMobject, TexMobjectError> {
        let typeset = engine.typeset(self.mode, self.source)?;
        let scale = calibrate(engine, self.font_size, self.font_size_for_unit_height)?;
        // tex_to_color_map, resolved to child ordinals before
        // construction: later entries win (the Reference's dict-update).
        let mut fills: Vec<Option<Srgb>> = vec![None; typeset.subs.len()];
        for (needle, color) in self.t2c {
            for occurrence in typeset.occurrences(needle) {
                for ord in occurrence {
                    fills[ord] = Some(*color);
                }
            }
        }
        let mut children = Vec::with_capacity(typeset.subs.len());
        for (ord, sub) in typeset.subs.iter().enumerate() {
            let mut style = self.style;
            if let Some(fill) = fills[ord] {
                style.fill_color = fill;
            }
            children.push(prim_child(engine, &typeset, sub, style, scale)?);
        }
        let vmob = VMobject::new()
            .with_style(self.style)
            .with_children(children);
        Ok(TexMobject { vmob, typeset })
    }
}

/// `TexText` (Appendix A `mobject/svg/tex_mobject`): the text-mainland
/// sibling of [`Tex`] — prose with `$…$` math islands.
#[derive(Debug, Clone)]
pub struct TexText<'a> {
    inner: Tex<'a>,
}

impl<'a> TexText<'a> {
    /// A `TexText` with the Reference's defaults.
    #[must_use]
    pub fn new(source: &'a str) -> Self {
        Self {
            inner: Tex {
                mode: Mode::Text,
                ..Tex::new(source)
            },
        }
    }

    /// The `font_size=` surface.
    #[must_use]
    pub fn font_size(mut self, font_size: f64) -> Self {
        self.inner = self.inner.font_size(font_size);
        self
    }

    /// The config's `tex.font_size_for_unit_height`.
    #[must_use]
    pub fn font_size_for_unit_height(mut self, fsuh: f64) -> Self {
        self.inner = self.inner.font_size_for_unit_height(fsuh);
        self
    }

    /// Replace the base style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.inner = self.inner.style(style);
        self
    }

    /// `tex_to_color_map` (`t2c`).
    #[must_use]
    pub fn t2c(mut self, t2c: &'a [(&'a str, Srgb)]) -> Self {
        self.inner = self.inner.t2c(t2c);
        self
    }

    /// Typeset and build the family.
    ///
    /// # Errors
    ///
    /// As [`Tex::build`].
    pub fn build(&self, engine: &TexEngine) -> Result<TexMobject, TexMobjectError> {
        self.inner.build(engine)
    }
}

/// The ems→scene-units scale, calibrated the Reference's way: typeset a
/// reference "0" (text-style math, the Reference's calibration surface)
/// and scale so its height is `font_size / font_size_for_unit_height`.
fn calibrate(engine: &TexEngine, font_size: f64, fsuh: f64) -> Result<f64, TexMobjectError> {
    let probe = engine.typeset(Mode::Math(MathStyle::Text), "0")?;
    let height = probe.layout.height + probe.layout.depth;
    if !height.is_finite() || height <= 0.0 {
        return Err(TexMobjectError::Calibration);
    }
    Ok(font_size / (fsuh * height))
}

/// One `Sub` child: its resolved contours as a positioned [`QuadPath`],
/// scaled to scene units. A primitive with no contours keeps its slot
/// as an empty child — ordinals never shift.
fn prim_child(
    engine: &TexEngine,
    typeset: &Typeset,
    sub: &fmn_tex::Sub,
    style: Style,
    scale: f64,
) -> Result<VMobject, TexMobjectError> {
    let contours = engine.resolve_prim(typeset, sub.prim)?;
    if contours.is_empty() {
        return Ok(VMobject::new().with_style(style));
    }
    let mut path = QuadPath::new();
    for contour in &contours {
        append_contour(&mut path, contour, scale)?;
    }
    Ok(VMobject::from_path(&path).with_style(style))
}

/// Append one resolved contour as a subpath — the break convention
/// (`start_new_path`'s handle-on-anchor marker) is the geometry
/// kernel's own, so counters and multi-contour constructions compile
/// correctly downstream (fm-ig3).
fn append_contour(
    path: &mut QuadPath,
    contour: &PathContour,
    scale: f64,
) -> Result<(), TexMobjectError> {
    let v = |x: f64, y: f64| -> Vec3 { [x * scale, y * scale, 0.0] };
    let geometry = |e: fmn_geom::GeomError| TexMobjectError::Geometry {
        what: format!("{e:?}"),
    };
    path.start_new_path(v(contour.start.0, contour.start.1));
    for seg in &contour.segments {
        match seg {
            PathSeg::Line { to } => {
                path.add_line_to(v(to.0, to.1), true).map_err(geometry)?;
            }
            PathSeg::Quad { ctrl, to } => {
                path.add_quadratic_bezier_curve_to(v(ctrl.0, ctrl.1), v(to.0, to.1), true)
                    .map_err(geometry)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_core::constants::{BLUE, RED};
    use fmn_mobject::Stage;
    use fmn_tex::Prim;

    fn engine() -> TexEngine {
        TexEngine::new("fmd-math/pack/default", None).expect("engine")
    }

    /// The deterministic golden format: one line per point of the
    /// family's whole point runs, fixed six decimals (the convention of
    /// fmd-math's `canonical_dump`).
    fn dump_family(vmob: &VMobject) -> String {
        let mut out = String::new();
        dump_one(vmob, &mut out);
        for child in vmob.children() {
            dump_one(child, &mut out);
        }
        out
    }

    fn dump_one(vmob: &VMobject, out: &mut String) {
        for p in vmob.points() {
            out.push_str(&format!("{:.6} {:.6} {:.6}\n", p[0], p[1], p[2]));
        }
    }

    #[test]
    fn one_child_per_sub_with_the_span_map_intact() {
        let m = Tex::new(r"\frac{a}{b}").build(&engine()).expect("builds");
        assert_eq!(m.vmob.children().len(), m.typeset.subs.len());
        // Every sub is exactly one child, in order; 'a' and 'b' select
        // single children by source identity.
        // 'a' also appears inside "\frac" itself; the containment
        // semantics select nothing for that occurrence (the command's
        // primitives carry the command's whole span), so keep the
        // non-empty selections.
        for (needle, ch) in [("a", 'a'), ("b", 'b')] {
            let occ = m.occurrences(needle);
            let selected: Vec<&Vec<usize>> = occ.iter().filter(|o| !o.is_empty()).collect();
            assert_eq!(selected.len(), 1, "one selecting occurrence of {needle:?}");
            assert_eq!(selected[0].len(), 1, "one glyph for {needle:?}");
            let ord = selected[0][0];
            assert!(
                matches!(m.typeset.subs[ord].prim, fmn_tex::Prim::Glyph(g)
                    if m.typeset.layout.glyphs[g].ch == ch),
                "{needle:?} should be a glyph for {ch:?}, got {:?}",
                m.typeset.subs[ord].prim
            );
            assert!(!m.vmob.children()[ord].points().is_empty());
        }
        // "\frac" itself is not matched by a containment query for "a".
        assert_eq!(m.occurrences("frac").len(), 1);
    }

    #[test]
    fn a_fraction_rule_is_a_rectangle_child() {
        let m = Tex::new(r"\frac{a}{b}").build(&engine()).expect("builds");
        let (rule_ord, r) = m
            .typeset
            .subs
            .iter()
            .enumerate()
            .find_map(|(i, s)| match s.prim {
                Prim::Rule(r) => Some((i, r)),
                _ => None,
            })
            .expect("a fraction has a rule");
        let rule = &m.typeset.layout.rules[r];
        let child = &m.vmob.children()[rule_ord];
        let (min, max) = child.extent().expect("has extent");
        // The child is the rule's rectangle at the calibrated scale:
        // recompute the scale from the "0" probe, exactly as build does.
        let probe = engine()
            .typeset(Mode::Math(MathStyle::Text), "0")
            .expect("probe");
        let scale = DEFAULT_FONT_SIZE
            / (DEFAULT_FONT_SIZE_FOR_UNIT_HEIGHT * (probe.layout.height + probe.layout.depth));
        assert!((min[0] - rule.x * scale).abs() < 1e-9);
        assert!((max[0] - (rule.x + rule.width) * scale).abs() < 1e-9);
        assert!((min[1] - rule.y * scale).abs() < 1e-9);
        assert!((max[1] - (rule.y + rule.height) * scale).abs() < 1e-9);
        let path = child.path().expect("a valid path");
        assert!(path.is_closed(), "a rule rectangle is closed");
    }

    #[test]
    fn a_stretchy_overbrace_is_a_drawn_path_child() {
        // Past the glyph-scaling ceiling, the delimiter engine draws the
        // construction parametrically (ADR-0005): one PlacedPath sub
        // (scribe2's fixture: overbrace = 3 glyphs / 0 rules / 1 path).
        let m = Tex::new(r"\overbrace{abc}")
            .build(&engine())
            .expect("builds");
        let path_ords: Vec<usize> = m
            .typeset
            .subs
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s.prim, Prim::Path(_)))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(path_ords.len(), 1, "one drawn radical path");
        let child = &m.vmob.children()[path_ords[0]];
        assert!(!child.points().is_empty(), "the radical has geometry");
        assert_eq!(
            child.points().len() % 2,
            1,
            "shared-anchor runs have odd length"
        );
    }

    #[test]
    fn t2c_colors_by_source_identity() {
        let t2c = [("x", RED), ("y", BLUE)];
        let m = Tex::new("x+y").t2c(&t2c).build(&engine()).expect("builds");
        let x = &m.occurrences("x")[0];
        let y = &m.occurrences("y")[0];
        let plus = &m.occurrences("+")[0];
        for &ord in x {
            assert_eq!(m.vmob.children()[ord].style().fill_color, RED);
        }
        for &ord in y {
            assert_eq!(m.vmob.children()[ord].style().fill_color, BLUE);
        }
        for &ord in plus {
            assert_eq!(
                m.vmob.children()[ord].style().fill_color,
                fmn_core::constants::WHITE
            );
        }
    }

    #[test]
    fn the_tex_style_is_the_text_style() {
        let m = Tex::new("x").build(&engine()).expect("builds");
        for child in m.vmob.children() {
            let s = child.style();
            assert_eq!(s.stroke_width, 0.0);
            assert_eq!(s.fill_opacity, 1.0);
            assert_eq!(s.fill_border_width, 0.5);
        }
    }

    #[test]
    fn the_calibration_makes_a_digit_font_size_over_fsuh_tall() {
        let engine = engine();
        for font_size in [48.0, 96.0] {
            let m = Tex::new("0")
                .font_size(font_size)
                .build(&engine)
                .expect("builds");
            let (min, max) = m.vmob.children()[0].extent().expect("has extent");
            let height = max[1] - min[1];
            let want = font_size / DEFAULT_FONT_SIZE_FOR_UNIT_HEIGHT;
            assert!(
                (height - want).abs() < 1e-9,
                "font_size {font_size}: \"0\" height {height} != {want}"
            );
        }
    }

    #[test]
    fn display_style_changes_the_layout() {
        let engine = engine();
        let text = Tex::new(r"\frac{a}{b}").build(&engine).expect("builds");
        let display = Tex::new(r"\frac{a}{b}")
            .display()
            .build(&engine)
            .expect("builds");
        let extent = |m: &TexMobject| {
            let (min, max) = m.vmob.extent().expect("has extent");
            max[1] - min[1]
        };
        assert!(
            extent(&display) > extent(&text),
            "display fractions are taller: {} vs {}",
            extent(&display),
            extent(&text)
        );
    }

    #[test]
    fn an_unsupported_construct_is_the_named_tier_tagged_error() {
        // The pending tier-2 example advances as constructs graduate (fm-j5t).
        let err = Tex::new(r"\dx").build(&engine()).expect_err("fails");
        assert!(
            matches!(&err, TexMobjectError::Tex(TexError::Math(_))),
            "expected a math error, got {err:?}"
        );
        // TexMobjectError's Display delegates to the MathError verbatim.
        let what = err.to_string();
        assert!(what.contains("\\dx"), "names the construct: {what}");
        assert!(what.contains("tier"), "carries the tier tag: {what}");
    }

    #[test]
    fn an_unmapped_char_is_the_named_error() {
        let err = Tex::new("x 🦀 y").build(&engine()).expect_err("fails");
        assert!(
            matches!(&err, TexMobjectError::Tex(TexError::Math(_))),
            "expected a math error, got {err:?}"
        );
        let what = err.to_string();
        assert!(what.contains('🦀'), "names the char: {what}");
    }

    #[test]
    fn textext_mixes_prose_and_math_islands() {
        let m = TexText::new("a $b$ c").build(&engine()).expect("builds");
        assert_eq!(m.vmob.children().len(), m.typeset.subs.len());
        // The island glyph is found by source identity.
        let occ = m.occurrences("b");
        assert_eq!(occ.len(), 1);
        assert!(!m.vmob.children()[occ[0][0]].points().is_empty());
    }

    #[test]
    fn resolve_prim_names_a_bad_index() {
        let engine = engine();
        let m = Tex::new("x").build(&engine).expect("builds");
        let err = engine
            .resolve_prim(&m.typeset, Prim::Glyph(999))
            .expect_err("out of range");
        assert!(
            matches!(&err, TexError::BadPrim { .. }),
            "expected BadPrim, got {err:?}"
        );
        let what = err.to_string();
        assert!(what.contains("999"), "names the index: {what}");
    }

    #[test]
    fn the_family_enters_the_arena_as_a_family() {
        let mut stage = Stage::new();
        let m = Tex::new("xy").build(&engine()).expect("builds");
        let n = m.vmob.children().len();
        let mob = stage.add(m.vmob);
        assert_eq!(stage.family(mob).len(), n + 1);
    }

    /// Golden maintenance: `cargo test -p fmn-library -- --ignored
    /// regenerate_tex_goldens` rewrites the golden files after a
    /// deliberate fmd-math / fmd-font pin move. The regenerated files
    /// are review material — never regenerate casually.
    #[test]
    #[ignore]
    fn regenerate_tex_goldens() {
        let engine = engine();
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens");
        std::fs::create_dir_all(dir).expect("goldens dir");
        let zero = Tex::new("0").build(&engine).expect("builds");
        std::fs::write(format!("{dir}/tex_zero.txt"), dump_family(&zero.vmob))
            .expect("write tex_zero");
        let island = TexText::new("a $b$").build(&engine).expect("builds");
        std::fs::write(
            format!("{dir}/textext_island.txt"),
            dump_family(&island.vmob),
        )
        .expect("write textext_island");
    }

    #[test]
    fn tex_zero_golden() {
        let m = Tex::new("0").build(&engine()).expect("builds");
        assert_eq!(m.vmob.children().len(), 1);
        let dump = dump_family(&m.vmob);
        let expected = include_str!("../tests/goldens/tex_zero.txt");
        assert_eq!(
            dump, expected,
            "golden drift (see tests/goldens/tex_zero.txt)"
        );
    }

    #[test]
    fn textext_island_golden() {
        let m = TexText::new("a $b$").build(&engine()).expect("builds");
        let dump = dump_family(&m.vmob);
        let expected = include_str!("../tests/goldens/textext_island.txt");
        assert_eq!(
            dump, expected,
            "golden drift (see tests/goldens/textext_island.txt)"
        );
    }
}
