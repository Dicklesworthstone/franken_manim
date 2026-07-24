# BN-05 — Native text typesetting: bundled faces, owned shaping, no Pango

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
- **Line metrics are fixed constants** (1.2 em baseline skip ×
  `line_spacing`, greedy breaking with manim's width semantics;
  least-badness is an explicit option), not Pango's font-derived leading.
- **Markup** is the manim tag set with precise line:column diagnostics —
  unknown tags/attributes are errors, not passthrough.

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
- Pango-only markup (arbitrary attributes, `<gravity>`, etc.) is out of
  the compatibility claim; the supported set is documented in
  `fmn_text::markup`.
