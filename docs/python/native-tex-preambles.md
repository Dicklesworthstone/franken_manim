# Native TeX preambles and template packs

`Tex`, `TexText` and the legacy `SingleStringTex` accept native macro definitions
in `additional_preamble`. Expansion uses fmd-math's existing token parser,
non-recursive macro system and budgets. No LaTeX process or Python typesetter is
introduced.

```python
from manimlib import *

preamble = r"\newcommand{\sq}[1]{#1^2}\newcommand{\RR}{\mathbb{R}}"
formula = Tex(r"\sq{x} + 1", additional_preamble=preamble, isolate=["x"])
label = TexText(r"area $\sq{x}$ in $\RR$", additional_preamble=preamble)
```

The `template` names `default`, `basic`, and `empty` select the existing native
preamble packs. The empty template name retains the default. Native primitives
are available in every pack; `default` additionally defines `\minus`. Inline
`\newcommand` refuses to shadow an existing definition, while `\renewcommand`
requires one. Calls can be nested in the expression, such as `\half{\sq{x}}`;
recursion is refused, as is expansion that exceeds fmd-math's token/depth limits.
The native symbol vocabulary includes `\minus` even in the bare packs; the
default pack additionally defines it as a macro, affecting shadowing rules.

**Pinned-engine limitation:** a macro calling another macro from its definition
body can produce invalid spans or fail to substitute forwarded arguments in the
pinned fmd-math version. Those preamble requests are rejected with an explicit
span-provenance error, not rendered with clamped positions or literal `#1`
tokens. Use source-nested calls or define the compound body directly from native
primitives. The upstream correction is tracked in `UPSTREAM_LEDGER.md` row 13;
this change does not silently replace the governed dependency pin.

A preamble must parse independently without producing ink or layout spacing.
It is not a full LaTeX document preamble: package loading, file inclusion,
shell execution, host-font templates, and unsupported constructs remain named
errors. Known unavailable and unknown templates list the available native packs.
A trailing comment cannot consume the expression because the native boundary
inserts a newline before the original formula.

## Reference template compatibility

The registry classifies all 58 templates from the pinned Reference. The three
native packs implement the native construct vocabulary, not complete LaTeX
packages; unsupported constructs remain named errors. All three retain the
standalone document's 345 pt paragraph width. `default` additionally selects
physics function arguments, disables text ligatures and defines `\minus`;
`basic` and `empty` keep ordinary text ligatures and no extra macro definitions.

Native grammar supplies the supported amsmath/amssymb operations, colors,
text islands, size/style commands, font-independent spacing and selected
symbols from the other default packages. `dsfont`, `mathrsfs`, `calligra`,
`wasysym`, `tipa`, `relsize` and the other package names are not promises of
complete package support. The coverage ratchet and named construct errors
describe the remaining vocabulary. Font selection, CJK shaping and a host
compiler never silently fall back to another look.

The partial classification has concrete limits. None of these packs executes
LaTeX packages, selects encodings with `fontenc`, implements Babel language
rules or automatic word hyphenation, or reproduces microtype's font expansion
and margin protrusion. `default` maps selected commands from its larger
package list; it does not load those packages. `basic` maps native math and
color operations without promising the entire amsmath/amssymb vocabulary.
`empty` adds no definitions but still exposes the shared native grammar,
including constructs beyond an empty LaTeX preamble.

The table is generated from the same classifications used by template
resolution. Regenerate its block with
`cargo test -p fmn-config template_table_in_user_docs -- --nocapture`.
The test also fails if this documented block differs from the runtime registry.

