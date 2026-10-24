# BN-05 — Native text typesetting: bundled faces, owned shaping, no Pango

**Status:** Draft.

**Subsystem:** Scribe I (fmn-text) · **Plan:** §11.2, D-08 · **Bead:** fm-u1u

## What the Reference does

`Text`/`MarkupText` render through Pango against **host fonts** (the
Reference's default resolves to whatever the platform maps its default
family to — commonly Consolas on Windows-authored scenes), with Pango's
shaping, ligatures, and line metrics. Output therefore differs between
machines with different font installations and Pango versions, and a
missing font silently substitutes.

## What FrankenManim does

- **The default text face is bundled Computer Modern** (CM Unicode), the
  3b1b typographic identity; CM Typewriter serves `<tt>`, IBM Plex Sans is
  the bundled sans. A bare install renders every built-in scene
  identically on every machine.
- **Shaping is owned**: cmap→gids, kerning from the legacy `kern` table
  plus the GPOS `kern` feature (CM Unicode kerns through GPOS), and the
  bundled faces' **ligature sets off by default** — matching the familiar
  manim look — with an explicit opt-in flag.
- **A missing font family is a named error**
  (`font family 'X' is not available; bundled families: …`), never a
  silent substitution. User TTFs load from bytes under the capability
  doctrine; the family-name lookup tolerates the bundled aliases only.
- **Per-character coverage falls back through a fixed, bundled chain.** A
  character the selected face has no glyph for (U+2212 MINUS SIGN, `×`,
  `→`, `½` in the Computer Modern subset) is drawn from Computer Modern,
  then the Noto Sans Math symbol face, then IBM Plex Sans — the first
  that covers it — and the glyph records the face it came from. Like
  Pango's fallback, but compiled in, so identical on every machine. Only
  a character no bundled face covers is a named `UnmappedChar` error.
- **Line metrics are fixed constants** (1.2 em baseline skip ×
  `line_spacing`, greedy breaking with manim's width semantics;
  least-badness is an explicit option), not Pango's font-derived leading.
- **Markup** is the manim tag set with precise line:column diagnostics —
  unknown tags/attributes are errors, not passthrough.
- **Empty input constructs an empty mobject.** At the pin, `TexText()`,
  `Title()`, `BulletedList()` and `TexTextFromPresetString()` with no text
  raise `LatexError` ("There's no line here to end", "perhaps a missing
  \item"), because the Reference compiles an empty LaTeX body. Native
  typesetting returns a point-less mobject with no glyph children instead
  (`Title` keeps its underline), which can be filled later with `become`
  or animated in. Found by the
  Appendix-A class sweep (fm-5wq.26).

## Why

Determinism and sovereignty: text output must be a pure function of the
input closure. Host-font resolution, Pango versioning, and silent
substitution all break that; the bundled set plus owned shaping closes
the pipeline (§4). Correctness: `Text[3:7]`/`isolate=` keep the
Reference's structural conventions (non-whitespace glyphs in order, a
ligature is one submobject), so scene code indexes identically.

## Migration

- Scenes that relied on a specific host font must load it explicitly
  (`FontBook::add_family`) — the name error tells you at once.
- Exact glyph metrics (advances, kerns, line heights) differ from any
  Pango render; positions shift at the sub-em scale. Layout-relative code
  (`next_to`, `align_to`) is unaffected; pixel-locked expectations are
  not honored anywhere in FrankenManim by design.
- **The magnitude is bounded** and checked by the structural differential's
  envelopes (fm-5wq.45). Text is a different outline, not a different size
  or place:
  - `Tex`-family boxes stay within 10% of the Reference's extent and
    0.05 units of its centre, since both engines set Computer Modern.
  - `Text`, `MarkupText` and `Code` boxes stay within 25% and 0.05 units.
    The Reference's own extent depends on its host's fonts: `Text('Hello')`
    measured 1.28 and 1.049 wide on two hosts.
  - A glyph regrouped out of its text stays within 25% and 0.1 units.
  - A larger difference is a bug with its own bead. For example,
    `fm-tex-metrics-glyphs-ru72` covered fallback glyphs and measured-ink
    boxes. Since fmd-math lays out from TeX's TFM metrics, 105 of the 111
    authored formulas in `tex_reference_boxes` are within 5% of the
    Reference's `Tex` box.
- **`Tex` is 0.5% smaller than on the Reference's capture host.** Both
  engines scale a formula so that a "0" is `font_size /
  font_size_for_unit_height` tall. FrankenManim measures its own "0": the
  Computer Modern design, 0.688 em of ink.
  - The Reference measures `latex_to_svg("0")`, a T1 text "0". Without
    cm-super's outline fonts, dvisvgm traces that glyph from a METAFONT
    bitmap, and it measures 0.6846 em. That is the pinned capture host's case.
  - Every Reference `Tex` there is therefore 0.49% larger than its design
    proportions.
  - This is a property of the host's font installation, not of manim, and is
    not replicated.
- Pango-only markup (arbitrary attributes, `<gravity>`, etc.) is out of
  the compatibility claim; the supported set is documented in
  `fmn_text::markup`.
- `\!`, `\:` and `\;` in mathematics are TeX's −3, 4 and 5 mu spaces. The
  Reference's default template loads `tipa`, which turns each into a
  one-token box with no space (Appendix C-27): there `x\;y` sets as `xy`,
  `x\;=y` loses the relation spacing around `=`, and `a\!\!b` does not
  compile. A formula that asks for a space gets it.

## Source selection and matching through the Python portal

String selectors now use the native source spans throughout the portal.
Literal lookup delegates to Scribe's literal occurrence finder; regular
expressions and explicit ranges are converted to the same UTF-8 byte intervals.
The Python-facing selector offsets remain character offsets. Native selection
retains only whole primitives inside the requested range, in emission order.

