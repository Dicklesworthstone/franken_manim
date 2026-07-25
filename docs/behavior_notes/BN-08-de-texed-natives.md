# BN-08 — The de-TeX'd natives

**Status:** Partial. Drafted in W7 (fm-y69) with the first native
constructions — `Brace`/`BraceLabel`/`LineBrace`, `SurroundingRectangle`,
`BackgroundRectangle`, `Cross`, `Underline`, `Checkmark`, `Exmark`. The
text-backed members (`DecimalNumber`/`Integer`, the `Matrix` family,
`BulletedList`, `Title`) land with the Scribe bridge and extend this note in
place.

> **Numbering.** §16.8 of the plan assigns BN-08 to "de-TeX'd classes
> (§11.6)", which is this note. A file named `BN-08-animation-contract.md`
> already occupies the number on disk, and `BN-07` is duplicated across two
> files. The plan's register is normative, so this note takes BN-08;
> reconciling the strays is filed separately rather than done here, because
> renumbering someone else's published note is a documentation act with its
> own review.

## What changed

The Reference routes a set of **non-mathematical** classes through LaTeX,
because LaTeX was already a hard dependency and therefore free at the margin.
It is not free here — it is the dependency this whole program exists to
delete — and several of those classes were never well served by it anyway:

| Class | Reference mechanism | FrankenManim |
|---|---|---|
| `Brace`, `BraceLabel`, `BraceText`, `LineBrace` | render `\underbrace{\qquad}`, then stretch the glyph | a **parametric path family** |
| `Checkmark`, `Exmark` | `\ding{51}` / `\ding{55}` via the `pifont` package | drawn paths |
| `Cross`, `Underline`, `SurroundingRectangle`, `BackgroundRectangle` | already native, but built on Tex-derived metrics in places | pure geometry over the target's box |
| `DecimalNumber`, `Integer`, the `Matrix` family, `BulletedList`, `Title` | `Tex`/`TexText` | native text through Scribe *(pending)* |

Nothing about the mathematics changes. These classes were never doing
mathematics; they were borrowing a typesetter to draw a curly bracket.

## Why the brace is the interesting one

`Brace` is not a de-TeXing of convenience — the Reference's implementation is
**incorrect at the edges**, and in a way that is intrinsic to the approach.

It renders one fixed drawing and then resizes it. Widening is handled by a
special case over six hard-coded submobject indices of that render (stretch
the two straight runs, shift the tips outward). Narrowing has no special case
at all and falls through to `set_width(width, stretch=True)`, which squashes
the curl horizontally along with everything else. A brace over a narrow column
in the Reference is a compressed caricature of a brace over a wide one.

FrankenManim generates the brace instead. The curl — the two end hooks and the
centre point — has its own size; the straight runs absorb all the width. Three
clamps make the family well-formed at *every* positive width:

```text
cap   ≤ 0.20 · w  and  waist ≤ 0.20 · w   ⇒  cap + waist ≤ 0.40 w < 0.50 w
thickness ≤ 0.10 · w                       ⇒  thickness < cap, and the two
                                              inner hooks cannot cross
height = thickness + 2·reach > 1.2·thickness
```

The first says an end hook can never reach the centre point, so the runs never
invert. The second says the inner edge stays inside the span — without it a
brace narrower than its own stroke is inside out, which is not a hypothetical:
it was caught by the property test at width 0.01 during development. The third
keeps the centre point's inner edge from punching through its own runs.

These are proofs about the family, not tunings. `Brace` is correct at any
width by construction, which is a claim the Reference cannot make.

The tip is likewise exact. The Reference finds it by scanning the rendered
glyph for its minimum-`y` point (`tip_point_index = np.argmin(...)`); here it
is closed-form, because the shape is known rather than discovered.

## Why Checkmark and Exmark are drawn, not set from a bundled glyph

The intent was to take both from bundled faces. The faces were checked rather
than assumed:

| Codepoint | Character | Computer Modern | IBM Plex Sans | Noto Sans Math |
|---|---|---|---|---|
| U+2713 | ✓ CHECK MARK | no | **yes** | **yes** |
| U+2717 | ✗ BALLOT X | no | no | no |
| U+2714 / U+2718 / U+2715 | heavy variants | no | no | no |

A glyph exists for the check; **none exists for the cross**, in any bundled
face. Three ways out were available: substitute U+00D7 (present, but a thin
multiplication sign — wrong weight, wrong shape for `\ding{55}`), bundle a
dingbat face for two glyphs, or draw both.

Both are drawn. Drawing only the cross would leave a matched pair mismatched:
the Reference's two dingbats come from one font and read as siblings, and an
IBM Plex check beside a hand-drawn cross would not. This also follows the
precedent ADR-0005 already set for extensible delimiters, where the drawn path
is the *mainline* rather than a fallback, for exactly the same reason — the
authored glyph is not there to be had.

Both marks are built in the same unit box, so they scale and align
identically.

## Migration guidance

- **Metrics differ.** These constructions are ours, so their exact
  dimensions differ from the LaTeX-derived originals. Scenes that hard-code
  offsets measured against a Reference brace's height, or against a
  `\ding{51}`'s width, need re-measuring. Scenes that position with
  `next_to`, `get_tip`, `put_at_tip`, or the positional API — which is nearly
  all of them — need no change.
- **Scaling behaviour is better defined, and therefore different.** A brace
  much narrower or much wider than its natural size now keeps its curl's
  proportions. If a scene relied on the Reference's distortion for a visual
  effect, it will look different — better, but different.
- **Stroke tapers are reproduced.** `Cross` and `Underline` keep their
  variable stroke widths (`[0, 6, 0]` and `[0, 3, 3, 0]`), resized onto the
  path by linear interpolation the way `set_stroke(width=[...])` does. The
  taper is the visual character of those classes, not a detail.
- **No LaTeX escape hatch.** There is deliberately no way to ask for the old
  rendering. `pifont` is gone; so is every other TeX package.

## Evidence

- `crates/fmn-library/src/brace.rs` — the parametric family, with
  `braces_never_self_intersect_at_any_width` checking all three invariants
  across widths from 0.001 to 200, and `the_curl_keeps_its_size_as_the_brace_widens`
  pinning the property the Reference lacks.
- `crates/fmn-library/src/matchers.rs` — the matchers and marks, with the
  cmap survey recorded in the module documentation and
  `both_marks_are_closed_shapes_in_the_same_unit_box` holding the pair to a
  matched size.
- `crates/fmn-library/src/vmobject.rs` — `with_stroke_profile` and
  `resize_with_interpolation`, the taper mechanism.
