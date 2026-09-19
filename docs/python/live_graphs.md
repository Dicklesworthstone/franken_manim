# Live function graphs

`axes.bind_graph_to_func(graph, func, jagged=False, get_discontinuities=None)`
updates a VMobject graph on the existing scene updater clock. Direct binding
accepts a vectorized callable: it receives one detached 1-D array of x samples
and returns either one real value per sample or a scalar constant. Binding
installs the updater without evaluating the function; the first ordinary
updater tick computes the new geometry.

```python
pole = ValueTracker(0)
graph = axes.get_graph(lambda x: x)  # Establish the full sampling interval.
axes.bind_graph_to_func(
    graph,
    lambda xs: 1 / (xs - pole.get_value()),
    get_discontinuities=lambda: [pole.get_value()],
)
self.add(graph)
self.play(pole.animate.set_value(1))
```

## Behavior correction: dynamic discontinuities

The pinned Reference and earlier portal binding insert discontinuity neighbors
into a fixed-length array and discard its rightmost samples. Repeated updates
therefore shrink the represented domain and draw connecting segments across
breaks. That behavior is deliberately corrected, not reproduced.

A binding freezes its baseline x samples once. Each update subtracts the
current discontinuity exclusion bands, adds their boundary samples without
truncation, and builds independent native subpaths. Exclusion half-width is
`max(graph.epsilon, 1e-6)`, or `1e-6` when absent. Overlapping bands merge; bands
can touch the endpoints or hide the entire domain, which can recover on a
later tick. Smoothing runs separately on each surviving segment. It cannot
smooth across a discontinuity.

The function and public coordinate conversions run before native corner-path
construction, smoothing and joining. Only then does `graph.match_points`
replace geometry. Callable, shape, nonfinite-value and native construction
errors preserve the last complete graph. A failure in an authored override of
the final `match_points` operation retains that override's normal partial-effect
semantics. The adapter does not roll back arbitrary external callback effects.

The current axes' public `p2c`/`c2p` methods define coordinates. Moving or
rotating the axes does not silently turn them into fixed original pixel
coordinates. The graph keeps its proxy, styles, uniforms, children, and author
updaters; only its owned path changes. Vertex styles follow the existing native
order-preserving resize policy when the number of samples changes.

Rebinding replaces only the previously installed graph callback, preserving
other updaters and the baseline sample interval. Explicitly call
`axes.unbind_graph_from_func(graph)` to stop following the function without
changing the last geometry. Copied graph updaters retain their callable/axes
closure dependencies, but receive and mutate the copied graph, not the original.

At most 65,536 evaluation samples and 1,024 declared discontinuities are
admitted per update. Nonfinite output is an error, not an implicit clipping or
silent hole; declare singularities explicitly. Generated coordinates must fit
finite f32 records. Native geometry, smoothing, buffer generations and rendering
remain the authorities. This is not a new renderer or certification claim.
