//! fm-n64 tranche 2: the Data mobjects — CSV→TableMobject end-to-end
//! through the suite's frame parser, BarChart structural fixtures against
//! the Reference's probability.py conventions, numpy-compatible rounding,
//! and bit-locked self-goldens.

use fmn_core::constants::{BLUE, YELLOW};
use fmn_hash::sha256;
use fmn_text::FontBook;

use fmn_library::data_mobjects::{BarChart, TableMobject, format_scalar, numpy_round2};

use fmn_anim::{AnimConfig, Animation, RateFunc, Succession, Transform};
use fmn_core::types::Vec3;
use fmn_library::data_mobjects::{BarChartBinding, ChangeBarValues, MAX_BAR_CHART_BARS};
use fmn_mobject::{Mob, Mobject, Placement, Stage};

fn fail(message: String) -> ! {
    std::panic::panic_any(message)
}

/// Original test CSV (never Reference bytes): column order deliberately
/// unsorted to prove insertion order survives, one null cell, mixed types.
const SAMPLE_CSV: &str = "region,score,active\nwest,3.5,true\neast,,false\nnorth,7,true\n";
fn book() -> FontBook {
    // The bundled default face set; Scribe resolves text against it.
    FontBook::bundled().unwrap_or_else(|error| fail(format!("bundled fonts: {error}")))
}

#[test]
fn csv_column_order_and_cells_survive_the_frame_round_trip() {
    let table =
        TableMobject::from_csv(SAMPLE_CSV, ',').unwrap_or_else(|error| fail(format!("{error}")));
    assert_eq!(table.headers(), ["region", "score", "active"]);
    assert_eq!(table.rows().len(), 3);
    assert_eq!(table.rows()[0], ["west", "3.5", "true"]);
    // The null cell renders empty per the documented rule.
    assert_eq!(table.rows()[1][1], "");
    assert_eq!(table.rows()[1][2], "false");
    assert_eq!(table.rows()[2], ["north", "7", "true"]);
}

#[test]
fn scalar_formatting_follows_the_documented_rules() {
    use fp_types::Scalar;
    assert_eq!(format_scalar(None), "");
    assert_eq!(
        format_scalar(Some(&Scalar::Null(fp_types::NullKind::NaT))),
        ""
    );
    assert_eq!(format_scalar(Some(&Scalar::Bool(true))), "true");
    assert_eq!(format_scalar(Some(&Scalar::Int64(-42))), "-42");
    assert_eq!(format_scalar(Some(&Scalar::Float64(3.5))), "3.5");
    assert_eq!(format_scalar(Some(&Scalar::Float64(-0.0))), "0");
    assert_eq!(format_scalar(Some(&Scalar::Utf8("hello".into()))), "hello");
    assert_eq!(format_scalar(Some(&Scalar::Timedelta64(90_000))), "90000ns");
}

#[test]
fn table_builds_a_ruled_grid_through_scribe() {
    let table =
        TableMobject::from_csv(SAMPLE_CSV, ',').unwrap_or_else(|error| fail(format!("{error}")));
    let family = table
        .build(&book())
        .unwrap_or_else(|error| fail(format!("{error}")));
    // Rules: outline + header rule + 2 row separators + 2 column rules = 6;
    // cells: 3 headers + 9 body = 12. Children: [rules…, cells…].
    assert_eq!(family.children().len(), 6 + 12);
}

