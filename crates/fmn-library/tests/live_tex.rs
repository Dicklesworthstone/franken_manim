//! Adopted TeX numbers retain source ownership through real native animation.

use fmn_anim::{AnimError, Animation};
use fmn_core::constants::{BLACK, BLUE, LEFT, ORIGIN, OUT, RED};
use fmn_core::types::Vec3;
use fmn_library::numbers::change_decimal_to_value;
use fmn_library::style::VStyle;
use fmn_library::tex::{TexBindingError, TexSpanBinding};
use fmn_library::{
    DecimalNumber, FontBook, SpanKindU8, SpanMapData, SpanMapEntry, Tex, TexEngine,
    TextMatchingError, TransformMatchingStrings, TransformMatchingTex,
};
use fmn_mobject::{Mob, Mobject, Stage};

fn book() -> FontBook {
    FontBook::bundled().expect("bundled fonts")
}

fn formula(stage: &mut Stage, source: &str) -> TexSpanBinding {
    let engine = TexEngine::new("fmd-math/pack/default", None).unwrap();
    let built = Tex::new(source).build(&engine).unwrap();
    let spans = built.span_map();
    let root = stage.add(built);
    TexSpanBinding::new(stage, root, spans).unwrap()
}

fn decimal() -> DecimalNumber {
    DecimalNumber::new(0.0)
        .num_decimal_places(1)
        .group_with_commas(false)
}

fn close(actual: Vec3, expected: Vec3) {
    for (actual, expected) in actual.into_iter().zip(expected) {
        assert!((actual - expected).abs() < 2e-6, "{actual} != {expected}");
    }
}

fn family_points(stage: &Stage, root: Mob) -> Vec<Vec<Vec3>> {
    stage
        .family(root)
        .into_iter()
        .map(|mob| stage.get_points(mob).unwrap_or_default())
        .collect()
}

fn bounds(points: impl IntoIterator<Item = Vec3>) -> (Vec3, Vec3) {
    let mut points = points.into_iter();
    let first = points.next().expect("measurable geometry");
    let mut min = first;
    let mut max = first;
    for point in points {
        for axis in 0..3 {
            min[axis] = min[axis].min(point[axis]);
            max[axis] = max[axis].max(point[axis]);
        }
    }
    (min, max)
}

fn center((min, max): (Vec3, Vec3)) -> Vec3 {
    std::array::from_fn(|axis| 0.5 * min[axis] + 0.5 * max[axis])
}

