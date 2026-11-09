# Live native bar charts

`BarChart::add_to` installs a chart in a native arena and returns a
`BarChartBinding`. The binding retains the chart, axis, and bar identities for
subsequent value updates and animations. `BarChartBinding` and
`ChangeBarValues` are available from `fmn::prelude`.

The existing detached `BarChart::build` API remains available. Positive-value
construction retains its existing geometry. Both construction paths accept
finite signed values, including all-zero data. With `max_value(None)`, the
scale is the largest absolute value, or one for an all-zero series.

## Authoring a live chart

```rust
use fmn::prelude::*;

struct Results;

impl SceneConstruct for Results {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let book = FontBook::bundled()
            .map_err(fmn::library::TextMobjectError::Text)?;
        let mut chart = BarChart::new(vec![0.25, 0.0, -0.5])
            .height(2.0)
            .width(4.0)
            .bar_names(vec!["Alpha".into(), "Beta".into(), "Gamma".into()])
            .add_to(stage.arena_mut(), &book)?;
        stage.scene_mut().add(&[chart.mobject()])?;

        let animation = ChangeBarValues::new(
            stage.arena(), &chart, &[0.75, -0.5, 0.0],
        )?.with_config(AnimConfig {
            run_time: 1.0,
            rate_func: RateFunc::linear(),
            ..AnimConfig::default()
        });
        stage.play(animation)?;

        // A shorter slice updates only that prefix of the chart.
        chart.change_bar_values(stage.arena_mut(), &[0.0, 0.5])?;
        stage.wait(0.5)?;
        Ok(())
    }
}
```

`add_to` allocates the family without putting it on the scene's draw list,
matching the native graph binding API. Add `chart.mobject()` explicitly.
The binding exposes its bars and y-axis for normal styling and inspection.

The runnable example renders two signed transitions and a prefix update to a
native PNG sequence:

```sh
cargo run --locked -p fmn --example live_bar_chart -- live-bars
```

The destination must be new, following the ordinary render publication
contract. The same scene can be sent to the existing GIF, Y4M, SVG, or FMTL
export APIs.

## Geometry and identity

An update reads the current y-axis vector and the current baseline edge of
each selected bar. It builds each rectangular target from those two vectors.
It never divides by the bar's previous height, so a bar can collapse exactly
to zero, grow again, or cross its baseline into negative values. Rotation,
translation, reflection, nonuniform scaling, and shear follow the live chart
geometry.

Bar handles, children, styles, and updaters remain attached to the same
objects. Fixed-size writes preserve existing record views. Axes and category
labels remain independent annotations; changing data does not reposition them
or silently rescale the value axis. The original `max_value` remains the
chart's value-to-height scale.

`BarChartBinding::from_handles` provides the checked entry point for a chart
that another authoring front end has already installed. It checks the supplied
arena handles, family membership, record layout, and live value axis. Each
update then validates the selected bars' geometry before writing. Adopting a
binding leaves object ownership and scene membership with the caller.

## Animation lifecycle

`ChangeBarValues` uses Choreo's existing `Transform` implementation. It retains
the requested numbers and prepares its target when the animation begins, so
queued animations observe changes made by preceding animations. It keeps the
ordinary timing, rate, suspension, finish, cleanup, and failure rules.

After preparation, its closed native interpolation can use the same pure
frame jobs as other supported transforms. Updaters, live views, aliases, and
other effects continue to control admission through the existing classifier.
No new animation loop or callback-purity inference is introduced.

## Admission and failures

Charts admit at most 4,096 bars and 4,096 value-axis tick intervals.
Values must be finite; height, width, and
`max_value` must be finite and positive; generated coordinates must fit finite
f32 records. A replacement slice may be shorter than the bar count but cannot
be longer. An empty slice changes no records but still validates the live
binding and value axis.

Updates validate their handles, rectangular point topology, schema, and all
candidate coordinates before publishing the first bar. A stale bar, a
malformed later bar, or an overflowing later value therefore cannot leave an
earlier bar partially updated. Arbitrary changes to the bar's point topology
require constructing a compatible chart binding again.

## Validation

The native library tests cover signed and all-zero construction, updates
through zero, affine chart geometry, stable styles/handles/views, atomic
refusals, and ordinary animation lifecycle. The existing positive chart
geometry golden remains an independent compatibility check.

`crates/fmn/tests/native_bar_chart.rs` executes the public example through
serial captures, reversed worker jobs, and native PNG publication at one,
four, and sixteen threads. The Gauntlet registers the same runnable example
as `lifecycle.live_bar_chart.v1`, including ordered publication and visible
zero/sign changes.

```sh
cargo test --locked -p fmn-library --test data_mobjects
cargo test --locked -p fmn --test native_bar_chart
cargo test --locked -p fmn-conformance --test e2e_scenarios live_bar_chart_scenario_passes
```

These are validation entry points. The execution report for a change must
separately state which commands ran and which were blocked.