#[test]
fn bar_chart_matches_the_reference_geometry() {
    let chart = BarChart::new(vec![0.25, 0.5, 1.0])
        .bar_names(vec!["a".into(), "b".into(), "c".into()])
        .build(&book())
        .unwrap_or_else(|error| fail(format!("{error}")));
    // x-axis + y-axis + 5 y-ticks + 4 labels + bars-group + 3 names.
    assert_eq!(chart.children().len(), 2 + 5 + 4 + 1 + 3);

    // Bars: width buff = width/(2n) = 1, heights v/max*height, bottoms on
    // a common baseline (the whole chart is centred afterwards, like the
    // Reference's trailing self.center()), left corners pitched by buff.
    let bars = &chart.children()[11];
    assert_eq!(bars.children().len(), 3);
    let bottoms: Vec<f64> = bars
        .children()
        .iter()
        .map(|bar| bar.bbox_point(fmn_core::constants::DOWN).expect("bbox")[1])
        .collect();
    assert!(
        bottoms.iter().all(|y| (*y - bottoms[0]).abs() < 1e-9),
        "bars share one baseline: {bottoms:?}"
    );
    let lefts: Vec<f64> = bars
        .children()
        .iter()
        .map(|bar| bar.bbox_point(fmn_core::constants::LEFT).expect("bbox")[0])
        .collect();
    for i in 0..3 {
        let right = bars.children()[i]
            .bbox_point(fmn_core::constants::RIGHT)
            .expect("bbox");
        let left = bars.children()[i]
            .bbox_point(fmn_core::constants::LEFT)
            .expect("bbox");
        assert!((right[0] - left[0] - 1.0).abs() < 1e-9, "bar {i} width");
        if i > 0 {
            assert!((lefts[i] - lefts[i - 1] - 1.0).abs() < 1e-9, "pitch {i}");
        }
        let expected_height = [0.25, 0.5, 1.0][i] * 4.0;
        let up = bars.children()[i]
            .bbox_point(fmn_core::constants::UP)
            .expect("bbox");
        assert!(
            (up[1] - bottoms[i] - expected_height).abs() < 1e-9,
            "bar {i} height"
        );
    }
    // Gradient endpoints across the three bars: BLUE … YELLOW.
    let first_fill = bars.children()[0].style().fill_color;
    let last_fill = bars.children()[2].style().fill_color;
    assert_eq!(first_fill, BLUE);
    assert_eq!(last_fill, YELLOW);
}

#[test]
fn derived_max_scales_bars_to_the_chart_height() {
    let chart = BarChart::new(vec![10.0, 20.0])
        .max_value(None)
        .n_ticks(2)
        .label_y_axis(false)
        .build(&book())
        .unwrap_or_else(|error| fail(format!("{error}")));
    // No y-labels with label_y_axis(false); ticks = 3 marks.
    let bars = &chart.children()[2 + 3];
    let tallest = &bars.children()[1];
    let up = tallest.bbox_point(fmn_core::constants::UP).expect("bbox");
    let down = tallest.bbox_point(fmn_core::constants::DOWN).expect("bbox");
    assert!((up[1] - down[1] - 4.0).abs() < 1e-9, "tallest fills height");
}

#[test]
fn numpy_round2_is_half_to_even() {
    assert!((numpy_round2(0.125) - 0.12).abs() < 1e-9, "ties go even");
    assert!((numpy_round2(0.135) - 0.14).abs() < 1e-9, "ties go even");
    assert!((numpy_round2(0.124) - 0.12).abs() < 1e-9);
    assert!((numpy_round2(1.005) - 1.0).abs() < 1e-9 || (numpy_round2(1.005) - 1.01).abs() < 1e-9);
}

#[test]
fn self_goldens_lock_the_canonical_table_and_chart() {
    let table =
        TableMobject::from_csv(SAMPLE_CSV, ',').unwrap_or_else(|error| fail(format!("{error}")));
    let table_family = table
        .build(&book())
        .unwrap_or_else(|error| fail(format!("{error}")));

    let chart = BarChart::new(vec![0.5, 1.0])
        .build(&book())
        .unwrap_or_else(|error| fail(format!("{error}")));

    let digest_of = |root: &fmn_library::vmobject::VMobject| {
        let mut bytes = Vec::new();
        let mut stack = vec![root];
        while let Some(current) = stack.pop() {
            for point in current.points() {
                bytes.extend_from_slice(&point[0].to_bits().to_le_bytes());
                bytes.extend_from_slice(&point[1].to_bits().to_le_bytes());
                bytes.extend_from_slice(&point[2].to_bits().to_le_bytes());
            }
            for child in current.children() {
                stack.push(child);
            }
        }
        sha256(&bytes).to_hex()
    };

    // Re-locked when every glyph runs counter-clockwise like the Reference's
    // (fm-hc78): the cell text and the chart's axis numbers.
    const TABLE_GOLDEN: &str = "0666319d002cf912236c27849a6232c01c70162efd76213838dd9b721b393b2c";
    const CHART_GOLDEN: &str = "143b324c742525e7b516bef6898216797cec65ba2398ea40e4890f92976c16a3";
    assert_eq!(digest_of(&table_family), TABLE_GOLDEN, "table drifted");
    assert_eq!(digest_of(&chart), CHART_GOLDEN, "chart drifted");
}