#[test]
fn second_occurrence_uses_installed_root_live_paint_and_affine_fit_without_first_sample_jump() {
    let mut stage = Stage::new();
    let mut binding = formula(&mut stage, "a=12.0+12.0");
    let root = binding.mobject();
    stage.add_to_scene(root).unwrap();
    stage.scale(root, 1.4);
    stage.rotate(root, 0.37, OUT, Some(ORIGIN), None);
    stage.shift(root, [2.0, -0.5, 0.0]);
    let occurrences = binding.occurrences(&stage, "12.0").unwrap();
    for &mob in &occurrences[0] {
        stage.set_fill(mob, Some(RED), Some(0.8), None, false);
    }
    for &mob in &occurrences[1] {
        stage.set_fill(mob, Some(BLUE), Some(0.6), None, false);
        stage.set_stroke(mob, Some(RED), Some(2.5), Some(0.4), None, false);
    }
    let children = stage.get(root).unwrap().submobjects().to_vec();
    let untouched = children
        .iter()
        .filter(|mob| !occurrences[1].contains(mob))
        .map(|&mob| {
            (
                mob,
                stage.get_points(mob).unwrap(),
                stage.get(mob).unwrap().placement(),
                stage.get_fill_color(mob),
            )
        })
        .collect::<Vec<_>>();
    let original_placement = stage.get(occurrences[1][0]).unwrap().placement();
    let selected_bounds = bounds(
        occurrences[1]
            .iter()
            .flat_map(|&mob| stage.get_object_points(mob).unwrap()),
    );
    let number = binding
        .make_number_changeable(
            &mut stage,
            "12.0",
            1,
            decimal().include_background_rectangle(true),
            &book(),
        )
        .unwrap();
    let (slot, model) = number.into_parts();
    assert_eq!(model.value(), 12.0);
    assert!(stage.get(root).unwrap().submobjects().contains(&slot));
    assert_eq!(binding.span_map().source.as_ref(), "a=12.0+\\decimalmob");
    assert_eq!(
        binding.occurrences(&stage, "12.0").unwrap(),
        [occurrences[0].clone()],
    );
    assert_eq!(binding.occurrences(&stage, r"\decimalmob").unwrap(), [vec![slot]]);
    assert_eq!(stage.get_fill_color(slot).unwrap().to_rgb8(), BLUE.to_rgb8());
    assert!((stage.get_fill_opacity(slot).unwrap() - 0.6).abs() < 1e-6);
    let members = stage.get(slot).unwrap().submobjects();
    let background = members[0];
    assert_eq!(
        stage.get_fill_color(background).unwrap().to_rgb8(),
        BLACK.to_rgb8(),
    );
    assert_eq!(stage.get_fill_opacity(background), Some(1.0));
    for &digit in &members[1..] {
        assert_eq!(
            stage.get_fill_color(digit).unwrap().to_rgb8(),
            BLUE.to_rgb8(),
        );
        assert_eq!(stage.get_stroke_width(digit), Some(2.5));
    }

    let local = bounds(
        stage
            .family(slot)
            .into_iter()
            .flat_map(|mob| stage.get_object_points(mob).unwrap_or_default()),
    );
    let fitted = stage.get(slot).unwrap().placement();
    close(
        fitted.apply_point(center(local)),
        original_placement.apply_point(center(selected_bounds)),
    );
    let factor = (selected_bounds.1[0] - selected_bounds.0[0]) / (local.1[0] - local.0[0]);
    for row in 0..3 {
        for column in 0..3 {
            assert!(
                (fitted.linear()[row][column] - factor * original_placement.linear()[row][column])
                    .abs()
                    < 1e-12,
            );
        }
    }
    let initial = family_points(&stage, slot);
    let anchor = stage.get_bounding_box_point(slot, LEFT);
    let mut animation = change_decimal_to_value(slot, model, book(), 1234.5).unwrap();
    animation.begin(&mut stage).unwrap();
    assert!(animation.deferred_error().is_none());
    let first_sample = family_points(&stage, slot);
    assert_eq!(first_sample.len(), initial.len());
    for (actual, expected) in first_sample.iter().zip(&initial) {
        assert_eq!(actual.len(), expected.len());
        for (&actual, &expected) in actual.iter().zip(expected) {
            close(actual, expected);
        }
    }
    animation.interpolate(&mut stage, 1.0);
    animation.finish(&mut stage);
    assert!(animation.deferred_error().is_none());
    close(stage.get_bounding_box_point(slot, LEFT), anchor);
    assert_ne!(family_points(&stage, slot), initial);
    assert_eq!(binding.occurrences(&stage, r"\decimalmob").unwrap(), [vec![slot]]);
    for (mob, points, placement, color) in untouched {
        assert!(stage.get(root).unwrap().submobjects().contains(&mob));
        assert_eq!(stage.get_points(mob).unwrap(), points);
        assert!(stage.get(mob).unwrap().placement().same_bits(placement));
        assert_eq!(stage.get_fill_color(mob), color);
    }
}

#[test]
fn replace_all_returns_independent_numbers_and_later_source_edits_remain_valid() {
    let mut stage = Stage::new();
    let mut binding = formula(&mut stage, "1.0+1.0=2.0");
    let numbers = binding
        .make_numbers_changeable(&mut stage, "1.0", decimal(), &book())
        .unwrap();
    assert_eq!(numbers.len(), 2);
    assert_ne!(numbers[0].mobject(), numbers[1].mobject());
    assert_eq!(
        binding.span_map().source.as_ref(),
        "\\decimalmob+\\decimalmob=2.0",
    );
    let second = numbers[1].mobject();
    let second_before = family_points(&stage, second);
    let (first, model) = numbers[0].clone().into_parts();
    let mut animation = change_decimal_to_value(first, model, book(), 9.5).unwrap();
    animation.begin(&mut stage).unwrap();
    animation.finish(&mut stage);
    assert!(animation.deferred_error().is_none());
    assert_eq!(family_points(&stage, second), second_before);
    let last = binding
        .make_number_changeable(&mut stage, "2.0", 0, decimal(), &book())
        .unwrap();
    assert_eq!(last.decimal().value(), 2.0);
    assert_eq!(
        binding.span_map().source.as_ref(),
        "\\decimalmob+\\decimalmob=\\decimalmob",
    );
    assert_eq!(
        binding.occurrences(&stage, r"\decimalmob").unwrap(),
        [vec![first], vec![second], vec![last.mobject()]],
    );
    let before = stage.snapshot_bytes().unwrap();
    assert!(
        binding
            .make_numbers_changeable(&mut stage, "77", decimal(), &book())
            .unwrap()
            .is_empty(),
    );
    assert_eq!(stage.snapshot_bytes().unwrap(), before);
}

