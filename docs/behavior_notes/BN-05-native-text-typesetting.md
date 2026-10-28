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

## TeX constructs beyond the 3b1b corpus

fmd-math's first vocabulary was ranked by 3b1b's own videos. These constructs from the
Reference's default preamble (amsmath, amssymb, mathrsfs) are set natively too. They differ
from the Reference in the ways below.

- **amssymb's symbols are the AMS msam/msbm glyphs the Reference draws.** Every amssymb name
  fmd-math knows (`\leqslant`, `\therefore`, `\square`, `\nleq`, `\lneq`, `\Lsh`,
  `\curvearrowleft` …) takes the family and code amssymb.sty declares, its spacing class and
  the TFM box (fm-0byq).
  - The Reference's template loads wasysym after amssymb. There `\lhd`, `\rhd`, `\unlhd`,
    `\unrhd`, `\mho`, `\sqsubset`, `\sqsupset`, `\Box` and `\leadsto` are wasy10's
    characters, in the same classes (TeX's `\meaning` under the template's preamble).
    fmd-math has no wasy10 face, the gap behind C-41's `\iint`, so it sets amssymb's msam and
    msbm forms of them. Under the template's full preamble, every other construct here keeps
    the box TeX gives it under amsmath and amssymb alone.
  - `\nleqslant`, `\ngeqslant`, `\nleqq`, `\ngeqq`, `\nsubseteqq` and `\nsupseteqq` have no
    single Unicode character to carry their meaning, and remain an untiered error.
- **`\mathscr` is Noto Sans Math's script.** The Reference uses mathrsfs's rsfs10. It stays
  distinct from `\mathcal`, which is cmsy's calligraphic in both. Lowercase `\mathscr`
  letters typeset here; rsfs10 has none.
- **Text font switches set one face each.** `\textit`, `\textrm`, `\textup`, `\textnormal`,
  `\texttt` and `\textsf` work in `Tex` and `TexText`.
  - Nested switches do not compose: `\textbf{\textit{x}}` is italic, not bold italic.
  - `\textsf` is IBM Plex Sans, not cmss.
- **`\hspace` reads `em` and `ex` from cmr10.** The Reference's text font is T1's ecrm, whose
  quad (9.99755 pt) is 0.025% narrower. Fixed lengths (`1cm`, `6pt`) are exact.
- `\not`, `\lvert`/`\rvert`, `\boxed`, `\cfrac` and `\genfrac` match TeX's boxes.
- `gather*`, `gathered` and `equation*` set display-style rows, centred.
- `\TeX` and `\LaTeX` are the kernel's constructions in cmr10 and cmr7, and `\mathchoice`
  sets the branch for the style in force. The logos follow Computer Modern's metrics; the
  Reference's T1 ecrm differs from them by up to 6e-4 em.
- `\mathfrak` is the Reference's own Euler Fraktur (eufm10/7/5, ADR-0028's recipe), with
  its TFM boxes.
- A script after `\mathchoice` or `\cancel` sits on an empty base, as in TeX: `\cancel` is
  a `\mathpalette`, so `\cancel{x}^2` raises the 2 at the plain superscript height, not
  above the struck box.
- Display `align*` (every `Tex` string's surface, and `TexText`'s `align*`) spreads its
  column pairs across the page's 345 pt `\linewidth`, as amsmath does. Every gap, the
  margins included, gets the same share, at least 10 pt. `aligned` keeps 10 pt between its
  pairs. Earlier pins set every pair 10 pt apart.
- **`\cancel`, `\bcancel` and `\xcancel` go beyond the Reference.** Its template does not
  load the cancel package, so there these strings are a LaTeX error. Here they are drawn
  as cancel.sty draws them: line-font slopes, LaTeX's `\@sline` tiling and a 0.4 pt
  round pen, with matching boxes. One deliberate difference: a stroke shorter than one
  line-font character, which LaTeX omits with a warning, is drawn.
- **Refused by name until they land**, with an error naming franken_manim
  fm-tex-numbered-displays-bxl9:
  - `\tag`, `\sideset` and `\cancelto`;
  - the numbered `equation`, `gather` and `align`, and `multline`: their numbers and flush
    lines sit against `\linewidth`. Numbered `align` used to be set without its numbers;
    it now refuses with the others.

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

## Math spacing: `\!`, `\;` and `\:` (Appendix C-41)