fn live_chart(values: &[f64]) -> (Stage, BarChartBinding) {
    let mut stage = Stage::new();
    let chart = BarChart::new(values.to_vec())
        .n_ticks(1)
        .label_y_axis(false)
        .add_to(&mut stage, &book())
        .expect("live chart");
    stage.add_to_scene(chart.mobject()).expect("scene root");
    (stage, chart)
}

fn points(stage: &Stage, mob: Mob) -> Vec<Vec3> {
    stage.get_points(mob).expect("live point records")
}

fn assert_points_close(actual: &[Vec3], expected: &[Vec3]) {
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(expected) {
        for (&a, &b) in actual.iter().zip(expected) {
            assert!(
                (a - b).abs() < 5e-6,
                "point {actual:?}, expected {expected:?}"
            );
        }
    }
}

fn assert_update_refused(stage: &mut Stage, chart: &mut BarChartBinding, values: &[f64]) {
    let before = stage.snapshot().to_bytes().expect("snapshot");
    let records = stage
        .family(chart.mobject())
        .into_iter()
        .map(|mob| {
            let entry = stage.get(mob).expect("live family");
            (
                mob,
                entry.buffer.revision(),
                entry.buffer.storage_id(),
                entry.placement().coefficients().map(f64::to_bits),
            )
        })
        .collect::<Vec<_>>();
    assert!(chart.change_bar_values(stage, values).is_err());
    assert_eq!(stage.snapshot().to_bytes().expect("snapshot"), before);
    for (mob, revision, storage, placement) in records {
        let entry = stage.get(mob).expect("refusal retains family");
        assert_eq!(entry.buffer.revision(), revision);
        assert_eq!(entry.buffer.storage_id(), storage);
        assert_eq!(
            entry.placement().coefficients().map(f64::to_bits),
            placement
        );
    }
}

#[test]
fn live_bars_cross_zero_preserving_styles_annotations_updaters_and_record_views() {
    use std::cell::Cell;
    use std::rc::Rc;

    let (mut stage, mut chart) = live_chart(&[0.0, 0.5]);
    let bar = chart.bar(0).expect("first bar");
    let other = chart.bar(1).expect("second bar");
    let annotation = stage.add(Mobject::from_points(&[[1.5, -0.75, 0.25]]));
    stage.attach(bar, annotation).expect("annotation");
    let annotation_points = points(&stage, annotation);
    let other_points = points(&stage, other);
    let family = stage.family(chart.mobject());
    let roots = stage.roots().to_vec();
    let calls = Rc::new(Cell::new(0));
    let counter = Rc::clone(&calls);
    let updater = stage
        .add_updater(bar, move |_, _| counter.set(counter.get() + 1), false)
        .expect("updater");
    let buffer = &mut stage.get_mut(bar).expect("bar").buffer;
    let profile = (0..buffer.len())
        .flat_map(|index| [0.1 * index as f32, 0.4, 0.6, 0.75])
        .collect::<Vec<_>>();
    buffer.write_range("fill_rgba", 0, &profile);
    let stroke = buffer.read_column("stroke_rgba").expect("stroke");
    let widths = buffer.read_column("stroke_width").expect("widths");
    let view = buffer.export_field_view("point", true).expect("view");
    let storage = buffer.storage_id();
    for value in [0.0, -0.5, 0.75, 0.0, 0.25] {
        let before = points(&stage, bar);
        chart
            .change_bar_values(&mut stage, &[value])
            .expect("value update");
        let after = points(&stage, bar);
        // Native rectangle anchors 4 and 6 span the persistent bottom edge;
        // anchor 0 lies at the top of the right edge even for signed heights.
        assert_eq!((after[4], after[6]), (before[4], before[6]));
        assert!((after[0][1] - after[6][1] - value * 4.0).abs() < 1e-6);
        let buffer = &stage.get(bar).expect("bar").buffer;
        assert_eq!(buffer.storage_id(), storage);
        assert!(view.is_attached_to(buffer));
        assert_eq!(view.read(0, "point"), buffer.read(0, "point"));
        assert_eq!(buffer.read_column("fill_rgba").expect("fill"), profile);
        assert_eq!(buffer.read_column("stroke_rgba").expect("stroke"), stroke);
        assert_eq!(buffer.read_column("stroke_width").expect("widths"), widths);
        assert_eq!(
            points(&stage, other),
            other_points,
            "prefix leaves later bar intact"
        );
        assert_eq!(points(&stage, annotation), annotation_points);
        assert_eq!(stage.family(chart.mobject()), family);
        assert_eq!(stage.roots(), roots);
        assert_eq!(stage.updater_ids(bar), [updater]);
        assert_eq!(calls.get(), 0, "value updates execute no callbacks");
    }
    let revision = stage.get(bar).expect("bar").buffer.revision();
    chart
        .change_bar_values(&mut stage, &[])
        .expect("empty prefix");
    assert_eq!(stage.get(bar).expect("bar").buffer.revision(), revision);
    stage.update(0.0);
    assert_eq!(calls.get(), 1, "the original updater still runs");
}