#[test]
fn fraction_rules_and_macro_enclosing_ink_keep_identity_and_emission_order() {
    let engine = TexEngine::new("fmd-math/pack/default", None).unwrap();
    for (source, preamble, expected) in [
        (r"\frac{12}{34}", "", r"\frac{\decimalmob}{34}"),
        (
            r"\half{12}",
            r"\newcommand{\half}[1]{\frac{#1}{2}}",
            r"\half{\decimalmob}",
        ),
    ] {
        let mut stage = Stage::new();
        let built = Tex::new(source)
            .preamble(preamble)
            .build(&engine)
            .unwrap();
        let spans = built.span_map();
        assert!(
            spans.entries.iter().any(|entry| entry.kind == SpanKindU8::MathRule),
        );
        let root = stage.add(built);
        let mut binding = TexSpanBinding::new(&stage, root, spans).unwrap();
        let selected = binding.occurrences(&stage, "12").unwrap().remove(0);
        let old_children = stage.get(root).unwrap().submobjects().to_vec();
        let preserved = old_children
            .iter()
            .copied()
            .filter(|mob| !selected.contains(mob))
            .map(|mob| (mob, stage.get_points(mob).unwrap()))
            .collect::<Vec<_>>();
        binding
            .make_number_changeable(
                &mut stage,
                "12",
                0,
                DecimalNumber::new(0.0).num_decimal_places(0),
                &book(),
            )
            .unwrap();
        assert_eq!(binding.span_map().source.as_ref(), expected);
        let remaining = stage
            .get(root)
            .unwrap()
            .submobjects()
            .iter()
            .copied()
            .filter(|mob| old_children.contains(mob))
            .collect::<Vec<_>>();
        assert_eq!(
            remaining,
            preserved.iter().map(|&(mob, _)| mob).collect::<Vec<_>>(),
        );
        for (mob, points) in preserved {
            assert_eq!(stage.get_points(mob).unwrap(), points);
        }
        for span in &binding.span_map().entries {
            assert!(binding.span_map().source.get(span.start..span.end).is_some());
        }
        if !preamble.is_empty() {
            assert!(
                binding
                    .span_map()
                    .entries
                    .iter()
                    .any(|span| span.start == 0 && span.end == expected.len()),
                "macro-generated enclosing ink still owns the complete rewritten call site",
            );
        }
    }
}

fn synthetic(stage: &mut Stage, source: &str, ranges: &[(usize, usize)]) -> TexSpanBinding {
    let children = ranges
        .iter()
        .enumerate()
        .map(|(index, _)| {
            let x = index as f64;
            Mobject::from_points(&[
                [x, 0.0, 0.0],
                [x + 0.25, 1.0, 0.0],
                [x + 0.5, 0.0, 0.0],
            ])
        })
        .collect();
    let mut tree = Mobject::new();
    tree.submobjects = children;
    let root = stage.add(tree);
    TexSpanBinding::new(
        stage,
        root,
        SpanMapData {
            source: source.into(),
            entries: ranges
                .iter()
                .map(|&(start, end)| SpanMapEntry {
                    start,
                    end,
                    kind: SpanKindU8::MathGlyph,
                })
                .collect(),
        },
    )
    .unwrap()
}

#[test]
fn unicode_source_offsets_are_rewritten_without_reordering_native_parts() {
    let mut stage = Stage::new();
    let mut binding = synthetic(
        &mut stage,
        "é1+λ1",
        &[(0, 2), (2, 3), (3, 4), (4, 6), (6, 7)],
    );
    let before = stage.get(binding.mobject()).unwrap().submobjects().to_vec();
    let second = binding
        .make_number_changeable(
            &mut stage,
            "1",
            1,
            DecimalNumber::new(0.0).num_decimal_places(0),
            &book(),
        )
        .unwrap();
    assert_eq!(binding.span_map().source.as_ref(), "é1+λ\\decimalmob");
    let second_points = family_points(&stage, second.mobject());
    let first = binding
        .make_number_changeable(
            &mut stage,
            "1",
            0,
            DecimalNumber::new(0.0).num_decimal_places(0),
            &book(),
        )
        .unwrap();
    let numbers = [first, second];
    assert_eq!(family_points(&stage, numbers[1].mobject()), second_points);
    assert_eq!(binding.span_map().source.as_ref(), "é\\decimalmob+λ\\decimalmob");
    assert_eq!(binding.occurrences(&stage, "é").unwrap(), [vec![before[0]]]);
    assert_eq!(binding.occurrences(&stage, "λ").unwrap(), [vec![before[3]]]);
    assert_eq!(
        stage.get(binding.mobject()).unwrap().submobjects(),
        &[
            before[0],
            numbers[0].mobject(),
            before[2],
            before[3],
            numbers[1].mobject(),
        ],
    );
}

