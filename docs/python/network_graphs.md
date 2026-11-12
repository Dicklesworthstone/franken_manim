# Native network graphs

`fmn_python.network_graph.NetworkGraph` connects the enhanced Atlas network
library to Python scenes. It does not require a Python `networkx` installation.
Native Rust owns circular, shell, breadth-first and seeded spring layouts,
graph traversal order and weighted flow schedules. Existing `Dot`, `Line`,
`Circle`, `Text` and animation kernels own drawing, typography and rendering.
This is an enhanced API, not a claim that
a `Graph` class existed in the pinned upstream `manimlib` surface.

```python
from manimlib import *
from fmn_python.network_graph import NetworkGraph

class ExplainNetwork(Scene):
    def construct(self):
        graph = NetworkGraph(
            ['a', 'b', 'c', 'isolated'], [('a', 'b'), ('b', 'c')],
            layout='circular', labels=True,
        )
        self.add(graph)
        self.play(graph.animate_layout(
            'breadth_first', layout_config={'root': 'a'}, path_arc=PI/3,
        ), run_time=2)
        self.play(graph.vertices['b'].animate.shift(UP))
        graph.add_edges(('c', 'new'), positions={'new': (2, -1, 0)})
        self.play(graph.animate_layout('spring', layout_config={'seed': 7}))
        self.play(graph.animate_traversal('a', order='depth_first', color=YELLOW))
        self.play(graph.animate_flow([
            ('a', 'b', 1), ('b', 'c', 3), ('c', 'new', 1),
        ], run_time_frames=60))
```

## Data and ownership

Labels are strings. Explicit vertices establish insertion order; edge-only
endpoints are appended on first appearance. Repeated vertices and repeated
undirected edges coalesce. Reversed duplicate edges refer to the same visual;
`get_edge(a, b)` accepts either orientation. Self-loops draw as native circles,
not zero-length lines. The graph paints edges beneath vertices, then optional
plain-text labels. `labels=True` uses node names; a string-valued mapping labels
a subset. Configure shared styles with `vertex_config`, `edge_config` and
`label_config`; the ordinary per-object setters remain available afterward.

`vertices`, `edges` and `labels` return new mappings to the actual owned native
objects. Editing a returned mapping does not change topology. Native copying,
deepcopy, pickle and scene checkpoints preserve the graph's internal ownership:
updaters operate on the copied graph rather than closing over the original.
`get_layout()` returns detached current world-coordinate arrays.

A graph updater reconnects edges and centers labels after vertex movement.
`refresh_edges()` performs the same operation explicitly for detached graphs.
Straight edges connect vertex centers; vertex fill covers the portions underneath
the dot. Independently curved/buffered edges and custom arrow tips are not part
of this API. Ordinary edge and vertex styling and attached annotations survive
refreshes. Directly changing the owned groups' child lists is unsupported.

## Layouts and motion

`change_layout(name, **options)` changes the layout immediately.
`animate_layout(name, layout_config={...}, **animation_options)` returns a normal
`Transform` subclass. It freezes the destination once, retains the shared frame
clock, easing, path functions and composition lifecycle, and reconnects edges
and labels after each interpolation. Use this form for curved trajectories;
ordinary `.animate.change_layout(...)` also supports straight interpolation.
`set_vertex_positions(mapping)` updates a subset, validating all supplied
coordinates first. Coincident vertices can separate again without rebuilding
the graph. Whole-graph affine transforms retain attached connectors and loops.

A custom layout is a complete mapping from vertex labels to two- or
three-component positions. `layout_scale` (default 2) and `center` (default the
origin) apply to the new layout's coordinates. These are world-coordinate
targets; a previous shift, rotation or scale of the graph is not implicitly
reapplied to a later layout. A layout animation currently requires the same
ordered topology and label membership at both endpoints.

## Traversal and edge flow

`graph.animate_traversal(root, order='breadth_first', color=YELLOW)` returns a
`TraverseGraph` animation. Import that class directly from
`fmn_python.network_graph` to subclass its ordinary animation lifecycle. Both
`breadth_first` and `depth_first` use the same native traversal planner as Rust
`TraverseGraph`: neighbors follow edge insertion order, each reachable vertex
is visited once, and disconnected vertices are left alone. An isolated root is
visited once. This traversal order differs deliberately from the layout's
sorted-neighbor placement rule. `animation.visit_order` exposes the scheduled
labels. Each vertex keeps its highlight after its interval; edges, labels and
unvisited vertices keep their styles. Moving or styling the graph between
animation construction and playback is supported.

