//! `DecimalNumber` and `Integer` — pure native text (§11.6, fm-ebl).
//!
//! The Reference builds a decimal readout from one cached `Text(char)`
//! mobject per displayed character, arranged `RIGHT` with a per-font-unit
//! buff, and rebuilds the submobject list on every `set_value` — reusing
//! the cached glyph mobjects so an update inside the already-seen alphabet
//! typesets nothing new. This module ports that design literally: the
//! struct owns a char → glyph-child cache (each entry the single-glyph
//! child of a one-character [`Text`] build, standing at the origin) and a
//! `typesets` counter that advances only when a *new* character enters the
//! cache.
//!
//! The formatting pipeline is CPython's `str.format` semantics, not a
//! resemblance of them, because the Reference's `get_formatter` *is* a
//! Python format spec: round-half-even at the requested precision,
//! truncation toward zero when `num_decimal_places=0`, negative-zero
//! suppression against `np.round`, `,` grouping every three integer digits,
//! and `0N` zero-padding that pads the digit run (re-grouping included)
//! until the total width is reached — overshoot and all
//! (`format(123456, "08,d") == "0,123,456"` is nine wide). The minus sign
//! then becomes U+2013 EN DASH, and the dash is re-seated to mid-digit
//! height exactly the Reference's way (aligned `UP` to the next glyph,
//! then shifted down half the next glyph's height).
//!
//! Deliberate divergences from the Reference (all BN-08 territory):
//!
//! * **No complex values.** `DecimalNumber` is `f64`; the complex
//!   formatter and `hide_zero_components_on_complex` are not ported.
//! * **`unit` is plain text.** The Reference routes `^\circ` through
//!   `Tex`; here a leading `^` is an alignment marker (stripped before
//!   typesetting, unit aligned `UP`), and TeX commands are not
//!   interpreted — write the glyph itself (`"°"`).
//! * **The ellipsis is the font's U+2026 glyph**, one child, where the
//!   Reference typesets three periods with hand-tuned spacing as one
//!   submobject. The one-child contract is preserved.
//! * **No scale side-effects.** The Reference mutates `font_size` when
//!   the mobject is scaled; here `font_size` is a construction
//!   parameter, and style/font setters invalidate the glyph cache
//!   (re-applying on the next `set_value`) rather than restyling cached
//!   geometry.
//!
//! The background rectangle composes [`matchers::background_rectangle`]
//! at child 0, BLACK at full opacity — the Reference's
//! `add_background_rectangle()` defaults (camera background, `buff=0`).

use std::collections::HashMap;
use std::fmt::Write as _;

use fmn_core::color::Srgb;
use fmn_core::constants::{BLACK, DOWN, LEFT, RIGHT, UP};
use fmn_core::types::Vec3;
use fmn_text::FontBook;

use crate::matchers;
use crate::style::Style;
use crate::text::{Text, TextMobjectError, text_style};
use crate::vmobject::VMobject;

/// The Reference's minus glyph: `-` is replaced by U+2013 before display.
const EN_DASH: char = '\u{2013}';

/// The `show_ellipsis` glyph: one child, the font's own U+2026.
const ELLIPSIS: char = '\u{2026}';

/// Default upper bound for characters produced by one native number
/// layout.
///
/// The limit covers numeric glyphs, an optional ellipsis, and unit
/// glyphs. It is deliberately far above ordinary scene labels while
/// preventing public precision/width inputs from becoming unbounded
/// formatting or child-layout work.
pub const DEFAULT_MAX_NUMBER_CHARACTERS: usize = 4_096;

/// `DecimalNumber` (Appendix A `mobject/numbers`): a native decimal
/// readout — one glyph child per displayed character, a per-character
/// glyph cache, and `set_value` updates that typeset only new characters.
///
/// The constructor surface is the Reference's
/// (`scripts/manim_ref/manimlib/mobject/numbers.py`): `new(number)` takes
/// the Reference defaults, the by-value setters mirror the keyword
/// arguments, and [`DecimalNumber::build`] typesets against the book.
/// Setters called *after* `build` re-apply on the next
/// [`DecimalNumber::set_value`] (style and font setters invalidate the
/// glyph cache; formatting setters keep it).
#[derive(Debug, Clone)]
pub struct DecimalNumber {
    // --- the Reference's constructor surface ---------------------------
    number: f64,
    style: Style,
    num_decimal_places: usize,
    min_total_width: usize,
    include_sign: bool,
    group_with_commas: bool,
    digit_buff_per_font_unit: f64,
    show_ellipsis: bool,
    unit: Option<String>,
    include_background_rectangle: bool,
    edge_to_fix: Vec3,
    font_size: f64,
    font_size_for_unit_height: f64,
    character_limit: usize,
    // --- built state -----------------------------------------------------
    /// The family: one child per displayed character (ellipsis and unit
    /// trailing as one child each), the background rectangle at child 0
    /// when configured.
    vmob: VMobject,
    /// The displayed string, en-dash substituted (ellipsis/unit excluded,
    /// exactly the Reference's `self.num_string`).
    num_string: String,
    /// char → the glyph child at the origin, styled with `self.style`.
    /// The style is invariant between cache clears, so the char alone is
    /// the key; style/font setters clear the cache rather than restyle.
    cache: HashMap<char, VMobject>,
    /// Cumulative count of char typesets since construction — the
    /// glyph-recycling witness.
    typesets: usize,
}

impl DecimalNumber {
    /// The Reference defaults: `num_decimal_places=2`,
    /// `min_total_width=0`, no sign, comma grouping on,
    /// `digit_buff_per_font_unit=0.001`, no ellipsis, no unit, no
    /// background rectangle, `edge_to_fix=LEFT`, `font_size=48` over
    /// 144-per-unit, stroke 0 / fill 1 / border 0.5 in the default color.
    #[must_use]
    pub fn new(number: f64) -> Self {
        Self {
            number,
            style: text_style(),
            num_decimal_places: 2,
            min_total_width: 0,
            include_sign: false,
            group_with_commas: true,
            digit_buff_per_font_unit: 0.001,
            show_ellipsis: false,
            unit: None,
            include_background_rectangle: false,
            edge_to_fix: LEFT,
            font_size: crate::text::DEFAULT_FONT_SIZE,
            font_size_for_unit_height: crate::text::DEFAULT_FONT_SIZE_FOR_UNIT_HEIGHT,
            character_limit: DEFAULT_MAX_NUMBER_CHARACTERS,
            vmob: VMobject::new(),
            num_string: String::new(),
            cache: HashMap::new(),
            typesets: 0,
        }
    }

    /// `num_decimal_places=` — rounding precision; `0` switches to the
    /// integer formatter (truncation toward zero, the Reference's
    /// `int(number)`).
    #[must_use]
    pub fn num_decimal_places(mut self, ndp: usize) -> Self {
        self.num_decimal_places = ndp;
        self
    }

    /// `min_total_width=` — zero-pad (re-grouping included, CPython's
    /// overshoot semantics) until the whole string is this wide.
    #[must_use]
    pub fn min_total_width(mut self, width: usize) -> Self {
        self.min_total_width = width;
        self
    }

    /// Maximum characters admitted for native number formatting and
    /// layout.
    ///
    /// The default is [`DEFAULT_MAX_NUMBER_CHARACTERS`]. Raising it is an
    /// explicit opt-in to proportionally larger work; count overflow is
    /// still refused.
    #[must_use]
    pub fn character_limit(mut self, limit: usize) -> Self {
        self.character_limit = limit;
        self
    }

    /// `include_sign=` — always show a sign (`+` for non-negative).
    #[must_use]
    pub fn include_sign(mut self, on: bool) -> Self {
        self.include_sign = on;
        self
    }

    /// `group_with_commas=` — group the integer digits by three.
    #[must_use]
    pub fn group_with_commas(mut self, on: bool) -> Self {
        self.group_with_commas = on;
        self
    }

    /// `digit_buff_per_font_unit=` — inter-digit buff as a fraction of
    /// the font size.
    #[must_use]
    pub fn digit_buff_per_font_unit(mut self, buff: f64) -> Self {
        self.digit_buff_per_font_unit = buff;
        self
    }

    /// `show_ellipsis=` — trail one U+2026 glyph child.
    #[must_use]
    pub fn show_ellipsis(mut self, on: bool) -> Self {
        self.show_ellipsis = on;
        self
    }

    /// `unit=` — one trailing child holding the unit's glyphs; a leading
    /// `^` is stripped and aligns the unit `UP` (the Reference's
    /// superscript convention), anything else aligns `DOWN` with the
    /// digits.
    #[must_use]
    pub fn unit(mut self, unit: &str) -> Self {
        self.unit = Some(unit.to_owned());
        self
    }

    /// `include_background_rectangle=` — a BLACK, fully opaque rectangle
    /// behind the digits, at child 0.
    #[must_use]
    pub fn include_background_rectangle(mut self, on: bool) -> Self {
        self.include_background_rectangle = on;
        self
    }

    /// `edge_to_fix=` — the edge `set_value` holds in place.
    #[must_use]
    pub fn edge_to_fix(mut self, edge: Vec3) -> Self {
        self.edge_to_fix = edge;
        self
    }

    /// `font_size=`.
    #[must_use]
    pub fn font_size(mut self, font_size: f64) -> Self {
        self.font_size = font_size;
        self.invalidate_cache();
        self
    }

    /// The config's `text.font_size_for_unit_height` — the font size at
    /// which "0" stands one manim unit tall.
    #[must_use]
    pub fn font_size_for_unit_height(mut self, fsuh: f64) -> Self {
        self.font_size_for_unit_height = fsuh;
        self.invalidate_cache();
        self
    }