#[test]
fn invalid_index_number_format_geometry_and_source_ownership_are_transactional() {
    let mut stage = Stage::new();
    let mut binding = formula(&mut stage, "a=1.0+1.0");
    let before = stage.snapshot_bytes().unwrap();
    let spans = binding.span_map().clone();
    for result in [
        binding.make_number_changeable(&mut stage, "1.0", 2, decimal(), &book()),
        binding.make_number_changeable(&mut stage, "a", 0, decimal(), &book()),
        binding.make_number_changeable(
            &mut stage,
            "1.0",
            0,
            decimal().font_size(1e100),
            &book(),
        ),
    ] {
        assert!(result.is_err());
    }
    let error = binding
        .make_numbers_changeable(
            &mut stage,
            "1.0",
            decimal().num_decimal_places(100).character_limit(8),
            &book(),
        )
        .unwrap_err();
    assert!(error.is_budget_exceeded());
    assert_eq!(stage.snapshot_bytes().unwrap(), before);
    assert_eq!(binding.span_map(), &spans);

    for (source, ranges, literal) in [
        ("123", vec![(0, 1), (1, 3)], "12"),
        ("1", vec![(0, 1), (0, 1)], "1"),
        ("11", vec![(0, 1), (1, 2), (1, 2)], "1"),
    ] {
        let mut stage = Stage::new();
        let mut binding = synthetic(&mut stage, source, &ranges);
        let before = stage.snapshot_bytes().unwrap();
        let spans = binding.span_map().clone();
        assert!(matches!(
            binding.make_numbers_changeable(&mut stage, literal, decimal(), &book()),
            Err(TexBindingError::AmbiguousSelection { .. }),
        ));
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        assert_eq!(binding.span_map(), &spans);
    }
}

#[test]
fn duplicated_macro_parameter_ink_is_refused_before_any_formula_edit() {
    let engine = TexEngine::new("fmd-math/pack/default", None).unwrap();
    let built = Tex::new(r"\twice{12}")
        .preamble(r"\newcommand{\twice}[1]{#1+#1}")
        .build(&engine)
        .unwrap();
    let spans = built.span_map();
    let mut stage = Stage::new();
    let root = stage.add(built);
    let mut binding = TexSpanBinding::new(&stage, root, spans).unwrap();
    let before = stage.snapshot_bytes().unwrap();
    assert!(matches!(
        binding.make_number_changeable(&mut stage, "12", 0, decimal(), &book()),
        Err(TexBindingError::AmbiguousSelection { .. }),
    ));
    assert_eq!(stage.snapshot_bytes().unwrap(), before);
}

#[test]
fn removed_primitives_keep_external_parents_and_independent_scene_roots() {
    let mut stage = Stage::new();
    let mut binding = formula(&mut stage, "12.0");
    let selected = binding.occurrences(&stage, "12.0").unwrap().remove(0);
    let outside = stage.add(Mobject::new());
    stage.attach(outside, selected[0]).unwrap();
    stage.add_to_scene(selected[1]).unwrap();
    let outside_points = stage.get_points(selected[0]).unwrap();
    let rooted_points = stage.get_points(selected[1]).unwrap();
    binding
        .make_number_changeable(&mut stage, "12.0", 0, decimal(), &book())
        .unwrap();
    assert_eq!(stage.get(outside).unwrap().submobjects(), &[selected[0]]);
    assert_eq!(stage.get_points(selected[0]).unwrap(), outside_points);
    assert_eq!(stage.get_points(selected[1]).unwrap(), rooted_points);
    assert!(stage.roots().contains(&selected[1]));
    for &private in &selected[2..] {
        assert!(!stage.contains(private));
    }
}

