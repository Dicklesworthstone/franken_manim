# Native guides on moving coordinate systems

Bind a built chart to the Stage family that displays it. Query the binding
again after movement, or take one `CoordinateFrame` snapshot to construct
several objects in exactly the same frame. The detached `Axes` builder is
not modified when its registered family moves.

```rust
use fmn_library::{Axes, FontBook};
use fmn_mobject::Stage;

let axes = Axes::new().build(&FontBook::bundled()?)?;
let mut stage = Stage::new();
let root = stage.add(axes.clone());
stage.add_to_scene(root)?;
let live = axes.bind(&stage, root)?;
stage.shift(root, [-2.0, 0.5, 0.0]);
let frame = live.snapshot(&stage)?;
let guide = frame.get_v_line_to_graph(1.0, &|x| x * x)?;
let guide_root = stage.add(guide);
stage.add_to_scene(guide_root)?;
```

The snippet belongs inside a function returning an error-compatible `Result`.
The same binding mechanism works for `NumberPlane`, `ComplexPlane`, and
`ThreeDAxes`. A later snapshot sees affine animation, including shear,
reflection, nonuniform scale and out-of-plane rotation. Existing snapshots
remain immutable, even after their original axes are removed or deleted.

## Projection and graph queries

`project_to_axis(index, point)` returns the closest world-space point on an
infinite numeric axis. `get_line_from_axis_to_point` draws a native dashed
line from that projection to the supplied point. The x-axis and y-axis
conveniences are `get_v_line` and `get_h_line`; their graph variants take
`(x, &function)`. Live methods additionally take `&Stage` first.

Projection is **Euclidean**, matching the detached NumberLine/Axis guide
contract. On sheared axes, it is not equivalent to keeping one numeric
coordinate and setting the others to zero. The latter construction remains
available explicitly through `c2p`. A 3D frame permits axis indices 0, 1,
and 2 and permits points outside the chosen axis or chart plane. Projections
are not clipped to the drawn shaft's minimum and maximum.

`line_from_axis_to_point_styled` accepts the normal `Style` value. Default
guides use `GREY_A`, stroke width 2, and the existing DashedLine spacing.
The chart's sampling budget bounds the dash point allocation; the native
DashedLine limits remain additional hard bounds. Invalid styles, indices,
non-finite inputs, or points that cannot fit finite f32 records are named
errors, not empty or partially constructed guides.

`CoordinateFrame::try_input_to_graph_point` is the checked graph-point
query. The original infallible `CoordinateFrame::input_to_graph_point`
remains unchanged for compatibility. The live binding exposes checked
`input_to_graph_point` and `i2gp`. Each successful query invokes the supplied
function once, with numeric z = 0 in a 3D chart. Invalid live axes, invalid x,
or an insufficient minimum output budget are rejected before a graph
callback executes. Callbacks are not purity declarations and their external
effects cannot be rolled back.

Queries do not mutate scene membership, geometry, updaters, clocks, or views.
An updater may take a fresh frame and rebuild its own guide geometry using
the ordinary Stage mechanisms; no second graph update loop is installed.

Tests: `fmn-library/tests/live_guides.rs` uses affine and Euclidean geometric
oracles, extreme unit scales, frozen snapshots and admission negatives.
`fmn/tests/live_guides.rs` compares production PNG output with independently
constructed sheared guides at 1, 4, and 16 render threads.