For example, selecting `"i"` in `Tex(r"e^{i\pi} + 1 = 0")` reaches the
exponent's i and leaves the pi glyph untouched. An occurrence inside a command
does not own that command's ink. Selection, color maps, direct source-range
queries, and matching keys all consume this same boundary. Copies and live
numeric replacements use their current native span/path metadata.

`substr_to_path_count` now counts the primitives represented by a matching
source occurrence. This fixes the former character-count behavior for commands
such as `\sin`, `\frac` and `\sqrt`. A TeX snippet absent from the live formula
is typeset through the existing native template and preamble engine. This
deliberately differs from the pinned Reference's approximate command-count
table when the native primitive decomposition differs. A live numeric group
counts as one source-span part; plain-text whitespace slots have no paths and
do not contribute.

`TransformMatchingTex` uses the same repeated longest-block matching as
`TransformMatchingStrings`, matching the pinned Reference's alias relationship.
Explicit pairs and `key_map` renames claim their parts first; `matched_keys`
adds priority matches while the remaining shared blocks still match. Reordered
blocks are preserved, and already-claimed parts cannot be consumed again.
Under D-09, unrelated source keys never match merely because their outlines
look alike. Use explicit pairs or a key map for an authored rename.

TeX color maps run once, after the constructor's public style hook, and once
when `init_points()` directly regenerates the native glyphs. Literal, regex and
range keys share this pass; no native-first duplicate color pass remains.

The independent native selection regression is in
`crates/fmn-tex/tests/scribe2.rs`; the compact text/TeX map checks are in
`crates/fmn-library/src/spans.rs`. Installed-extension regressions in
`crates/fmn-python/tests/text_selection.py` cover selection, coloring, counts,
copying, numeric replacement and matching. The source-level matching protocol
suites use explicit native-boundary doubles and are not rendering parity
evidence.

Against the pinned Reference, `crates/fmn-python/tests/tex_selection_reference.py`
runs `tex[sel]`, `t2c`, `isolate`, `substr_to_path_count` and
`TransformMatchingTex` over the box oracle's 111 authored formulas, 631
selectors and 50 formula pairs, and compares glyphs by position with the
Reference's recorded selections
(`crates/fmn-conformance/fixtures/tex_selection_reference.v1.ndjson`, captured by
`scripts/capture_tex_selection.py`). Of 2352 rows, 1998 agree. 296 differ
because the Reference's symbol-count slicing contradicts its own isolated
selection (Appendix C-36): `Tex(r"e^{i\pi} + 1 = 0")["i"]` selects the i and
the `+` there and the i here. 36 differ because the Reference's labelling braces
make LaTeX refuse the formula (C-37): `t2c={"(": RED}` on
`\left( \frac{a}{b} \right)` raises there and colours the parenthesis here. 22
rows of `\iint_D f\,dA` remain fm-zxxy's. The native CM row at `439a910`
is one selectable part, but follows amsmath's repeated-integral metrics;
the pinned Reference loads wasysym, which redefines the integral. Live TeX
measures the complete display formula at 41.4865 pt with amsmath and
34.78821 pt with wasysym. The native public box remains 19.1% wider than
the Reference. This is an unresolved Reference comparison, not an accepted
exception or a closed acceptance criterion; its existing owned rows and
the box and ink floors remain in force. The check runs in
the e2e catalog as `parity.tex_selection_reference.v1` and in the
installed-wheel gate.

## TeX colour commands

`\color` and `\textcolor` colour their glyphs, rules and drawn paths. The
pinned Reference loads xcolor but repaints every Tex glyph in its base colour:
`Tex(r"\color{red} x + y")` measures `#FFFFFF` on all three submobjects there
(measured 2026-10-20, fm-tex-color-silent-i0p2), so the commands are silently
inert. FrankenManim does what the source asks:

- The surface is plain xcolor's: the 19 base names (`red`, `orange`, `teal`,
  `lightgray`, …), mixes such as `blue!70` and `red!30!blue` (linear in RGB, as
  xcolor computes them), a leading `-` for the complement, and the `[rgb]`,
  `[RGB]`, `[HTML]` and `[gray]` models. Anything else is a precise parse error
  naming that set. That includes manim's palette names (`\color{RED}`), which
  are an undefined-colour LaTeX error in the Reference too.
- Scoping is TeX's. A declaration colours the rest of its group, so
  `\color{red} x + y` is red throughout, while `{\color{red} x} + y` colours
  only `x`. `\textcolor{c}{…}` is the group `{\color{c}…}`. Colour never moves
  anything.
- `tex_to_color_map` and `set_color_by_tex` apply on top of the source's
  colours. In the portal, constructor colour keywords (`color=`,
  `fill_color=`) are manim's `init_colors` and colour the whole family, as in
  the Reference. Leave them out to keep the source's colours. The native
  `Tex::style` is the base beneath `\color`.

Migration: a Reference scene that writes `\color` and expects white keeps
white only if it passes a constructor colour. No scene in the pinned
3b1b/videos corpus uses either command.

Reference source at the pinned revision:
[string selectors](https://github.com/3b1b/manim/blob/6199a00d4c1b1127ebe45cb629c3f22538b10e13/manimlib/mobject/svg/string_mobject.py),
[TeX counting](https://github.com/3b1b/manim/blob/6199a00d4c1b1127ebe45cb629c3f22538b10e13/manimlib/mobject/svg/tex_mobject.py),
[matching blocks and Tex alias](https://github.com/3b1b/manim/blob/6199a00d4c1b1127ebe45cb629c3f22538b10e13/manimlib/animation/transform_matching_parts.py).