#[test]
fn copied_binding_remaps_slots_and_snapshot_restore_requires_matching_provenance() {
    let mut stage = Stage::new();
    let mut binding = formula(&mut stage, "1.0+2.0");
    let original_binding = binding.clone();
    let snapshot = stage.snapshot();
    let number = binding
        .make_number_changeable(&mut stage, "1.0", 0, decimal(), &book())
        .unwrap();
    let copied = stage.copy_family_mapped(binding.mobject()).unwrap();
    let mut copy_binding = binding.for_copy(&stage, &copied).unwrap();
    let copy_slot = copied.get(number.mobject()).unwrap();
    assert_eq!(
        copy_binding.occurrences(&stage, r"\decimalmob").unwrap(),
        [vec![copy_slot]],
    );
    let original_points = family_points(&stage, binding.mobject());
    let mut animation =
        change_decimal_to_value(copy_slot, number.decimal().clone(), book(), 8.0).unwrap();
    animation.begin(&mut stage).unwrap();
    animation.finish(&mut stage);
    copy_binding
        .make_number_changeable(&mut stage, "2.0", 0, decimal(), &book())
        .unwrap();
    assert_eq!(family_points(&stage, binding.mobject()), original_points);
    assert_eq!(binding.span_map().source.as_ref(), "\\decimalmob+2.0");
    assert_eq!(
        copy_binding.span_map().source.as_ref(),
        "\\decimalmob+\\decimalmob",
    );
    assert!(original_binding.for_copy(&stage, &copied).is_err());
    stage.restore(&snapshot);
    assert!(binding.occurrences(&stage, "1.0").is_err());
    assert_eq!(original_binding.occurrences(&stage, "1.0").unwrap().len(), 1);
}

#[test]
fn grouped_matching_requires_explicit_binding_and_replans_after_numeric_regeneration() {
    for strings in [false, true] {
        let mut stage = Stage::new();
        let mut source = formula(&mut stage, "v=1.0");
        let mut target = formula(&mut stage, "v=2.0");
        let number = source
            .make_number_changeable(&mut stage, "1.0", 0, decimal(), &book())
            .unwrap();
        target
            .make_number_changeable(&mut stage, "2.0", 0, decimal().unit("kg"), &book())
            .unwrap();
        stage.add_to_scene(source.mobject()).unwrap();
        let before = stage.snapshot_bytes().unwrap();
        assert!(matches!(
            TransformMatchingTex::new(
                &mut stage,
                source.mobject(),
                target.mobject(),
                source.span_map(),
                target.span_map(),
            ),
            Err(TextMatchingError::InvalidLayout { .. }),
        ));
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        let mut planned: Box<dyn Animation> = if strings {
            Box::new(TransformMatchingStrings::from_bindings(&mut stage, &source, &target).unwrap())
        } else {
            Box::new(TransformMatchingTex::from_bindings(&mut stage, &source, &target).unwrap())
        };
        let (mob, model) = number.into_parts();
        let mut update = change_decimal_to_value(mob, model, book(), 123.4).unwrap();
        update.begin(&mut stage).unwrap();
        update.finish(&mut stage);
        assert!(update.deferred_error().is_none());
        let before = stage.snapshot_bytes().unwrap();
        assert!(matches!(
            planned.begin(&mut stage),
            Err(AnimError::Stage(_)),
        ));
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        let mut fresh: Box<dyn Animation> = if strings {
            Box::new(TransformMatchingStrings::from_bindings(&mut stage, &source, &target).unwrap())
        } else {
            Box::new(TransformMatchingTex::from_bindings(&mut stage, &source, &target).unwrap())
        };
        fresh.begin(&mut stage).unwrap();
        fresh.finish(&mut stage);
        fresh.clean_up_from_scene(&mut stage);
        assert!(fresh.deferred_error().is_none());
        assert_eq!(stage.roots(), &[target.mobject()]);
    }
}

#[test]
fn matching_rejects_cross_operand_and_within_operand_descendant_aliases_before_allocating() {
    for within_source in [false, true] {
        let mut stage = Stage::new();
        let mut source = formula(&mut stage, "1.0+1.0");
        let mut target = formula(&mut stage, "2.0+2.0");
        let a = source
            .make_numbers_changeable(&mut stage, "1.0", decimal(), &book())
            .unwrap();
        let b = target
            .make_numbers_changeable(&mut stage, "2.0", decimal(), &book())
            .unwrap();
        let shared = stage.get(a[0].mobject()).unwrap().submobjects()[0];
        let borrower = if within_source {
            a[1].mobject()
        } else {
            b[0].mobject()
        };
        stage.attach(borrower, shared).unwrap();
        let before = stage.snapshot_bytes().unwrap();
        let result = TransformMatchingTex::from_bindings(&mut stage, &source, &target);
        if within_source {
            assert!(matches!(
                result,
                Err(TextMatchingError::InvalidLayout { .. }),
            ));
        } else {
            assert!(matches!(result, Err(TextMatchingError::AliasedFamilies)));
        }
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
    }
}