    /// `color=` — stroke and fill together.
    #[must_use]
    pub fn color(mut self, color: Srgb) -> Self {
        self.style = self.style.color(color);
        self.invalidate_cache();
        self
    }

    /// `stroke_width=` (0 by default).
    #[must_use]
    pub fn stroke_width(mut self, width: f64) -> Self {
        self.style = self.style.stroke_width(width);
        self.invalidate_cache();
        self
    }

    /// `fill_opacity=` (1 by default).
    #[must_use]
    pub fn fill_opacity(mut self, opacity: f64) -> Self {
        self.style = self.style.fill_opacity(opacity);
        self.invalidate_cache();
        self
    }

    /// `fill_border_width=` (0.5 by default).
    #[must_use]
    pub fn fill_border_width(mut self, width: f64) -> Self {
        self.style = self.style.fill_border_width(width);
        self.invalidate_cache();
        self
    }

    /// Replace the digit style wholesale.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self.invalidate_cache();
        self
    }

    /// Typeset and lay out, centered at the origin — the Reference's
    /// constructor path (`arrange` with `center=True`).
    ///
    /// # Errors
    ///
    /// [`TextMobjectError`]: an unmapped character (a unit glyph the
    /// book lacks) or a font-policy failure surfaces at typeset time.
    pub fn build(mut self, book: &FontBook) -> Result<Self, TextMobjectError> {
        self.refresh(book)?;
        Ok(self)
    }

    /// `set_value`: rebuild the displayed string, reusing cached glyphs,
    /// and restore the `edge_to_fix` bounding-box point to where it was.
    ///
    /// # Errors
    ///
    /// [`TextMobjectError`]: a newly displayed character the book cannot
    /// typeset, or formatting/layout exceeds the declared resource limits.
    /// A refusal keeps the previous value, displayed string, and geometry.
    /// Glyphs successfully cached before a later refusal remain reusable.
    pub fn set_value(&mut self, number: f64, book: &FontBook) -> Result<(), TextMobjectError> {
        let move_to = self.vmob.bbox_point(self.edge_to_fix);
        self.refresh_value(number, book)?;
        if let Some(point) = move_to {
            self.vmob = core::mem::take(&mut self.vmob).moved_to_aligned(point, self.edge_to_fix);
        }
        Ok(())
    }

    /// `increment_value`: `set_value(value + delta)`.
    ///
    /// # Errors
    ///
    /// As [`DecimalNumber::set_value`].
    pub fn increment_value(&mut self, delta: f64, book: &FontBook) -> Result<(), TextMobjectError> {
        self.set_value(self.number + delta, book)
    }

    /// `get_value` — the current number.
    #[must_use]
    pub fn value(&self) -> f64 {
        self.number
    }

    /// The displayed string (en-dash substituted; ellipsis and unit are
    /// trailing children, not part of it — the Reference's `num_string`).
    #[must_use]
    pub fn num_string(&self) -> &str {
        &self.num_string
    }

    /// The built family.
    #[must_use]
    pub fn vmob(&self) -> &VMobject {
        &self.vmob
    }

    /// Consume into the family (for `Stage::add`).
    #[must_use]
    pub fn into_vmob(self) -> VMobject {
        self.vmob
    }

    /// `get_font_size`.
    #[must_use]
    pub fn get_font_size(&self) -> f64 {
        self.font_size
    }

    /// The glyph-recycling witness: how many characters have been
    /// typeset since construction. An update inside the cached alphabet
    /// leaves this unchanged; each new character adds exactly one.
    #[must_use]
    pub fn typesets(&self) -> usize {
        self.typesets
    }

    /// Style/font geometry changed: cached children no longer match the
    /// configuration. The counter is cumulative work and is not rewound.
    fn invalidate_cache(&mut self) {
        self.cache.clear();
    }

    /// Regenerate at the canonical layout origin, committing the logical
    /// value only with its displayed string and geometry. Staged animations
    /// anchor after record conversion in world space, avoiding a cumulative
    /// f64-to-f32 translation roundtrip in the cached glyph layout.
    fn refresh_value(&mut self, number: f64, book: &FontBook) -> Result<(), TextMobjectError> {
        let previous = core::mem::replace(&mut self.number, number);
        if let Err(error) = self.refresh(book) {
            self.number = previous;
            return Err(error);
        }
        Ok(())
    }

    /// The cached child for `c`, typesetting it (and counting the
    /// typeset) only on first sight.
    fn collect_child(&mut self, c: char, book: &FontBook) -> Result<VMobject, TextMobjectError> {
        if !self.cache.contains_key(&c) {
            let mut buf = [0u8; 4];
            let built = Text::new(c.encode_utf8(&mut buf))
                .font_size(self.font_size)
                .font_size_for_unit_height(self.font_size_for_unit_height)
                .style(self.style)
                .build(book)?;
            // The single glyph child, standing at its layout position.
            let child = built.vmob.children().first().cloned().unwrap_or_default();
            self.typesets += 1;
            self.cache.insert(c, child);
        }
        Ok(self.cache.get(&c).cloned().unwrap_or_default())
    }

    /// The Reference's `set_submobjects_from_number`: format, gather the
    /// children from the cache, arrange `RIGHT` aligned `DOWN` and center,
    /// then re-seat the special characters.
    fn refresh(&mut self, book: &FontBook) -> Result<(), TextMobjectError> {
        let num_string = self.format_number()?;
        let numeric_characters = bounded_char_count(
            &num_string,
            self.character_limit,
            "decimal-number numeric characters",
        )?;
        let unit_up = self
            .unit
            .as_deref()
            .is_some_and(|unit| unit.starts_with('^'));
        let unit_source = self
            .unit
            .as_deref()
            .map(|unit| unit.strip_prefix('^').unwrap_or(unit));
        let unit_characters = match unit_source {
            Some(unit) => {
                bounded_char_count(unit, self.character_limit, "decimal-number unit characters")?
            }
            None => 0,
        };
        let total_characters = checked_add(
            "decimal-number character count",
            checked_add(
                "decimal-number character count",
                numeric_characters,
                usize::from(self.show_ellipsis),
            )?,
            unit_characters,
        )?;
        ensure_resource_limit(
            "decimal-number characters",
            total_characters,
            self.character_limit,
        )?;

        // Own the bounded unit text before mutating the glyph cache.
        let unit_text = match unit_source {
            Some(unit) => {
                let mut owned = try_string_with_capacity("decimal-number unit text", unit.len())?;
                owned.push_str(unit);
                Some(owned)
            }
            None => None,
        };

        let digit_buff = self.digit_buff_per_font_unit * self.font_size;
        let mut chars =
            try_vec_with_capacity("decimal-number numeric characters", numeric_characters)?;
        chars.extend(num_string.chars());
        let child_capacity = checked_add(
            "decimal-number child count",
            checked_add(
                "decimal-number child count",
                chars.len(),
                usize::from(self.show_ellipsis),
            )?,
            usize::from(self.unit.is_some()),
        )?;
        let mut children = try_vec_with_capacity("decimal-number children", child_capacity)?;
        for &c in &chars {
            children.push(self.collect_child(c, book)?);
        }
        if self.show_ellipsis {
            children.push(self.collect_child(ELLIPSIS, book)?);
        }
        if let Some(unit) = unit_text {
            let mut glyphs = try_vec_with_capacity("decimal-number unit glyphs", unit_characters)?;
            for c in unit.chars() {
                glyphs.push(self.collect_child(c, book)?);
            }
            children.push(VMobject::arranged(glyphs, RIGHT, digit_buff, DOWN));
        }

        // arrange(RIGHT, buff, aligned_edge=DOWN) with center=True.
        let mut placed = try_vec_with_capacity("decimal-number placement", children.len())?;
        for child in children {
            match placed.last() {
                None => placed.push(child),
                Some(previous) => placed.push(child.next_to(previous, RIGHT, digit_buff, DOWN)),
            }
        }
        if let Some((min, max)) = union_extent(&placed) {
            let shift = [
                -0.5 * (min[0] + max[0]),
                -0.5 * (min[1] + max[1]),
                -0.5 * (min[2] + max[2]),
            ];
            for child in &mut placed {
                *child = core::mem::take(child).shifted(shift);
            }
        }

        // Special-character alignment, the Reference's loop verbatim: the
        // en dash rides at mid-height of the following glyph; the comma
        // drops by half its own height.
        for (i, &c) in chars.iter().enumerate() {
            if c == EN_DASH && i + 1 < chars.len() {
                // align_to(next, UP), then down half the next glyph's
                // height — computed without cloning the neighbour.
                let (Some(next_top), Some(dash_top)) =
                    (placed[i + 1].bbox_point(UP), placed[i].bbox_point(UP))
                else {
                    continue;
                };
                let drop = 0.5 * placed[i + 1].length_over_dim(1);
                let dy = next_top[1] - dash_top[1] - drop;
                placed[i] = core::mem::take(&mut placed[i]).shifted([0.0, dy, 0.0]);
            } else if c == ',' {
                let drop = 0.5 * placed[i].length_over_dim(1);
                placed[i] = core::mem::take(&mut placed[i]).shifted([0.0, -drop, 0.0]);
            }
        }
        if unit_up {
            // self[-1].align_to(self, UP): the unit's top meets the
            // family's top (a no-op when the unit is already the tallest).
            if let Some((_, union_max)) = union_extent(&placed)
                && let Some(last) = placed.last_mut()
                && let Some((_, last_max)) = last.extent()
            {
                let dy = union_max[1] - last_max[1];
                *last = core::mem::take(last).shifted([0.0, dy, 0.0]);
            }
        }

        let family_capacity = checked_add(
            "decimal-number family count",
            placed.len(),
            usize::from(self.include_background_rectangle),
        )?;
        let mut family = try_vec_with_capacity("decimal-number family", family_capacity)?;
        if self.include_background_rectangle {
            // add_background_rectangle(): camera background (BLACK), full
            // opacity, buff 0, added to the back — child 0.
            let probe = VMobject::new().with_children(placed.clone());
            family.push(matchers::background_rectangle(&probe, BLACK, 1.0));
        }
        family.extend(placed);
        self.vmob = VMobject::new().with_style(self.style).with_children(family);
        self.num_string = num_string;
        Ok(())
    }

    /// The Reference's `get_num_string` for a real number: CPython
    /// `str.format` semantics for sign, zero-padding (re-grouping
    /// included), comma grouping, and precision, then negative-zero
    /// suppression against round-half-even, then the en-dash swap.
    fn format_number(&self) -> Result<String, TextMobjectError> {
        let ndp = self.num_decimal_places;
        let number = self.number;
        ensure_resource_limit("decimal-number precision", ndp, self.character_limit)?;
        ensure_resource_limit(
            "decimal-number minimum width",
            self.min_total_width,
            self.character_limit,
        )?;
        let frac_len = if ndp > 0 {
            checked_add("decimal-number fractional width", 1, ndp)?
        } else {
            0
        };
        let minimum_numeric_width =
            checked_add("decimal-number minimum numeric width", 1, frac_len)?;
        ensure_resource_limit(
            "decimal-number numeric characters",
            minimum_numeric_width,
            self.character_limit,
        )?;

        // int(number) truncation when ndp == 0; round-half-even fixed
        // precision otherwise (Rust's float formatting rounds exactly the
        // way CPython's does).
        let (neg, int_digits, frac_digits, rounded_is_zero) = if ndp == 0 {
            let t = number.trunc();
            // int(-0.4) == 0: no negative zero survives truncation.
            let t = if t == 0.0 { 0.0 } else { t };
            let s = format_float(t, Some(0), "decimal-number integer formatting")?;
            let body = s.trim_start_matches('-');
            bounded_char_count(body, self.character_limit, "decimal-number formatted body")?;
            (
                t < 0.0,
                try_copy_string("decimal-number integer digits", body)?,
                String::new(),
                t == 0.0,
            )
        } else {
            let s = format_float(
                number,
                Some(ndp),
                "decimal-number fixed-precision formatting",
            )?;
            let neg = s.starts_with('-');
            let body = s.trim_start_matches('-');
            bounded_char_count(body, self.character_limit, "decimal-number formatted body")?;
            let (int_part, frac_part) = body.split_once('.').unwrap_or((body, ""));
            (
                neg,
                try_copy_string("decimal-number integer digits", int_part)?,
                try_copy_string("decimal-number fractional digits", frac_part)?,
                body.bytes().all(|byte| byte == b'0' || byte == b'.'),
            )
        };

        // 0N zero-padding: pad the digit run until sign + grouped digits
        // + fraction reach the width. Determine the bounded final length
        // first, then prepend once; comma-boundary overshoot remains
        // CPython's exactly without repeated front insertion.
        let sign_len = usize::from(neg || self.include_sign);
        let mut digits = int_digits;
        let fixed_width = checked_add("decimal-number fixed width", sign_len, frac_len)?;
        let padded_digits = padded_digit_count(
            digits.len(),
            fixed_width,
            self.min_total_width,
            self.group_with_commas,
        );
        if padded_digits > digits.len() {
            let zeroes = padded_digits - digits.len();
            ensure_resource_limit(
                "decimal-number padded digits",
                padded_digits,
                self.character_limit,
            )?;
            let mut padded =
                try_string_with_capacity("decimal-number padded digits", padded_digits)?;
            for _ in 0..zeroes {
                padded.push('0');
            }
            padded.push_str(&digits);
            digits = padded;
        }
        let grouped_characters = grouped_len(digits.len(), self.group_with_commas)?;
        let emitted_sign_len = usize::from(if neg {
            !rounded_is_zero || self.include_sign
        } else {
            self.include_sign
        });
        let output_characters = checked_add(
            "decimal-number rendered width",
            checked_add(
                "decimal-number rendered width",
                emitted_sign_len,
                grouped_characters,
            )?,
            frac_len,
        )?;
        ensure_resource_limit(
            "decimal-number numeric characters",
            output_characters,
            self.character_limit,
        )?;
        let grouped = if self.group_with_commas {
            group_digits(&digits)?
        } else {
            digits
        };
        debug_assert_eq!(grouped.len(), grouped_characters);
        let output_bytes = checked_add(
            "decimal-number output bytes",
            output_characters,
            if neg && !rounded_is_zero {
                EN_DASH.len_utf8() - 1
            } else {
                0
            },
        )?;
        let mut out = try_string_with_capacity("decimal-number output", output_bytes)?;
        if neg {
            if rounded_is_zero {
                if self.include_sign {
                    out.push('+');
                }
            } else {
                out.push(EN_DASH);
            }
        } else if self.include_sign {
            out.push('+');
        }
        out.push_str(&grouped);
        if ndp > 0 {
            out.push('.');
            out.push_str(&frac_digits);
        }
        Ok(out)
    }
}