<!-- BEGIN TEMPLATE COMPATIBILITY -->
| Reference template | Classification | Native pack or unavailable contract |
|---|---|---|
| <a id="template-default"></a>`default` | partial native pack | fmd-math/pack/default |
| <a id="template-basic"></a>`basic` | partial native pack | fmd-math/pack/basic |
| <a id="template-empty"></a>`empty` | partial native pack | fmd-math/pack/empty |
| <a id="template-ctex"></a>`ctex` | refused by name | it selects CJK typesetting, which is outside the current typography tier (complex scripts are tiered out with a revisit trigger; see the Behavior Notes) |
| <a id="template-basic_ctex"></a>`basic_ctex` | refused by name | it selects CJK typesetting, which is outside the current typography tier (complex scripts are tiered out with a revisit trigger; see the Behavior Notes) |
| <a id="template-empty_ctex"></a>`empty_ctex` | refused by name | it requests XeLaTeX's Unicode/font engine; its empty preamble supplies no native font mapping |
| <a id="template-american_typewriter"></a>`american_typewriter` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-antykwa"></a>`antykwa` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-apple_chancery"></a>`apple_chancery` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-auriocus_kalligraphicus"></a>`auriocus_kalligraphicus` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-baskervald_adf_fourier"></a>`baskervald_adf_fourier` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-baskerville_it"></a>`baskerville_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-biolinum"></a>`biolinum` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-brushscriptx"></a>`brushscriptx` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-chalkboard_se"></a>`chalkboard_se` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-chalkduster"></a>`chalkduster` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-comfortaa"></a>`comfortaa` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-comic_sans"></a>`comic_sans` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-droid_sans"></a>`droid_sans` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-droid_sans_it"></a>`droid_sans_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-droid_serif"></a>`droid_serif` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-droid_serif_px_it"></a>`droid_serif_px_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-ecf_augie"></a>`ecf_augie` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-ecf_jd"></a>`ecf_jd` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-ecf_skeetch"></a>`ecf_skeetch` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-ecf_tall_paul"></a>`ecf_tall_paul` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-ecf_webster"></a>`ecf_webster` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-electrum_adf"></a>`electrum_adf` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-epigrafica"></a>`epigrafica` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-fourier_utopia"></a>`fourier_utopia` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-french_cursive"></a>`french_cursive` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-gfs_bodoni"></a>`gfs_bodoni` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-gfs_didot"></a>`gfs_didot` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-gfs_neohellenic"></a>`gfs_neohellenic` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-gnu_freesans_tx"></a>`gnu_freesans_tx` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-gnu_freeserif_freesans"></a>`gnu_freeserif_freesans` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-helvetica_fourier_it"></a>`helvetica_fourier_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-latin_modern_tw"></a>`latin_modern_tw` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-latin_modern_tw_it"></a>`latin_modern_tw_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-libertine"></a>`libertine` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-libris_adf_fourier"></a>`libris_adf_fourier` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-minion_pro_myriad_pro"></a>`minion_pro_myriad_pro` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-minion_pro_tx"></a>`minion_pro_tx` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-new_century_schoolbook"></a>`new_century_schoolbook` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-new_century_schoolbook_px"></a>`new_century_schoolbook_px` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-noteworthy_light"></a>`noteworthy_light` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-palatino"></a>`palatino` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-papyrus"></a>`papyrus` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-romande_adf_fourier_it"></a>`romande_adf_fourier_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-slitex"></a>`slitex` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-times_fourier_it"></a>`times_fourier_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-urw_avant_garde"></a>`urw_avant_garde` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-urw_zapf_chancery"></a>`urw_zapf_chancery` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-venturis_adf_fourier_it"></a>`venturis_adf_fourier_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-verdana_it"></a>`verdana_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-vollkorn"></a>`vollkorn` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-vollkorn_fourier_it"></a>`vollkorn_fourier_it` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
| <a id="template-zapf_chancery"></a>`zapf_chancery` | refused by name | it selects a font family or font-specific math package outside the bundled faces (sometimes a host system font); FrankenManim never substitutes a different native face |
<!-- END TEMPLATE COMPATIBILITY -->

## Source identity and caching

`get_tex()`, selectors, isolation, coloring, matching animations and live numeric
replacement continue to address the **original expression**. Macro-generated
ink belongs to its call site; argument glyphs retain their argument spans.
Preamble bytes never become selectable formula characters. Formula diagnostics
report offsets relative to that expression. Preamble failures are explicitly
labelled `additional_preamble`. Selection uses containment: select `\half{x}`
to include whole-call generated ink, or `x` for its literal argument. The bare
command name `\half` alone does not contain the full expansion-site span.
Primitive commands differ. Ink that a built-in command draws itself (the rule
of `\frac`/`\over`, the radical of `\sqrt`, accents, and over/under lines and
braces) belongs to the command token. So `tex[R"\over"]` selects the fraction
bar and `tex[R"\sqrt"]` the radical sign, never argument glyphs, as in the
Reference (fmn-tex `KEYWORD_INK_COMMANDS`).

A single-use numeric macro argument can become a live readout without removing
generated ink around it. When one argument is expanded into multiple displayed
copies, `make_number_changeable` refuses rather than collapsing those copies
into one number. Author separate numeric source occurrences when each copy
must have a live readout. Ordinary selection and rendering of repeated macro
arguments remain supported.

Cache lookup includes the complete effective source and the native pack's
fingerprint. Different definitions cannot reuse each other's expansion, and
request-local definitions never leak into another object. Copying and pickling
retain the Python configuration and the formula's projected span map.

Additional preambles are limited to 65,536 UTF-8 bytes; preamble, separator and
formula together are limited to 262,144 bytes. Empty preambles retain the
existing ordinary source path. Native expansion limits still apply within
these input bounds. Invalid preambles and templates fail before the Python
constructor installs live object state; failed render generations publish no
output.

## Rust and acceptance

`TexEngine::typeset_with_preamble(mode, source, preamble)` returns native typeset
data with original-source coordinates. Library callers use
`Tex::new(source).preamble(preamble).build(&engine)` or the same builder method on
`TexText`. Engine selection remains explicit for Rust callers.

`crates/fmn-tex/tests/preamble.rs` and
`crates/fmn-library/tests/tex_preamble.rs` cover native scopes, caches, diagnostics,
geometry and source provenance. `crates/fmn-python/tests/tex_preamble.py` covers
real portal construction and selection. The registered
`render_matrix.python_tex_preamble.v1` Gauntlet scenario exercises an eight-frame
native render at 1/4/16 threads, matching transforms, live numeric replacement
and rejection of a recursive-macro generation.