#[test]
fn live_value_changes_commute_with_chart_affines_and_keep_individual_bar_widths() {
    for linear in [
        [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        [[-2.0, 0.75, 0.0], [0.0, 0.5, 0.0], [0.25, 1.0, 1.0]],
    ] {
        let (mut actual, mut chart) = live_chart(&[0.2, 0.6, 0.0]);
        let first = chart.bar(0).expect("bar");
        // An authored width/position change is independent of value height.
        actual.apply_affine(
            first,
            Placement::new(
                [[1.5, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
                [0.25, 0.0, 0.0],
            ),
        );
        let source = actual.snapshot();
        let affine = Placement::new(linear, [1.0, -2.0, 0.5]);
        actual.apply_affine(chart.mobject(), affine);
        for values in [[0.0, -0.4, 1.25], [0.6, 0.0, -0.2], [-0.5, 0.7, 0.0]] {
            let mut expected = source.materialize();
            chart
                .clone()
                .change_bar_values(&mut expected, &values)
                .expect("untransformed update");
            expected.apply_affine(chart.mobject(), affine);
            chart
                .change_bar_values(&mut actual, &values)
                .expect("transformed update");
            for &bar in chart.bars() {
                assert_points_close(&points(&actual, bar), &points(&expected, bar));
            }
        }
    }
}

#[test]
fn invalid_chart_installation_and_late_invalid_values_are_atomic() {
    let fonts = book();
    let mut stage = Stage::new();
    let unrelated = stage.add(Mobject::from_points(&[[1.0, 2.0, 3.0]]));
    stage.add_to_scene(unrelated).expect("unrelated root");
    let before = stage.snapshot().to_bytes().expect("snapshot");
    for chart in [
        BarChart::new(vec![]),
        BarChart::new(vec![f64::NAN]),
        BarChart::new(vec![f64::MAX]),
        BarChart::new(vec![1.0]).width(0.0),
        BarChart::new(vec![1.0]).height(-1.0),
        BarChart::new(vec![1.0]).width(1e-50),
        BarChart::new(vec![1.0]).max_value(Some(0.0)),
        BarChart::new(vec![1.0]).max_value(Some(f64::INFINITY)),
        BarChart::new(vec![1.0]).n_ticks(usize::MAX),
        BarChart::new(vec![0.0; MAX_BAR_CHART_BARS + 1]),
    ] {
        assert!(chart.add_to(&mut stage, &fonts).is_err());
        assert_eq!(stage.snapshot().to_bytes().expect("snapshot"), before);
    }
    let (mut stage, mut chart) = live_chart(&[0.25, 0.75]);
    for values in [
        vec![0.5, f64::NAN],
        vec![0.5, f64::INFINITY],
        vec![0.5, f64::MAX],
        vec![0.5, 0.25, 1.0],
    ] {
        assert_update_refused(&mut stage, &mut chart, &values);
    }
    let second = chart.bar(1).expect("bar");
    stage
        .set_points(second, &[[0.0, 0.0, 0.0]; 5])
        .expect("valid but unsupported path");
    assert_update_refused(&mut stage, &mut chart, &[0.5, 1.0]);
}

#[test]
fn signed_and_zero_initial_series_have_a_usable_automatic_scale() {
    for (values, scale, heights) in [
        (vec![-2.0, 1.0, 0.0], 2.0, vec![-4.0, 2.0, 0.0]),
        (vec![0.0, 0.0], 1.0, vec![0.0, 0.0]),
    ] {
        let mut stage = Stage::new();
        let mut chart = BarChart::new(values)
            .max_value(None)
            .label_y_axis(false)
            .add_to(&mut stage, &book())
            .expect("signed chart");
        assert_eq!(chart.max_value(), scale);
        for (&bar, height) in chart.bars().iter().zip(heights) {
            let points = points(&stage, bar);
            assert!((points[0][1] - points[6][1] - height).abs() < 1e-6);
        }
        chart
            .change_bar_values(&mut stage, &[scale])
            .expect("grow from zero or negative");
        let points = points(&stage, chart.bar(0).expect("first bar"));
        assert!((points[0][1] - points[6][1] - 4.0).abs() < 1e-6);
    }
}

#[test]
fn large_finite_scales_keep_exact_scientific_axis_labels() {
    let fonts = book();
    for (value, text) in [
        (i64::MAX as f64, "9.223372036854776e18"),
        (1e20, "1e20"),
        (f64::MAX, "1.7976931348623157e308"),
    ] {
        let chart = BarChart::new(vec![value])
            .max_value(None)
            .n_ticks(1)
            .build(&fonts)
            .expect("finite semantic scale");
        // Two axes, two ticks, then the single numeric label. Compare its
        // actual glyph paths with the expected number, independent of chart
        // centering. This catches both inf and saturated-i64 label strings.
        let actual = chart.children()[4].clone().moved_to([0.0; 3]);
        let expected = fmn_library::Text::new(text)
            .build(&fonts)
            .expect("expected tick text")
            .vmob
            .with_height(0.25, false)
            .moved_to([0.0; 3]);
        let mut pending = vec![(&actual, &expected)];
        while let Some((a, b)) = pending.pop() {
            assert_points_close(a.points(), b.points());
            assert_eq!(a.children().len(), b.children().len());
            pending.extend(a.children().iter().zip(b.children()));
        }
    }
}

#[test]
fn adopted_charts_reject_aliases_foreign_members_and_lost_live_bases() {
    let (mut stage, mut chart) = live_chart(&[0.25, 0.75]);
    let first = chart.bar(0).expect("first");
    let second = chart.bar(1).expect("second");
    let unrelated = stage.add(Mobject::from_points(&[[0.0; 3]]));
    for bars in [
        vec![first, first],
        vec![first, unrelated],
        vec![chart.y_axis()],
    ] {
        assert!(
            BarChartBinding::from_handles(
                &stage,
                chart.mobject(),
                chart.y_axis(),
                &bars,
                1.0,
                4.0 / 4.5,
            )
            .is_err()
        );
    }
    assert!(
        BarChartBinding::from_handles(
            &Stage::new(),
            chart.mobject(),
            chart.y_axis(),
            chart.bars(),
            1.0,
            4.0 / 4.5,
        )
        .is_err()
    );
    let parent = stage.get(second).expect("bar").parents()[0];
    stage.detach(parent, second);
    assert_update_refused(&mut stage, &mut chart, &[0.5, 1.0]);
    stage.attach(parent, second).expect("reattach");
    stage.delete(second).expect("retire bar");
    assert_update_refused(&mut stage, &mut chart, &[0.5, 1.0]);

    let (mut stage, mut chart) = live_chart(&[0.25, 0.75]);
    // In f64 the second baseline is still distinct; publishing those points
    // to f32 would collapse its width. No earlier bar may be written first.
    stage.shift(chart.bar(1).expect("bar"), [1e10, 0.0, 0.0]);
    assert_update_refused(&mut stage, &mut chart, &[0.5, 1.0]);
    stage
        .set_points(chart.y_axis(), &[[0.0; 3]; 3])
        .expect("collapsed axis");
    assert_update_refused(&mut stage, &mut chart, &[]);
}

fn linear_config() -> AnimConfig {
    AnimConfig {
        run_time: 1.0,
        rate_func: RateFunc::linear(),
        ..AnimConfig::default()
    }
}

#[test]
fn value_animation_rebuilds_at_each_begin_and_releases_lifecycle_state() {
    use std::cell::Cell;
    use std::rc::Rc;

    let (mut stage, mut chart) = live_chart(&[0.25]);
    let bar = chart.bar(0).expect("bar");
    let calls = Rc::new(Cell::new(0));
    let counter = Rc::clone(&calls);
    let updater = stage
        .add_updater(bar, move |_, _| counter.set(counter.get() + 1), false)
        .expect("updater");
    let before = stage.snapshot().to_bytes().expect("snapshot");
    let mut animation = ChangeBarValues::new(&stage, &chart, &[0.75])
        .expect("value animation")
        .with_config(AnimConfig {
            suspend_mobject_updating: true,
            ..linear_config()
        });
    assert_eq!(
        stage.snapshot().to_bytes().expect("snapshot"),
        before,
        "constructing targets changes no arena entries"
    );
    assert!(animation.freeze_interpolation().is_none());
    stage.apply_affine(
        chart.mobject(),
        Placement::new(
            [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 1.0]],
            [2.0, -1.0, 0.5],
        ),
    );
    for iteration in 0..3 {
        chart
            .change_bar_values(&mut stage, &[0.0])
            .expect("reset to zero");
        stage.shift(chart.mobject(), [0.25, 0.0, 0.0]);
        // Transform bakes lazy world placement into the authoritative f32
        // source/target records during setup; that is the interpolation basis.
        let baseline = points(&stage, bar)[6].map(|value| f64::from(value as f32));
        animation
            .begin(&mut stage)
            .expect("begin from current geometry");
        assert!(
            animation.freeze_interpolation().is_some(),
            "closed Transform carrier"
        );
        assert!(stage.is_animating(bar));
        assert!(stage.is_updating_suspended(bar));
        stage.update(0.0);
        assert_eq!(calls.get(), iteration, "a suspended child does not run");
        animation.interpolate(&mut stage, 0.5);
        let halfway = points(&stage, bar);
        assert_eq!(halfway[6], baseline);
        assert!((halfway[0][1] - halfway[6][1] - 3.0).abs() < 1e-6);
        animation.finish(&mut stage);
        let final_points = points(&stage, bar);
        assert!((final_points[0][1] - final_points[6][1] - 6.0).abs() < 1e-6);
        assert_eq!(stage.updater_ids(bar), [updater]);
        assert_eq!(
            calls.get(),
            iteration + 1,
            "one updater resume pass per finish"
        );
        for mob in stage.family(chart.mobject()) {
            assert!(!stage.is_animating(mob));
            assert!(!stage.is_updating_suspended(mob));
            assert!(
                stage
                    .get(mob)
                    .expect("family")
                    .buffer
                    .locked_keys()
                    .is_empty()
            );
        }
    }
    stage.delete(bar).expect("retire before next begin");
    let before = stage.snapshot().to_bytes().expect("snapshot");
    assert!(animation.begin(&mut stage).is_err());
    assert_eq!(stage.snapshot().to_bytes().expect("snapshot"), before);
}

#[test]
fn value_animation_uses_preceding_succession_geometry_and_replays_initialized_members() {
    let (mut stage, chart) = live_chart(&[0.25]);
    let initial = stage.snapshot();
    let root = chart.mobject();
    let bar = chart.bar(0).expect("bar");
    let values = ChangeBarValues::new(&stage, &chart, &[1.0])
        .expect("deferred values")
        .with_config(linear_config());
    let target = stage.copy_family(root).expect("affine endpoint");
    let affine = Placement::new(
        [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 1.0]],
        [2.0, 0.0, 0.0],
    );
    stage.apply_affine(target, affine);
    let transform = Transform::new(root, target).with_config(linear_config());
    let mut succession = Succession::in_group(
        &mut stage,
        vec![Box::new(transform), Box::new(values)],
        1.0,
        Some(root),
    )
    .expect("succession")
    .with_rate_func(RateFunc::linear());
    succession.begin(&mut stage).expect("begin");
    for alpha in [0.0, 0.25, 0.5, 0.75, 1.0, 0.2, 0.8, 0.0, 1.0] {
        succession.interpolate(&mut stage, alpha);
        assert_eq!(succession.deferred_error(), None);
        let mut expected = initial.materialize();
        if alpha <= 0.5 {
            expected.apply_affine(
                root,
                Placement::new(
                    [
                        [1.0, 0.0, 0.0],
                        [0.0, 1.0 + 2.0 * alpha, 0.0],
                        [0.0, 0.0, 1.0],
                    ],
                    [4.0 * alpha, 0.0, 0.0],
                ),
            );
        } else {
            expected.apply_affine(root, affine);
            chart
                .clone()
                .change_bar_values(&mut expected, &[0.25 + (alpha - 0.5) * 1.5])
                .expect("expected value");
        }
        assert_points_close(&points(&stage, bar), &points(&expected, bar));
        assert_points_close(
            &points(&stage, chart.y_axis()),
            &points(&expected, chart.y_axis()),
        );
    }
    succession.finish(&mut stage);
    assert_eq!(succession.deferred_error(), None);
    for mob in stage.family(root) {
        assert!(!stage.is_animating(mob));
        assert!(!stage.is_updating_suspended(mob));
        assert!(
            stage
                .get(mob)
                .expect("family")
                .buffer
                .locked_keys()
                .is_empty()
        );
    }
}