impl From<DecimalNumber> for fmn_mobject::Mobject {
    fn from(d: DecimalNumber) -> Self {
        d.vmob.into()
    }
}

/// `Integer` (the same module): a `DecimalNumber` with
/// `num_decimal_places=0` whose `get_value` rounds (half-even) to an
/// integer. The display still truncates — the Reference's
/// `int(number)` in the formatter — so `Integer(2.7)` shows "2" while
/// `value()` is 3.
#[derive(Debug, Clone)]
pub struct Integer {
    inner: DecimalNumber,
}

impl Integer {
    /// `Integer(number)`: the `DecimalNumber` defaults with
    /// `num_decimal_places=0`.
    #[must_use]
    pub fn new(number: f64) -> Self {
        Self {
            inner: DecimalNumber::new(number).num_decimal_places(0),
        }
    }

    /// `num_decimal_places=` (Reference parity via `**kwargs`).
    #[must_use]
    pub fn num_decimal_places(mut self, ndp: usize) -> Self {
        self.inner = self.inner.num_decimal_places(ndp);
        self
    }

    /// See [`DecimalNumber::min_total_width`].
    #[must_use]
    pub fn min_total_width(mut self, width: usize) -> Self {
        self.inner = self.inner.min_total_width(width);
        self
    }

    /// See [`DecimalNumber::character_limit`].
    #[must_use]
    pub fn character_limit(mut self, limit: usize) -> Self {
        self.inner = self.inner.character_limit(limit);
        self
    }

    /// See [`DecimalNumber::include_sign`].
    #[must_use]
    pub fn include_sign(mut self, on: bool) -> Self {
        self.inner = self.inner.include_sign(on);
        self
    }

    /// See [`DecimalNumber::group_with_commas`].
    #[must_use]
    pub fn group_with_commas(mut self, on: bool) -> Self {
        self.inner = self.inner.group_with_commas(on);
        self
    }

    /// See [`DecimalNumber::digit_buff_per_font_unit`].
    #[must_use]
    pub fn digit_buff_per_font_unit(mut self, buff: f64) -> Self {
        self.inner = self.inner.digit_buff_per_font_unit(buff);
        self
    }

    /// See [`DecimalNumber::show_ellipsis`].
    #[must_use]
    pub fn show_ellipsis(mut self, on: bool) -> Self {
        self.inner = self.inner.show_ellipsis(on);
        self
    }

    /// See [`DecimalNumber::unit`].
    #[must_use]
    pub fn unit(mut self, unit: &str) -> Self {
        self.inner = self.inner.unit(unit);
        self
    }

    /// See [`DecimalNumber::include_background_rectangle`].
    #[must_use]
    pub fn include_background_rectangle(mut self, on: bool) -> Self {
        self.inner = self.inner.include_background_rectangle(on);
        self
    }

    /// See [`DecimalNumber::edge_to_fix`].
    #[must_use]
    pub fn edge_to_fix(mut self, edge: Vec3) -> Self {
        self.inner = self.inner.edge_to_fix(edge);
        self
    }

    /// See [`DecimalNumber::font_size`].
    #[must_use]
    pub fn font_size(mut self, font_size: f64) -> Self {
        self.inner = self.inner.font_size(font_size);
        self
    }

    /// See [`DecimalNumber::color`].
    #[must_use]
    pub fn color(mut self, color: Srgb) -> Self {
        self.inner = self.inner.color(color);
        self
    }

    /// See [`DecimalNumber::stroke_width`].
    #[must_use]
    pub fn stroke_width(mut self, width: f64) -> Self {
        self.inner = self.inner.stroke_width(width);
        self
    }

    /// See [`DecimalNumber::fill_opacity`].
    #[must_use]
    pub fn fill_opacity(mut self, opacity: f64) -> Self {
        self.inner = self.inner.fill_opacity(opacity);
        self
    }

    /// See [`DecimalNumber::fill_border_width`].
    #[must_use]
    pub fn fill_border_width(mut self, width: f64) -> Self {
        self.inner = self.inner.fill_border_width(width);
        self
    }