`graph.animate_flow(steps, color=YELLOW, stroke_width=5, time_width=.25)` returns
`FlowAlongEdges`. Each step is `(source, destination, positive_weight)` for an
existing edge. The source and destination set pulse direction independently
of the undirected edge's stored orientation. Repeated visits and self-loops
are supported. Weights `1` and `3` receive one quarter and three quarters of
the total time; `animation.durations` exposes the native normalized durations.
Normalization supports large finite weights whose raw sum would overflow,
and rejects weights too small to occupy a distinct timeline interval.

Pulses use the existing native `ShowPassingFlash` and `VFadeIn` kernels.
`time_width` controls the passing segment's width in normalized edge progress;
opacity falls to zero at both ends of each visit. The temporary
`animation.pulses` overlay freezes live edge paths and rendering uniforms at
begin. Later edge motion does not retarget a running pulse. Cleanup removes
only this overlay and restores the original graph pixels; the source graph's
geometry, styles, ownership and attached annotations are never modified.
An aborted callback also releases the overlay and shared animation state.

Both animations default to two seconds and support the ordinary `run_time`,
`run_time_frames`, easing and composition controls. `Scene.play` overrides and
nested groups use the shared native timing machinery. Define custom `begin`,
`interpolate`, `finish` or cleanup hooks by subclassing and calling `super()`.
Topology is checked again at begin: construct a new animation after editing
connectivity. Flow is bounded to 8,192 steps and shares the bridge's label-data
budget. Invalid roots, absent edges and nonpositive/nonfinite weights fail
explicitly before changing the graph.

## Changing connectivity

`set_graph(vertices, edges, positions=...)` replaces connectivity without
replacing surviving vertex, edge or label objects. Reversing an edge's spelling
retains that edge's identity. New endpoints are inferred just as at construction;
new vertices start at the origin unless `positions` supplies world coordinates.
Existing vertices keep their current positions unless explicitly repositioned.
`add_edges(*edges, positions=...)`, `remove_edges(*edges)` and
`remove_vertices(*names)` provide incremental edits. Removing a vertex also
removes its labels and incident edges. Removing an edge retains its vertices.
External references to removed visuals remain ordinary usable mobjects.

All input validation and new visual construction precede publication. Invalid
requests leave the existing graph intact. Reentrant topology edits and edits to
an actively animated graph family refuse; edits from a graph/scene updater while
another object drives the clock are supported. These are discrete changes, not
a topology-morph algorithm: `.animate.set_graph(...)` and the incremental edit
methods explicitly refuse rather than silently misalign graph catalogs.

The constructor's style dictionaries and label policy govern newly added
objects. Styles and annotations on surviving members are preserved. Copying and
scene checkpoints carry both catalogs and visual groups, so later edits of a
copy never mutate the original graph's connectivity. Use scene checkpoints for
restoring changed topology; ordinary mobject `restore()` retains its existing
same-family-shape requirement. Directly editing private catalogs or replacing
graph-owned groups is outside this contract.

The native layout options are:

| Layout | Options and ordering |
|---|---|
| `circular` | Unit circle in node insertion order. |
| `shell` | `shells=[...]`, outermost first; must partition every vertex exactly once. |
| `breadth_first` | `root='...'`; sorted-neighbor expansion. Unreachable vertices occupy a separate radius-1.5 ring in insertion order. |
| `spring` | `seed=0`, `iterations=50`, `ideal_edge=1`; the native named RNG substream and fixed accumulation order. |

The Python front door bounds each request to 1,024 vertices, 8,192 input edges
and 1 MiB of label data. Host iterables are bounded before eager native
conversion. Spring requests admit 1–1,000 iterations and at most 16,777,216
pair/edge force evaluations; `ideal_edge` must be in `[1e-6, 1e6]`. These bounds
apply to this bridge, not retroactively to every direct Rust graph constructor.
Positions must be finite and f32-representable. Unknown nodes, incomplete custom
layouts and malformed shell partitions fail explicitly rather than silently
omitting graph content.

No second graph database, force solver, renderer or RNG is implemented in
Python. This delivery does not add directed or parallel edges, animated topology
correspondence, weighted spring forces, general graph-algorithm execution or a
certified-layout claim.
`demo/python/network_layouts.py` is a runnable no-assets scene. The native layout
and full scene acceptance suites include independent geometry and Y4M witnesses.
Traversal and flow acceptance also exercise real PNG sequences at one, four
and sixteen rendering threads, assert byte equality and visible intermediate
motion, and verify that flow cleanup restores the initial graph frame. The
installed-wheel script runs these sequences in certified mode with explicit
source bytes; embedded Rust tests use standard mode because they have no
installed extension identity.
