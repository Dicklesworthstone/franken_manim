# Importing editable SVG paint geometry

`SVGMobject` now uses the checked native paint importer for both file inputs and
`svg_string`. The public class, import paths, constructor signature, authored
subclass dispatch, native record/view ownership, and scene clock are unchanged.

```python
from manimlib import *

source = '''<svg>
  <path fill="blue" fill-rule="evenodd"
        d="M0 0H10V10H0Z M2 2H8V8H2Z"/>
  <path fill="none" stroke="yellow" stroke-dasharray="2 1 4 1"
        d="M12 1Q17 12 22 1"/>
</svg>'''
figure = SVGMobject(svg_string=source, width=8, stroke_width=None)
# In a Scene:
# self.play(FadeIn(figure))
# self.play(figure.animate.rotate(.2).fade(.5))
```

`stroke_width=None` retains SVG widths; the familiar constructor default remains
zero. A later `set_stroke(width=...)` reveals the already prepared dash geometry.
The example in `demo/python/imported_svg_paints.py` needs no external assets.

## Embedded stylesheets

User SVGs can also keep their embedded CSS. The shared native document processor
resolves `<style>` rules before building shapes, so the Python portal and native
Rust importer produce the same editable paints:

```python
source = '''<svg width="12" height="8">
  <path class="panel" d="M1 1H7V7H1Z M2 2H6V6H2Z"/>
  <path id="accent" class="hot" style="fill:green;fill-opacity:.5"
        d="M8 1H11V4H8Z"/>
  <style>
    .panel { fill:#4263eb; fill-rule:evenodd }
    #accent.hot { fill:rgba(255,64,32,.5) !important }
  </style>
</svg>'''
figure = SVGMobject(svg_string=source, width=8, stroke_width=None)
```

The panel retains its hole. The accent is orange with a quarter-opacity fill:
its important rule overrides the ordinary inline green, and paint alpha and
`fill-opacity` multiply independently.

Supported selectors are element types (`path`), universal (`*`), classes
(`.panel`), IDs (`#accent`), compounds (`path.panel.hot`, `#accent.hot`), and
comma-separated lists. Identifiers use unescaped ASCII letters, digits,
underscores and hyphens with CSS identifier starting rules. Class names match
complete whitespace-separated words and remain case sensitive. CSS comments,
CDATA sections and predefined/numeric XML character references are accepted.
Comments between selector tokens preserve their token boundaries.