    /// See [`DecimalNumber::style`].
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.inner = self.inner.style(style);
        self
    }

    /// Typeset and lay out; see [`DecimalNumber::build`].
    ///
    /// # Errors
    ///
    /// As [`DecimalNumber::build`].
    pub fn build(self, book: &FontBook) -> Result<Self, TextMobjectError> {
        Ok(Self {
            inner: self.inner.build(book)?,
        })
    }

    /// See [`DecimalNumber::set_value`].
    ///
    /// # Errors
    ///
    /// As [`DecimalNumber::set_value`].
    pub fn set_value(&mut self, number: f64, book: &FontBook) -> Result<(), TextMobjectError> {
        self.inner.set_value(number, book)
    }

    /// See [`DecimalNumber::increment_value`].
    ///
    /// # Errors
    ///
    /// As [`DecimalNumber::set_value`].
    pub fn increment_value(&mut self, delta: f64, book: &FontBook) -> Result<(), TextMobjectError> {
        self.inner.increment_value(delta, book)
    }

    /// `get_value`: the current value rounded half-even to an integer.
    #[must_use]
    pub fn value(&self) -> i64 {
        self.inner.value().round_ties_even() as i64
    }

    /// See [`DecimalNumber::num_string`].
    #[must_use]
    pub fn num_string(&self) -> &str {
        self.inner.num_string()
    }

    /// See [`DecimalNumber::vmob`].
    #[must_use]
    pub fn vmob(&self) -> &VMobject {
        self.inner.vmob()
    }

    /// Consume into the family (for `Stage::add`).
    #[must_use]
    pub fn into_vmob(self) -> VMobject {
        self.inner.into_vmob()
    }

    /// See [`DecimalNumber::get_font_size`].
    #[must_use]
    pub fn get_font_size(&self) -> f64 {
        self.inner.get_font_size()
    }

    /// See [`DecimalNumber::typesets`].
    #[must_use]
    pub fn typesets(&self) -> usize {
        self.inner.typesets()
    }
}

impl From<Integer> for fmn_mobject::Mobject {
    fn from(i: Integer) -> Self {
        i.inner.into()
    }
}

/// The union bounding box over a row of placed children.
fn union_extent(children: &[VMobject]) -> Option<(Vec3, Vec3)> {
    let mut min = [f64::INFINITY; 3];
    let mut max = [f64::NEG_INFINITY; 3];
    let mut any = false;
    for child in children {
        if let Some((lo, hi)) = child.extent() {
            any = true;
            for k in 0..3 {
                min[k] = min[k].min(lo[k]);
                max[k] = max[k].max(hi[k]);
            }
        }
    }
    any.then_some((min, max))
}

fn ensure_resource_limit(
    context: &'static str,
    requested: usize,
    limit: usize,
) -> Result<(), TextMobjectError> {
    if requested > limit {
        Err(TextMobjectError::ResourceLimit {
            context,
            requested,
            limit,
        })
    } else {
        Ok(())
    }
}

fn checked_add(context: &'static str, lhs: usize, rhs: usize) -> Result<usize, TextMobjectError> {
    lhs.checked_add(rhs)
        .ok_or(TextMobjectError::CapacityOverflow { context })
}

fn try_vec_with_capacity<T>(
    context: &'static str,
    capacity: usize,
) -> Result<Vec<T>, TextMobjectError> {
    let element_size = core::mem::size_of::<T>();
    if element_size != 0 {
        let bytes = capacity
            .checked_mul(element_size)
            .ok_or(TextMobjectError::CapacityOverflow { context })?;
        if bytes > isize::MAX as usize {
            return Err(TextMobjectError::CapacityOverflow { context });
        }
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| TextMobjectError::AllocationFailed {
            context,
            requested: capacity,
        })?;
    Ok(values)
}

fn try_string_with_capacity(
    context: &'static str,
    capacity: usize,
) -> Result<String, TextMobjectError> {
    if capacity > isize::MAX as usize {
        return Err(TextMobjectError::CapacityOverflow { context });
    }
    let mut value = String::new();
    value
        .try_reserve_exact(capacity)
        .map_err(|_| TextMobjectError::AllocationFailed {
            context,
            requested: capacity,
        })?;
    Ok(value)
}

fn try_copy_string(context: &'static str, source: &str) -> Result<String, TextMobjectError> {
    let mut value = try_string_with_capacity(context, source.len())?;
    value.push_str(source);
    Ok(value)
}

fn bounded_char_count(
    value: &str,
    limit: usize,
    context: &'static str,
) -> Result<usize, TextMobjectError> {
    let mut count = 0usize;
    for _ in value.chars() {
        count = checked_add(context, count, 1)?;
        ensure_resource_limit(context, count, limit)?;
    }
    Ok(count)
}

#[derive(Default)]
struct CountingWriter {
    bytes: usize,
}

impl core::fmt::Write for CountingWriter {
    fn write_str(&mut self, value: &str) -> core::fmt::Result {
        self.bytes = self
            .bytes
            .checked_add(value.len())
            .ok_or(core::fmt::Error)?;
        Ok(())
    }
}

/// Format once into a counting sink, reserve the exact byte count
/// fallibly, then render into that reservation. The caller bounds
/// `precision` before this helper runs.
fn format_float(
    number: f64,
    precision: Option<usize>,
    context: &'static str,
) -> Result<String, TextMobjectError> {
    let mut count = CountingWriter::default();
    let counted = match precision {
        Some(precision) => write!(&mut count, "{number:.precision$}"),
        None => write!(&mut count, "{number}"),
    };
    counted.map_err(|_| TextMobjectError::CapacityOverflow { context })?;

    let mut output = try_string_with_capacity(context, count.bytes)?;
    let rendered = match precision {
        Some(precision) => write!(&mut output, "{number:.precision$}"),
        None => write!(&mut output, "{number}"),
    };
    rendered.map_err(|_| TextMobjectError::CapacityOverflow { context })?;
    debug_assert_eq!(output.len(), count.bytes);
    Ok(output)
}

/// The rendered length of an `n`-digit run, commas included.
fn grouped_len(n: usize, with_commas: bool) -> Result<usize, TextMobjectError> {
    if n == 0 {
        return Ok(0);
    }
    checked_add(
        "decimal-number grouped width",
        n,
        if with_commas { (n - 1) / 3 } else { 0 },
    )
}

fn grouped_len_saturating(n: usize, with_commas: bool) -> usize {
    n.saturating_add(if with_commas {
        n.saturating_sub(1) / 3
    } else {
        0
    })
}

/// Find the smallest digit run whose sign, fraction, and optional comma
/// grouping reach `minimum_width`. Saturating comparison preserves the
/// monotone search at the host boundary; exact checked arithmetic still
/// gates the resulting allocation.
fn padded_digit_count(
    current_digits: usize,
    fixed_width: usize,
    minimum_width: usize,
    with_commas: bool,
) -> usize {
    let rendered_width =
        |digits: usize| fixed_width.saturating_add(grouped_len_saturating(digits, with_commas));
    if rendered_width(current_digits) >= minimum_width {
        return current_digits;
    }

    let mut low = current_digits;
    let mut high = minimum_width.max(current_digits);
    while low < high {
        let midpoint = low + (high - low) / 2;
        if rendered_width(midpoint) >= minimum_width {
            high = midpoint;
        } else {
            low = midpoint + 1;
        }
    }
    low
}

/// Insert a comma every three digits from the right.
fn group_digits(digits: &str) -> Result<String, TextMobjectError> {
    let n = digits.len();
    let capacity = grouped_len(n, true)?;
    let mut out = try_string_with_capacity("decimal-number grouped digits", capacity)?;
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (n - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    Ok(out)
}

// -------------------------------------------------- value animations
// (fm-jh7: ChangingDecimal / ChangeDecimalToValue / CountInFrom — the
// typed wrappers live here, beside DecimalNumber, because Choreo cannot
// depend upward on this crate)

use fmn_anim::{AnimError, UpdateFromFunc};
use fmn_mobject::uniforms::Uniforms;
use fmn_mobject::{Mob, Mobject, RecordBuffer, RecordSchema, Stage};

const NUMBER_STYLE_FIELDS: [&str; 4] = [
    "fill_rgba",
    "fill_border_width",
    "stroke_rgba",
    "stroke_width",
];

/// Capture paint before retiring a rebuilt family. Keeping the record
/// columns, rather than only their first row, preserves gradient colors and
/// stroke profiles when the new glyph has a different point count.
struct NumberPaint {
    buffer: RecordBuffer,
    z_index: i32,
}

impl NumberPaint {
    fn capture(stage: &Stage, mob: Mob) -> Result<Self, AnimError> {
        let entry = stage.get(mob).ok_or(AnimError::StaleHandle(mob))?;
        // Do not carry geometry into the paint sampler. Constant style
        // records can then take RecordBuffer's exact repeat path instead of
        // accumulating interpolation roundoff as glyph shapes change.
        let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), entry.buffer.len())
            .map_err(number_update_error)?;
        for field in NUMBER_STYLE_FIELDS {
            if buffer.is_empty() {
                if let Some(value) = entry.buffer.default_record(field) {
                    buffer.write_default_record(field, &value);
                }
            } else if let Some(values) = entry.buffer.read_column(field) {
                buffer.write_range(field, 0, &values);
            }
        }
        Ok(Self {
            buffer,
            z_index: stage.z_index(mob),
        })
    }

    fn apply(&self, target: &mut Mobject, uniforms: Uniforms) -> Result<(), AnimError> {
        if target.buffer.is_empty() {
            for field in NUMBER_STYLE_FIELDS {
                if let Some(value) = self
                    .buffer
                    .read(0, field)
                    .or_else(|| self.buffer.default_record(field))
                {
                    target.buffer.write_default_record(field, &value);
                }
            }
        } else {
            let mut source = self.buffer.snapshot_clone();
            source
                .resize_with_interpolation(target.buffer.len())
                .map_err(number_update_error)?;
            for field in NUMBER_STYLE_FIELDS {
                if let Some(values) = source.read_column(field) {
                    target.buffer.write_range(field, 0, &values);
                }
            }
        }
        // The buffer's defaults above now carry the live style; an older
        // detached constructor row must not overwrite them during Stage::add.
        target.defaults_row = None;
        target.uniforms = uniforms;
        target.z_index = self.z_index;
        Ok(())
    }

    fn apply_family(&self, target: &mut Mobject, uniforms: Uniforms) -> Result<(), AnimError> {
        self.apply(target, uniforms)?;
        for child in &mut target.submobjects {
            self.apply_family(child, uniforms)?;
        }
        Ok(())
    }
}