The Reference's default template loads `tipa` for IPA symbols. tipa redefines
`\!`, `\;` and `\:` as phonetic diacritics. So in the Reference's math these
commands set no space and take the next token as their argument:

- `7 \! 4` is 1.0 em wide, where LaTeX's negative thin space sets 0.8333.
- `3 \; x` is 1.0715 em, where LaTeX's thick space sets 1.3493.
- `3 \; \bigcup_{m=4}^{n} x` loses the operator's limits, which the swallowed
  `\bigcup` turns into ordinary scripts.

FrankenManim sets LaTeX's math spacing: `\!` is −3 mu, `\:` 4 mu, `\;` 5 mu,
and the next token is untouched. That is what the authors of the 25 corpus
strings with `\!` and the 12 with `\;` wrote. The math box census measures
LaTeX without tipa for the same reason (`ruled_math_preamble` in
scripts/capture_tex_corpus_boxes.py). Text mode keeps the Reference's
preamble.

Migration: a scene whose look depends on the Reference's missing space can
delete the command. `\negthinspace`, `\medspace` and `\thickspace` were never
affected and behave the same in both.

Measured with TeX Live 2025, 2026-10-28, by the grammar differential
(fm-tex-grammar-differential-r12-i0nh). Recapturing the math census without
tipa changed exactly the 22 rows whose strings use these commands.

## TexText paragraphs and line breaking

The Reference sets `TexText` as a `\centering` paragraph in a `standalone[preview]`
page whose `\linewidth` is 345 pt, so TeX breaks a string wider than that into
centred lines. FrankenManim breaks it the same way (franken_markdown 61304ccc through 450111a4,
fm-texttext-line-breaking-r11-bfdx):

- **The breaker is TeX's** (tex.web §813–890), with the Reference document's parameters
  read from TeX: `\linepenalty` 10, `\adjdemerits` 10000, `\pretolerance` 100, and the class's
  `\sloppy` (`\tolerance` 9999, `\emergencystretch` 3 em). Every ragged line has badness 0,
  so TeX's tie rule fills each line. A line shrinks its interword glue, rather than
  breaking, where TeX would. A word wider than the line is set alone and overfull.
- **Interword glue is TeX's**: cmr10's space, stretch and shrink, sized by the space factor
  under LaTeX's `\nonfrenchspacing`. After `.`, `?`, `!` and `:` the space is 0.111 em wider,
  unless a capital precedes the period (`U.S. Army`). This holds on one line too: `Yes. No`
  is as wide as TeX sets it.
- **The block's width is its widest line**, not 345 pt. Lines centre on a common axis, as in
  the Reference. Since the Reference's mobject is the ink of its glyphs, the positions agree.
- **Line pitch follows the size declaration**, as LaTeX's `size10.clo` sets it: `\Large` lines
  are 18 pt apart, not 1.2 × 14.4 pt. In `center`, `flushleft` and `flushright`, a size or
  colour declaration carries over `\\`, which is LaTeX's `\par` there.
- **A blank line ends the paragraph**, as TeX's `\par` does, so `First⏎⏎second` is two
  lines. Repeated blank lines and a final one add no empty line. Inside `\text{…}` a blank
  line stays a space.
- **`\\` drops the glue before it**: a trailing `\quad` or `\hspace` too, not only a space.
  A centred line therefore carries no invisible offset.

Not modelled:
- hyphenation, and breaks after explicit hyphens;
- breaks inside a math island, where `\relpenalty` and `\binoppenalty` would add at least
  250000 demerits;
- breaks inside a font-switch group (`\emph{…}`);
- microtype's character protrusion. It moves a line by up to about 1 pt and changes no
  break in the corpus.

`\Large` and `\huge` paragraphs can break differently. The Reference's T1 fonts have
optical sizes (ecrm1440 is about 5% narrower than cmr10 scaled to 14.4 pt), and fmd-math
scales cmr10.

Verification: fmd-math's placement tests compare break positions, line widths, centring and
pitch with TeX's `\showbox`, within 1e-4 em. Over the 3b1b corpus, the paragraph census
(`tex_text_paragraphs_census.v1.tsv`) compares every multi-line or wrapping text-mode
string's widest line and total height with real TeX's.

Migration: none. A long `TexText` that the Reference breaks now breaks the same way, where it
used to be one line up to 185 em wide.