Rules apply in document order, including sheets in `defs` and sheets placed
after the geometry they style. Only matching arms of a selector list contribute
specificity. Presentation attributes precede stylesheet declarations, including
universal rules; inline declarations have greater specificity than ordinary
rules. Importance is considered first, and equal priorities use the last
declaration. The cascade follows the author-style ordering in
[SVG styling](https://www.w3.org/TR/SVG2/styling.html) and the
[CSS cascade](https://www.w3.org/TR/CSS2/cascade.html).

The existing paint/stroke property subset also supports `inherit`, `initial`
and `unset`. Inherited `currentColor` resolves against the child's computed
color, including its alpha. A group with `visibility:hidden` can contain a
child that restores `visibility:visible`; `display:none` suppresses its entire
subtree. A winning opacity is applied once per element before the documented
group-opacity fold. This remains a flattening of group compositing: overlapping
children do not acquire a separate off-screen browser opacity group.

Stylesheets need no retained CSS runtime or new native record fields. The
resolved colors, opacities, widths, fill rules and dash patterns flow into the
same native geometry and animation APIs as presentation attributes. `use`
instances match their original element's type, ID and classes and inherit from
their instance host.

Conditional media, alternate/scoped sheets, external stylesheets and XML
stylesheet processing instructions are refused. The supported media value is
unconditional `all` (or an omitted/empty attribute); the supported type is
`text/css`. At-rules, descendant/child/sibling combinators, attribute selectors,
pseudo selectors, CSS escapes, variables and unsupported sheet properties have
named errors. Clipping, masks, filters, markers and paint servers retain their
named refusals when expressed through CSS as well. Unrelated unknown properties
in legacy inline `style` attributes keep their earlier documented ignored
behavior; embedded rules use the stricter property admission.

## Filled sets and strokes

Before this integration, the document processor accepted `fill-rule="evenodd"`
and dash attributes but the mobject consumer ignored them. Same-direction inner
contours could lose their holes, and dashed paths rendered as solid strokes.

Even-odd filled sets now pass through **Chisel's existing bounded boolean kernel**,
which emits consistently oriented native fill geometry. Nested holes, intersecting
contours, and coincident contour cancellation use that one geometry authority.
Stroke geometry retains the original curves independently: canceling duplicate
filled contours must not erase their authored strokes.

Dash intervals use **Chisel's true arc-length table**, not control-polygon length
or equally weighted curve indices. Odd-length lists repeat to form an even list;
positive and negative offsets select the initial phase; each authored move starts
a new pattern. Zero-length gaps merge adjacent painted intervals, and a dash that
crosses a closed contour's seam is joined rather than acquiring two extra caps.
The all-zero list is solid. Zero-length *painted* dash entries currently produce a
named refusal because native dot-cap support is not yet connected to this importer.

There is still one top-level native child per pointful authored SVG shape, in
original order. Ordinary nonzero, solid shapes retain their original flat path.
An even-odd or dashed shape instead contains a fill layer followed by stroke
layers. Empty or initially hidden roles remain present so later styling cannot
silently restore a solid path or double-wound fill.

The ordinary color, opacity, RGBA-array and style APIs respect these layer roles,
including calls through an enclosing `VGroup` and through animation targets.
Copies, pickle and scene checkpoints preserve the roles. As with ordinary
`VMobject`, `set_opacity` assigns channel alphas, while `fade` scales existing
alphas; use `fade` for relative fading that leaves originally unpainted channels
unpainted. As with other native
mobjects, explicit low-level record edits can alter the underlying geometry and
paint data; role metadata is not a restriction on writable record views.

## Ownership and input boundaries

A filename is read once through the existing bounded native file reader. Authored
`file_name_to_svg_string` overrides still run after normal instance initialization;
rendered geometry uses the exact source stored in `svg_string`/`hash_seed`.

`mobjects_from_svg_string(source)` returns detached native shapes without replacing
the receiver's geometry, even when the receiver is already part of a Scene.
`init_svg_mobject()` prepares the stored source, then adds the built family, as the
Reference does: a second call appends a second family, and the call returns `None`
(Ledger row `same`). It is the raw document-space build operation, not an additional
constructor centering or size-normalization pass. Root identity and callbacks remain owned by the Scene.
Parse, admission and geometry-preparation errors occur before either operation
publishes replacement geometry.

The existing XML processor continues to enforce its byte, nesting, element,
command and expansion limits and to reject DOCTYPE, scripts, external references
and unsupported features by name. Embedded sheets additionally default to 4,096
selector entries (every arm of a comma list counts) and 16,777,216 units of
conservatively charged selector/declaration work. Comparisons account for class
and ID byte lengths and repeated `use` instances; caller-supplied `SvgLimits`
can lower either budget. Hidden shapes skip geometry refinement while still
visiting children that may restore visibility. Paint preparation adds document-wide limits of
1,048,576 native points, 4,096 dash pieces, 131,072 dash traversal steps and the
existing boolean work budgets. Dash extraction reserves its touched-curve output
before allocation, rather than copying a whole densely sampled path per dash.

## Scope and deliberate limits

This is not a browser SVG implementation. Even-odd fills use the existing boolean
flattening tolerance in resolved viewport units; strokes retain their quadratic
curves. Native round caps, joins, stroke-width conversion, lighting and compositing
remain the engine's look, not promises of exact browser rendering. The document
processor's scalar transform rule for widths/dash distances also remains in force:
nonuniformly transformed or sheared SVG dashes are interpreted in the resolved
native viewport metric, not an exact pre-transform SVG stroke metric. Ordinary
mobject transforms after import transform the already prepared geometry.

Gradients, patterns, masks, SVG text and other features outside the existing
parser's accepted subset retain their named refusals. No additional XML parser,
renderer, font engine, numerical solver, dependency, or external tool is added.

For native Rust consumers, use `fmn_library::svg::svg_mobject_with_paints` (bytes)
or `svg_document_with_paints` (an already resolved document), with optional
`SvgPaintOverrides`. Both return typed preparation errors. The older infallible
`svg_document_mobject` and its legacy `svg_mobject` wrapper remain flat-path
adapters for existing internal drawing builders; they are not substitutes for
the checked user-document ingress.