fn number_update_error(error: impl core::fmt::Display) -> AnimError {
    AnimError::UpdateFailed {
        operation: "DecimalNumber::set_value",
        detail: error.to_string(),
    }
}

impl DecimalNumber {
    /// Prepare a native numeric slot using an existing glyph as its paint
    /// donor. Keep the background's own paint while propagating placement-
    /// independent render settings to every member. The caller can validate
    /// the complete detached result before editing an adopted formula.
    pub(crate) fn with_live_paint(
        &self,
        stage: &Stage,
        donor: Mob,
    ) -> Result<Mobject, AnimError> {
        let entry = stage.get(donor).ok_or(AnimError::StaleHandle(donor))?;
        let uniforms = *entry.uniforms();
        let paint = NumberPaint::capture(stage, donor)?;
        let mut prepared: Mobject = self.vmob.clone().into();
        paint.apply(&mut prepared, uniforms)?;
        for (index, child) in prepared.submobjects.iter_mut().enumerate() {
            if self.include_background_rectangle && index == 0 {
                // The background is independent of the digits' live color.
                child.uniforms = uniforms;
                child.z_index = prepared.z_index;
            } else {
                paint.apply_family(child, uniforms)?;
            }
        }
        Ok(prepared)
    }
}

/// Add the current number's glyph family and return the stable root handle
/// a value animation drives. Background, digit, ellipsis, and unit geometry
/// retain their separate styles. The initial displayed value is validated
/// against `book`; no speculative value is formatted or typeset.
///
/// # Errors
/// [`TextMobjectError`] if the current number cannot be built against `book`
/// within its declared formatting/layout limits. No stage entry is added on
/// failure.
pub fn add_decimal_for_value_animation(
    stage: &mut Stage,
    decimal: &DecimalNumber,
    book: &FontBook,
) -> Result<Mob, TextMobjectError> {
    let mut prepared = decimal.clone();
    prepared.set_value(prepared.value(), book)?;
    Ok(stage.add(prepared.into_vmob()))
}

/// `ChangingDecimal` (numbers.py:14): drive the displayed value from a
/// function of the time-spanned alpha, rebuilding through
/// [`DecimalNumber::set_value`] and installing the result over `mob` each
/// frame (`suspend_mobject_updating` defaults false exactly as the
/// Reference passes). `mob` is the handle [`add_decimal_for_value_animation`]
/// returned; `decimal` is the same typed value it was built from.
///
/// Each sample retains the current whole-family affine placement and holds
/// the staged `edge_to_fix` bounding-box point in world space. The first
/// numeric glyph supplies the new glyphs' live paint columns (as in the
/// Reference); the background keeps its own paint. The root's uniforms,
/// including fixed-in-frame/depth settings, propagate to the rebuilt family,
/// and the root's draw order survives. Individual glyph geometry edits are
/// replaced by the numeric layout rather than carried to unrelated digits.
///
/// Only the current value is validated at construction. Per-sample formatting
/// and typesetting are fallible: the first refusal is retained through
/// [`fmn_anim::Animation::deferred_error`], with the previous visible family
/// intact. Later samples do not call `number_update_func` until a new begin.
///
/// # Errors
/// [`TextMobjectError`] if the current value cannot be built. A later layout
/// refusal surfaces as [`AnimError::UpdateFailed`]; a stale `mob` as
/// [`AnimError::StaleHandle`], through the animation's frame boundary.
pub fn changing_decimal(
    mob: Mob,
    mut decimal: DecimalNumber,
    book: FontBook,
    number_update_func: impl FnMut(f64) -> f64 + 'static,
) -> Result<UpdateFromFunc, TextMobjectError> {
    decimal.set_value(decimal.value(), &book)?;
    let mut update = number_update_func;
    Ok(UpdateFromFunc::try_new_alpha(
        mob,
        move |stage, mob, alpha| {
            let entry = stage.get(mob).ok_or(AnimError::StaleHandle(mob))?;
            let uniforms = *entry.uniforms();
            let anchor = stage.get_bounding_box_point(mob, decimal.edge_to_fix);
            let root_paint = NumberPaint::capture(stage, mob)?;
            let background = decimal
                .include_background_rectangle
                .then(|| entry.submobjects().first().copied())
                .flatten();
            let background_paint = background
                .map(|background| NumberPaint::capture(stage, background))
                .transpose()?;
            let digit = entry
                .submobjects()
                .iter()
                .copied()
                .find(|child| Some(*child) != background)
                .unwrap_or(mob);
            let digit_paint = NumberPaint::capture(stage, digit)?;
            // The family's affine lives on its members with records: a
            // point-free root returns to identity on every box refresh,
            // which a general `apply_affine` performs.
            let placement = stage
                .family(digit)
                .into_iter()
                .filter_map(|member| stage.get(member))
                .find(|member| !member.buffer.is_empty())
                .map_or_else(|| entry.placement(), |member| member.placement());

            // All fallible layout and paint preparation precedes publication.
            // Stage::install accepts an owned, acyclic family over this already
            // checked handle; every placement target below is then newly live.
            decimal
                .refresh_value(update(alpha), &book)
                .map_err(number_update_error)?;
            let mut prepared: Mobject = decimal.vmob().clone().into();
            root_paint.apply(&mut prepared, uniforms)?;
            for (index, child) in prepared.submobjects.iter_mut().enumerate() {
                let paint = if decimal.include_background_rectangle && index == 0 {
                    background_paint.as_ref()
                } else {
                    Some(&digit_paint)
                };
                if let Some(paint) = paint {
                    paint.apply_family(child, uniforms)?;
                }
            }
            stage.install(mob, prepared).map_err(AnimError::Stage)?;
            for member in stage.family(mob) {
                stage
                    .set_placement(member, placement)
                    .map_err(AnimError::Stage)?;
            }
            stage.move_to(mob, anchor, decimal.edge_to_fix);
            Ok(())
        },
    ))
}

/// `ChangeDecimalToValue` (numbers.py:38): interpolate from the decimal's
/// current value to `target_number`.
///
/// # Errors
/// As [`changing_decimal`].
pub fn change_decimal_to_value(
    mob: Mob,
    decimal: DecimalNumber,
    book: FontBook,
    target_number: f64,
) -> Result<UpdateFromFunc, TextMobjectError> {
    let start_number = decimal.value();
    changing_decimal(mob, decimal, book, move |alpha| {
        (1.0 - alpha) * start_number + alpha * target_number
    })
}

/// `CountInFrom` (numbers.py:53): count up from `source_number` to the
/// decimal's current value.
///
/// # Errors
/// As [`changing_decimal`].
pub fn count_in_from(
    mob: Mob,
    decimal: DecimalNumber,
    book: FontBook,
    source_number: f64,
) -> Result<UpdateFromFunc, TextMobjectError> {
    let target_number = decimal.value();
    changing_decimal(mob, decimal, book, move |alpha| {
        let alpha = alpha.clamp(0.0, 1.0);
        (1.0 - alpha) * source_number + alpha * target_number
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_anim::animation::Animation as _;
    use fmn_core::constants::{DEFAULT_MOBJECT_COLOR, ORIGIN};
    use fmn_mobject::Stage;

    const EPS: f64 = 1e-9;

    fn book() -> FontBook {
        FontBook::bundled().expect("bundled faces parse")
    }

    fn extent(v: &VMobject) -> (Vec3, Vec3) {
        v.extent().expect("has extent")
    }

    fn point_count(stage: &Stage, mob: Mob) -> usize {
        stage
            .family(mob)
            .into_iter()
            .map(|member| stage.get(member).unwrap().buffer.len())
            .sum()
    }

    fn first_digit(stage: &Stage, mob: Mob, background: bool) -> Mob {
        stage.get(mob).unwrap().submobjects()[usize::from(background)]
    }

    fn assert_point_close(actual: Vec3, expected: Vec3) {
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
    }

    fn assert_number_geometry(stage: &Stage, actual: Mob, expected: Mob) {
        let actual_family = stage.family(actual);
        let expected_family = stage.family(expected);
        assert_eq!(actual_family.len(), expected_family.len());
        for (actual, expected) in actual_family.into_iter().zip(expected_family) {
            let actual = stage.get_points(actual).unwrap();
            let expected = stage.get_points(expected).unwrap();
            assert_eq!(actual.len(), expected.len());
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert_point_close(actual, expected);
            }
        }
    }

    #[test]
    fn negative_zero_is_suppressed() {
        let book = book();
        let d = DecimalNumber::new(-0.001).build(&book).expect("builds");
        assert_eq!(d.num_string(), "0.00");
        let d = DecimalNumber::new(-0.001)
            .include_sign(true)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "+0.00");
        // A genuine negative keeps its sign.
        let d = DecimalNumber::new(-0.5).build(&book).expect("builds");
        assert_eq!(d.num_string(), "\u{2013}0.50");
    }

    #[test]
    fn minus_becomes_en_dash_at_mid_digit_height() {
        let book = book();
        let d = DecimalNumber::new(-1.0)
            .num_decimal_places(0)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "\u{2013}1");
        let children = d.vmob().children();
        assert_eq!(children.len(), 2);
        let (_, dash_max) = extent(&children[0]);
        let (digit_min, digit_max) = extent(&children[1]);
        let digit_h = digit_max[1] - digit_min[1];
        // align_to(next, UP) then shift down half the next glyph's height.
        let want = digit_max[1] - 0.5 * digit_h;
        assert!(
            (dash_max[1] - want).abs() < EPS,
            "en-dash top {} != {want}",
            dash_max[1]
        );
    }

    #[test]
    fn commas_group_every_three_digits() {
        let book = book();
        let d = DecimalNumber::new(1_234_567.891)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "1,234,567.89");
        let d = DecimalNumber::new(1_234_567.891)
            .group_with_commas(false)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "1234567.89");
    }

    #[test]
    fn zero_padding_reaches_min_total_width() {
        let book = book();
        let d = DecimalNumber::new(42.0)
            .min_total_width(6)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "042.00");
        // CPython: format(1234, "09,d") == "0,001,234".
        let d = DecimalNumber::new(1234.0)
            .num_decimal_places(0)
            .min_total_width(9)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "0,001,234");
        // CPython: format(123456, "08,d") == "0,123,456" — the overshoot.
        let d = DecimalNumber::new(123_456.0)
            .num_decimal_places(0)
            .min_total_width(8)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "0,123,456");
        // CPython: format(7, "+04d") == "+007".
        let d = DecimalNumber::new(7.0)
            .num_decimal_places(0)
            .include_sign(true)
            .min_total_width(4)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "+007");
    }

    #[test]
    fn formatting_budget_accepts_the_boundary_and_refuses_one_over() {
        let exact = DecimalNumber::new(42.0)
            .num_decimal_places(0)
            .group_with_commas(false)
            .min_total_width(8)
            .character_limit(8)
            .format_number()
            .expect("the declared boundary is admitted");
        assert_eq!(exact, "00000042");

        let err = DecimalNumber::new(42.0)
            .num_decimal_places(0)
            .group_with_commas(false)
            .min_total_width(9)
            .character_limit(8)
            .format_number();
        assert!(matches!(
            err,
            Err(TextMobjectError::ResourceLimit {
                context: "decimal-number minimum width",
                requested: 9,
                limit: 8,
            })
        ));

        // Grouping can overshoot a requested width at a comma boundary;
        // the resulting width, not merely `min_total_width`, is bounded.
        let err = DecimalNumber::new(123_456.0)
            .num_decimal_places(0)
            .min_total_width(8)
            .character_limit(8)
            .format_number();
        assert!(matches!(
            err,
            Err(TextMobjectError::ResourceLimit {
                context: "decimal-number numeric characters",
                requested: 9,
                limit: 8,
            })
        ));
    }

    #[test]
    fn formatting_budget_reports_host_count_overflow() {
        let err = DecimalNumber::new(0.0)
            .num_decimal_places(usize::MAX)
            .character_limit(usize::MAX)
            .format_number();
        assert!(matches!(
            err,
            Err(TextMobjectError::CapacityOverflow {
                context: "decimal-number fractional width",
            })
        ));

        let err = DecimalNumber::new(0.0)
            .num_decimal_places(0)
            .group_with_commas(false)
            .min_total_width(usize::MAX)
            .character_limit(usize::MAX)
            .format_number();
        assert!(matches!(
            err,
            Err(TextMobjectError::CapacityOverflow {
                context: "decimal-number padded digits",
            })
        ));
    }

    #[test]
    fn high_bounded_precision_suppresses_negative_zero_without_scaling() {
        let rendered = DecimalNumber::new(-0.0)
            .num_decimal_places(4_000)
            .character_limit(4_002)
            .format_number()
            .expect("bounded formatting");
        assert_eq!(rendered.chars().count(), 4_002);
        assert!(rendered.starts_with("0."));
        assert!(!rendered.contains(EN_DASH));
    }

    #[test]
    fn ellipsis_appends_one_glyph_child() {
        let book = book();
        let d = DecimalNumber::new(1.23456)
            .show_ellipsis(true)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "1.23");
        let children = d.vmob().children();
        assert_eq!(children.len(), 5, "four digits + one ellipsis child");
        assert!(!children[4].points().is_empty(), "the ellipsis has ink");
    }

    #[test]
    fn unit_aligns_down_unless_caret_then_up() {
        let book = book();
        let d = DecimalNumber::new(3.5)
            .unit("cm")
            .build(&book)
            .expect("builds");
        let children = d.vmob().children();
        assert_eq!(children.len(), 5, "four digits + one unit child");
        let (digit_min, _) = extent(&children[0]);
        let (unit_min, _) = extent(&children[4]);
        assert!(
            (unit_min[1] - digit_min[1]).abs() < EPS,
            "plain unit rides the baseline with the digits"
        );

        let d = DecimalNumber::new(3.5)
            .unit("^cm")
            .build(&book)
            .expect("builds");
        let children = d.vmob().children();
        assert_eq!(children.len(), 5);
        let (_, digit_max) = extent(&children[0]);
        let (_, unit_max) = extent(&children[4]);
        assert!(
            (unit_max[1] - digit_max[1]).abs() < EPS,
            "caret unit aligns UP: unit top {} != digit top {}",
            unit_max[1],
            digit_max[1]
        );
    }

    #[test]
    fn comma_drops_half_its_own_height() {
        let book = book();
        let d = DecimalNumber::new(1234.0)
            .num_decimal_places(0)
            .build(&book)
            .expect("builds");
        assert_eq!(d.num_string(), "1,234");
        let children = d.vmob().children();
        let (digit_min, _) = extent(&children[0]);
        let (comma_min, comma_max) = extent(&children[1]);
        let comma_h = comma_max[1] - comma_min[1];
        let want = digit_min[1] - 0.5 * comma_h;
        assert!(
            (comma_min[1] - want).abs() < EPS,
            "comma bottom {} != {want}",
            comma_min[1]
        );
    }

    #[test]
    fn glyph_recycling_typesets_only_new_characters() {
        let book = book();
        let mut d = DecimalNumber::new(12.0).build(&book).expect("builds");
        assert_eq!(d.typesets(), 4, "'1', '2', '.', '0'");
        d.set_value(21.0, &book).expect("set");
        assert_eq!(d.num_string(), "21.00");
        assert_eq!(d.typesets(), 4, "the whole alphabet was already cached");
        d.set_value(35.0, &book).expect("set");
        assert_eq!(d.typesets(), 6, "'3' and '5' are new, one typeset each");
        d.set_value(-3.0, &book).expect("set");
        assert_eq!(d.typesets(), 7, "the en dash is one new character");
        d.set_value(-3.33, &book).expect("set");
        assert_eq!(d.typesets(), 7, "nothing new again");
    }

    #[test]
    fn digits_are_buffed_by_digit_buff_per_font_unit() {
        let book = book();
        let d = DecimalNumber::new(11.0)
            .num_decimal_places(0)
            .build(&book)
            .expect("builds");
        let children = d.vmob().children();
        assert_eq!(children.len(), 2);
        let (a_min, a_max) = extent(&children[0]);
        let (b_min, _) = extent(&children[1]);
        let buff = 0.001 * 48.0;
        assert!(
            (b_min[0] - a_max[0] - buff).abs() < EPS,
            "gap {} != digit_buff {buff}",
            b_min[0] - a_max[0]
        );
        // ... and the origins differ by digit width + buff.
        let width = a_max[0] - a_min[0];
        assert!(
            (b_min[0] - a_min[0] - (width + buff)).abs() < EPS,
            "pitch {} != width + buff {}",
            b_min[0] - a_min[0],
            width + buff
        );
    }

    #[test]
    fn edge_to_fix_survives_set_value() {
        let book = book();
        // The constructor centers at the origin.
        let mut d = DecimalNumber::new(0.0).build(&book).expect("builds");
        let c = d.vmob().center_point();
        assert!(c[0].abs() < EPS && c[1].abs() < EPS, "centered at ORIGIN");
        let (left0, _) = extent(d.vmob());
        let left0 = left0[0];
        d.set_value(-12345.6, &book).expect("set");
        let (left1, _) = extent(d.vmob());
        assert!(
            (left1[0] - left0).abs() < EPS,
            "LEFT edge moved: {left0} -> {}",
            left1[0]
        );

        let mut d = DecimalNumber::new(0.0)
            .edge_to_fix(fmn_core::constants::RIGHT)
            .build(&book)
            .expect("builds");
        let (_, right0) = extent(d.vmob());
        let right0 = right0[0];
        d.set_value(987654.3, &book).expect("set");
        let (_, right1) = extent(d.vmob());
        assert!(
            (right1[0] - right0).abs() < EPS,
            "RIGHT edge moved: {right0} -> {}",
            right1[0]
        );
    }

    #[test]
    fn integer_truncates_display_and_rounds_value() {
        let book = book();
        let i = Integer::new(2.7).build(&book).expect("builds");
        assert_eq!(i.num_string(), "2", "display truncates (int(number))");
        assert_eq!(i.value(), 3, "get_value rounds");
        assert_eq!(Integer::new(2.5).build(&book).expect("b").value(), 2);
        assert_eq!(Integer::new(3.5).build(&book).expect("b").value(), 4);
        assert_eq!(Integer::new(-2.5).build(&book).expect("b").value(), -2);
        let i = Integer::new(-1.2).build(&book).expect("builds");
        assert_eq!(i.num_string(), "\u{2013}1", "int(-1.2) == -1");
    }

    #[test]
    fn default_style_is_the_reference_text_style() {
        let book = book();
        let d = DecimalNumber::new(5.0).build(&book).expect("builds");
        for child in d.vmob().children() {
            let s = child.style();
            assert_eq!(s.stroke_width, 0.0);
            assert_eq!(s.fill_opacity, 1.0);
            assert_eq!(s.fill_border_width, 0.5);
            assert_eq!(s.fill_color, DEFAULT_MOBJECT_COLOR);
        }
    }

    #[test]
    fn background_rectangle_is_child_zero_and_covers_the_digits() {
        let book = book();
        let d = DecimalNumber::new(42.0)
            .include_background_rectangle(true)
            .build(&book)
            .expect("builds");
        let children = d.vmob().children();
        assert_eq!(
            children.len(),
            d.num_string().chars().count() + 1,
            "background + digit children"
        );
        let rect = &children[0];
        assert_eq!(rect.style().fill_color, BLACK);
        assert_eq!(rect.style().fill_opacity, 1.0);
        let (r_min, r_max) = extent(rect);
        let (d_min, d_max) = union_extent(&children[1..]).expect("digits");
        for k in 0..2 {
            assert!(r_min[k] <= d_min[k] + EPS, "rect covers digit min");
            assert!(r_max[k] >= d_max[k] - EPS, "rect covers digit max");
        }
    }

    #[test]
    fn increment_value_adds() {
        let book = book();
        let mut d = DecimalNumber::new(1.0).build(&book).expect("builds");
        d.increment_value(0.5, &book).expect("increment");
        assert_eq!(d.value(), 1.5);
        assert_eq!(d.num_string(), "1.50");
    }

    #[test]
    fn refused_value_rebuild_preserves_value_string_and_visible_geometry() {
        let book = book();
        let mut decimal = DecimalNumber::new(1.0)
            .num_decimal_places(0)
            .character_limit(2)
            .build(&book)
            .unwrap();
        let before = decimal.vmob().clone();
        assert!(matches!(
            decimal.set_value(1234.0, &book),
            Err(TextMobjectError::ResourceLimit { .. })
        ));
        assert_eq!(decimal.value(), 1.0);
        assert_eq!(decimal.num_string(), "1");
        assert_eq!(decimal.vmob(), &before);

        // The numeric glyphs may be cached successfully before a later
        // unit glyph refuses. The public value/string/geometry still agree.
        decimal = decimal.character_limit(16).unit("\u{10ffff}");
        let before_typesets = decimal.typesets();
        assert!(decimal.set_value(23.0, &book).is_err());
        assert_eq!(decimal.value(), 1.0);
        assert_eq!(decimal.num_string(), "1");
        assert_eq!(decimal.vmob(), &before);
        assert_eq!(decimal.typesets(), before_typesets + 2);
        decimal = decimal.unit("m");
        decimal.set_value(23.0, &book).unwrap();
        assert_eq!(decimal.value(), 23.0);
        assert_eq!(decimal.num_string(), "23");
        assert_eq!(decimal.typesets(), before_typesets + 3);
    }

    #[test]
    fn value_animation_accepts_a_single_digit_character_budget() {
        use fmn_anim::{AnimConfig, RateFunc};

        let expected_book = book();
        let book = book();
        let mut stage = Stage::new();
        let decimal = DecimalNumber::new(0.0)
            .num_decimal_places(0)
            .group_with_commas(false)
            .character_limit(1)
            .build(&book)
            .unwrap();
        let mob = add_decimal_for_value_animation(&mut stage, &decimal, &book).unwrap();
        stage.shift(mob, [2.0, 1.0, 0.0]);
        let anchor = stage.get_bounding_box_point(mob, LEFT);
        let mut animation = change_decimal_to_value(mob, decimal, book, 9.0)
            .unwrap()
            .with_config(AnimConfig {
                rate_func: RateFunc::linear(),
                ..AnimConfig::default()
            });
        animation.begin(&mut stage).unwrap();
        animation.interpolate(&mut stage, 0.5);
        animation.finish(&mut stage);
        assert!(animation.deferred_error().is_none());
        assert_eq!(stage.family(mob).len(), 2);
        assert_point_close(stage.get_bounding_box_point(mob, LEFT), anchor);
        let expected = stage.add(
            DecimalNumber::new(9.0)
                .num_decimal_places(0)
                .build(&expected_book)
                .unwrap(),
        );
        stage.move_to(expected, anchor, LEFT);
        assert_number_geometry(&stage, mob, expected);
    }

    #[test]
    fn refused_initial_number_does_not_allocate_an_arena_family() {
        let book = book();
        let mut stage = Stage::new();
        let before = stage.snapshot_bytes().unwrap();
        let decimal = DecimalNumber::new(1000.0)
            .num_decimal_places(0)
            .character_limit(2);
        assert!(add_decimal_for_value_animation(&mut stage, &decimal, &book).is_err());
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
    }

    #[test]
    fn failed_number_sample_is_reported_once_without_publishing_a_partial_family() {
        use std::cell::Cell;
        use std::rc::Rc;

        use fmn_anim::{AnimConfig, RateFunc};

        let book = book();
        let mut stage = Stage::new();
        let decimal = DecimalNumber::new(1.0)
            .num_decimal_places(0)
            .character_limit(2)
            .build(&book)
            .unwrap();
        let mob = add_decimal_for_value_animation(&mut stage, &decimal, &book).unwrap();
        let calls = Rc::new(Cell::new(0));
        let observed = Rc::clone(&calls);
        let mut animation = changing_decimal(mob, decimal, book, move |alpha| {
            observed.set(observed.get() + 1);
            if alpha < 0.5 { 22.0 } else { 1234.0 }
        })
        .unwrap()
        .with_config(AnimConfig {
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        });
        animation.begin(&mut stage).unwrap();
        assert_eq!(calls.get(), 1);
        let good_frame = stage.snapshot_bytes().unwrap();
        animation.interpolate(&mut stage, 0.5);
        let failure = animation.deferred_error().unwrap();
        assert!(matches!(
            &failure,
            AnimError::UpdateFailed {
                operation: "DecimalNumber::set_value",
                detail,
            } if detail.contains("decimal-number")
        ));
        assert_eq!(stage.snapshot_bytes().unwrap(), good_frame);
        animation.interpolate(&mut stage, 0.9);
        assert_eq!(calls.get(), 2);
        assert_eq!(animation.deferred_error(), Some(failure));
        assert_eq!(stage.snapshot_bytes().unwrap(), good_frame);
        animation.finish(&mut stage);
        assert_eq!(calls.get(), 2);

        animation.begin(&mut stage).unwrap();
        assert_eq!(calls.get(), 3);
        assert!(animation.deferred_error().is_none());
        animation.abort(&mut stage);
    }

    #[test]
    fn a_stale_number_handle_refuses_before_calling_the_value_function() {
        use std::cell::Cell;
        use std::rc::Rc;

        let book = book();
        let mut stage = Stage::new();
        let decimal = DecimalNumber::new(1.0).build(&book).unwrap();
        let mob = add_decimal_for_value_animation(&mut stage, &decimal, &book).unwrap();
        let calls = Rc::new(Cell::new(0));
        let observed = Rc::clone(&calls);
        let mut animation = changing_decimal(mob, decimal, book, move |_| {
            observed.set(observed.get() + 1);
            2.0
        })
        .unwrap();
        stage.delete(mob).unwrap();
        let before = stage.snapshot_bytes().unwrap();
        animation.interpolate(&mut stage, 0.5);
        assert_eq!(
            animation.deferred_error(),
            Some(AnimError::StaleHandle(mob))
        );
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn value_animation_preserves_affine_placement_live_style_background_and_uniforms() {
        use fmn_core::constants::BLUE;
        use fmn_mobject::Placement;

        use crate::style::VStyle;

        let expected_book = book();
        let book = book();
        let mut stage = Stage::new();
        let decimal = DecimalNumber::new(9.0)
            .num_decimal_places(0)
            .include_background_rectangle(true)
            .show_ellipsis(true)
            .unit("m")
            .edge_to_fix(RIGHT)
            .build(&book)
            .unwrap();
        let mob = add_decimal_for_value_animation(&mut stage, &decimal, &book).unwrap();
        let affine = Placement::new(
            [[0.0, -1.5, 0.0], [2.0, 0.25, 0.0], [0.0, 0.0, 1.0]],
            [3.0, -2.0, 0.5],
        );
        stage.apply_affine(mob, affine);
        stage.set_fill(mob, Some(BLUE), Some(0.4), Some(1.25), true);
        stage.set_stroke(mob, Some(BLUE), Some(2.0), Some(0.3), Some(true), true);
        let background = stage.get(mob).unwrap().submobjects()[0];
        stage.set_fill(background, Some(BLACK), Some(1.0), Some(0.0), true);
        stage.set_stroke(background, None, Some(0.0), Some(0.0), None, true);
        stage.set_fixed_in_frame(mob, 1.0, true);
        stage.set_z_index(mob, 7, true);
        let uniforms = *stage.uniforms(mob).unwrap();
        let anchor = stage.get_bounding_box_point(mob, RIGHT);
        let mut animation = changing_decimal(mob, decimal.clone(), book, |_| 1234.0).unwrap();
        animation.begin(&mut stage).unwrap();
        animation.finish(&mut stage);
        assert!(animation.deferred_error().is_none());
        assert_point_close(stage.get_bounding_box_point(mob, RIGHT), anchor);
        assert_eq!(stage.z_index(mob), 7);
        assert_eq!(stage.placement(mob).unwrap().linear(), affine.linear());
        assert_eq!(
            stage.get(mob).unwrap().buffer.default_record("fill_rgba"),
            Some(vec![BLUE.r as f32, BLUE.g as f32, BLUE.b as f32, 0.4])
        );
        let children = stage.get(mob).unwrap().submobjects();
        assert_eq!(children.len(), 8, "background, 1,comma,2,3,4,ellipsis,unit");
        let background = children[0];
        assert_eq!(stage.get_fill_color(background), Some(BLACK));
        assert_eq!(stage.get_fill_opacity(background), Some(1.0));
        for member in stage.family(mob).into_iter().skip(1) {
            assert_eq!(stage.uniforms(member), Some(&uniforms));
            assert_eq!(stage.z_index(member), 7);
            if member != background && !stage.get(member).unwrap().buffer.is_empty() {
                let color = stage.get_fill_color(member).unwrap();
                assert_point_close([color.r, color.g, color.b], [BLUE.r, BLUE.g, BLUE.b]);
                assert!((stage.get_fill_opacity(member).unwrap() - 0.4).abs() < 1e-6);
                assert_eq!(stage.get_stroke_width(member), Some(2.0));
            }
        }

        let mut expected = decimal;
        expected.set_value(1234.0, &expected_book).unwrap();
        let expected = stage.add(expected);
        stage.apply_affine(expected, affine);
        stage.move_to(expected, anchor, RIGHT);
        assert_number_geometry(&stage, mob, expected);
    }

    #[test]
    fn count_in_from_clips_an_overshooting_rate_at_the_target() {
        use fmn_anim::{AnimConfig, RateFunc};

        let expected_book = book();
        let book = book();
        let mut stage = Stage::new();
        let decimal = DecimalNumber::new(5.0).build(&book).unwrap();
        let mob = add_decimal_for_value_animation(&mut stage, &decimal, &book).unwrap();
        let anchor = stage.get_bounding_box_point(mob, LEFT);
        let mut animation =
            count_in_from(mob, decimal, book, 1.0)
                .unwrap()
                .with_config(AnimConfig {
                    rate_func: RateFunc::linear().not_quite_there(2.0),
                    ..AnimConfig::default()
                });
        animation.begin(&mut stage).unwrap();
        animation.interpolate(&mut stage, 0.75);
        let expected = stage.add(DecimalNumber::new(5.0).build(&expected_book).unwrap());
        stage.move_to(expected, anchor, LEFT);
        assert_number_geometry(&stage, mob, expected);
        assert!(animation.deferred_error().is_none());
        animation.finish(&mut stage);
    }

    #[test]
    fn regenerated_digits_keep_live_gradient_paint_and_stroke_profiles() {
        let book = book();
        let mut stage = Stage::new();
        let decimal = DecimalNumber::new(8.0)
            .num_decimal_places(0)
            .build(&book)
            .unwrap();
        let mob = add_decimal_for_value_animation(&mut stage, &decimal, &book).unwrap();
        let digit = first_digit(&stage, mob, false);
        let entry = stage.get_mut(digit).unwrap();
        let count = entry.buffer.len();
        assert!(count > 1);
        let mut colors = Vec::with_capacity(count * 4);
        let mut widths = Vec::with_capacity(count);
        for index in 0..count {
            let alpha = index as f32 / (count - 1) as f32;
            colors.extend_from_slice(&[alpha, 0.25, 1.0 - alpha, 1.0]);
            widths.push(1.0 + 4.0 * alpha);
        }
        entry.buffer.write_range("fill_rgba", 0, &colors);
        entry.buffer.write_range("stroke_width", 0, &widths);
        let mut animation = changing_decimal(mob, decimal, book, |_| 2.0).unwrap();
        animation.begin(&mut stage).unwrap();
        let entry = stage.get(first_digit(&stage, mob, false)).unwrap();
        let new_count = entry.buffer.len();
        assert_ne!(new_count, count, "the glyph change must resample its style");
        for index in 0..new_count {
            let alpha = index as f32 / (new_count - 1) as f32;
            let color = entry.buffer.read(index, "fill_rgba").unwrap();
            assert!((color[0] - alpha).abs() < 1e-6);
            assert!((color[1] - 0.25).abs() < 1e-6);
            assert!((color[2] - (1.0 - alpha)).abs() < 1e-6);
            assert_eq!(color[3], 1.0);
            let width = entry.buffer.read(index, "stroke_width").unwrap()[0];
            assert!((width - (1.0 + 4.0 * alpha)).abs() < 1e-6);
        }
        assert!(animation.deferred_error().is_none());
        animation.finish(&mut stage);
    }

    #[test]
    fn the_family_enters_the_arena_as_a_family() {
        let mut stage = Stage::new();
        let d = DecimalNumber::new(12.0).build(&book()).expect("builds");
        let n = d.vmob().children().len();
        let mob = stage.add(d.into_vmob());
        assert_eq!(stage.family(mob).len(), n + 1, "parent + digit children");
        // The built family sits centered at the origin.
        let c = stage.get_center(mob);
        assert!(
            (c[0] - ORIGIN[0]).abs() < EPS && (c[1] - ORIGIN[1]).abs() < EPS,
            "arena center {c:?}"
        );
    }

    #[test]
    fn changing_decimal_drives_the_displayed_value_through_the_stage() {
        let expected_book = book();
        let book = book();
        let mut stage = Stage::new();
        let decimal = DecimalNumber::new(0.0).build(&book).expect("builds");
        let mob = add_decimal_for_value_animation(&mut stage, &decimal, &book)
            .expect("number family added");
        let mut anim = changing_decimal(mob, decimal, book, |alpha| 2.0 * alpha)
            .expect("current value builds on the bundled book");
        anim.interpolate(&mut stage, 1.0);

        let expected = stage.add(
            DecimalNumber::new(2.0)
                .build(&expected_book)
                .expect("builds"),
        );
        assert_eq!(
            point_count(&stage, mob),
            point_count(&stage, expected),
            "the 2.0 render's structure is installed"
        );
        // Placement: set_value re-anchors through edge_to_fix with recycled
        // glyphs, so absolute coordinates may drift from a fresh build when
        // digit widths differ — the display must simply BE the new value's
        // ink, not the old one's.
        let stale = stage.add(
            DecimalNumber::new(0.0)
                .build(&expected_book)
                .expect("builds"),
        );
        let p0 = stage.get_points(first_digit(&stage, mob, false)).unwrap()[0];
        let old = stage.get_points(first_digit(&stage, stale, false)).unwrap()[0];
        let drifted = (p0[0] - old[0]).abs() > 1e-6 || (p0[1] - old[1]).abs() > 1e-6;
        assert!(drifted, "the installed ink is the new value's, not 0.00's");
        stage.move_to(expected, stage.get_bounding_box_point(mob, LEFT), LEFT);
        assert_number_geometry(&stage, mob, expected);
        assert!(anim.deferred_error().is_none());
    }

    #[test]
    fn change_decimal_to_value_interpolates_halfway() {
        let expected_book = book();
        let book = book();
        let mut stage = Stage::new();
        let decimal = DecimalNumber::new(1.0).build(&book).expect("builds");
        let mob = add_decimal_for_value_animation(&mut stage, &decimal, &book)
            .expect("number family added");
        let mut anim = change_decimal_to_value(mob, decimal, book, 5.0).expect("wired");
        anim.interpolate(&mut stage, 0.5);

        // value 3.0 renders as "3.00" — same glyph count as "1.00".
        let expected = stage.add(
            DecimalNumber::new(3.0)
                .build(&expected_book)
                .expect("builds"),
        );
        stage.move_to(expected, stage.get_bounding_box_point(mob, LEFT), LEFT);
        assert_number_geometry(&stage, mob, expected);
        assert!(anim.deferred_error().is_none());
    }

    #[test]
    fn count_in_from_rises_from_the_source() {
        let expected_book = book();
        let book = book();
        let mut stage = Stage::new();
        let decimal = DecimalNumber::new(5.0).build(&book).expect("builds");
        let mob = add_decimal_for_value_animation(&mut stage, &decimal, &book)
            .expect("number family added");
        let mut anim = count_in_from(mob, decimal, book, 0.0).expect("wired");
        anim.interpolate(&mut stage, 0.0);
        let at_source = stage.add(
            DecimalNumber::new(0.0)
                .build(&expected_book)
                .expect("builds"),
        );
        assert_eq!(
            point_count(&stage, mob),
            point_count(&stage, at_source),
            "alpha zero shows the source value"
        );
        stage.move_to(at_source, stage.get_bounding_box_point(mob, LEFT), LEFT);
        assert_number_geometry(&stage, mob, at_source);
        anim.interpolate(&mut stage, 1.0);
        let at_target = stage.add(
            DecimalNumber::new(5.0)
                .build(&expected_book)
                .expect("builds"),
        );
        assert_eq!(
            point_count(&stage, mob),
            point_count(&stage, at_target),
            "alpha one shows the target value"
        );
        stage.move_to(at_target, stage.get_bounding_box_point(mob, LEFT), LEFT);
        assert_number_geometry(&stage, mob, at_target);
        assert!(anim.deferred_error().is_none());
    }
}
