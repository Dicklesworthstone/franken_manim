//! Keep every cached view of a built plane in the same scene coordinate frame.

use super::{ComplexPlane, NumberPlane, ThreeDAxes};
use crate::coords::placement::{CoordinateTransform, coordinate_placement};

impl NumberPlane {
    pub(crate) fn transformed_coordinates(mut self, transform: CoordinateTransform) -> Self {
        let built = self.built.as_mut()
            .expect("NumberPlane::build must run before coordinate-system placement");
        built.axes = core::mem::take(&mut built.axes).transformed_coordinates(transform);
        built.background_lines = transform.geometry(core::mem::take(&mut built.background_lines));
        built.faded_lines = transform.geometry(core::mem::take(&mut built.faded_lines));
        // Preserve extra family children, including ComplexPlane's labels.
        built.vmob = transform.geometry(core::mem::take(&mut built.vmob));
        self
    }
}
coordinate_placement!(NumberPlane);

impl ThreeDAxes {
    pub(crate) fn transformed_coordinates(mut self, transform: CoordinateTransform) -> Self {
        let built = self.built.as_mut()
            .expect("ThreeDAxes::build must run before coordinate-system placement");
        built.axes = core::mem::take(&mut built.axes).transformed_coordinates(transform);
        built.z_axis = transform.line(core::mem::take(&mut built.z_axis));
        built.vmob = transform.geometry(core::mem::take(&mut built.vmob));
        self
    }
}
coordinate_placement!(ThreeDAxes);

impl ComplexPlane {
    pub(crate) fn transformed_coordinates(mut self, transform: CoordinateTransform) -> Self {
        self.plane = core::mem::take(&mut self.plane).transformed_coordinates(transform);
        self.coordinate_labels = self.coordinate_labels.into_iter()
            .map(|label| transform.geometry(label)).collect();
        self
    }
}
coordinate_placement!(ComplexPlane);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoordinateSystem, FontBook, VMobject};
    use fmn_core::constants::{ORIGIN, OUT, PI, UR};
    use fmn_core::types::Vec3;

    fn near(a: Vec3, b: Vec3) {
        for i in 0..3 {
            assert!((a[i] - b[i]).abs() < 1e-8, "{a:?} != {b:?}");
        }
    }

    fn family_near(a: &VMobject, b: &VMobject) {
        assert_eq!(a.points().len(), b.points().len());
        assert_eq!(a.children().len(), b.children().len());
        assert_eq!(a.style(), b.style());
        for (&a, &b) in a.points().iter().zip(b.points()) { near(a, b); }
        for (a, b) in a.children().iter().zip(b.children()) { family_near(a, b); }
    }

    #[test]
    fn placed_number_plane_keeps_grid_axes_graph_and_unit_sizes_coherent() {
        let base = NumberPlane::new().x_range([-2.0, 2.0, 1.0])
            .y_range([-1.0, 1.0, 1.0]).width(4.0).height(3.0)
            .faded_line_ratio(1).build(&FontBook::bundled().unwrap()).unwrap();
        let shift = [1.0, -1.5, 0.0];
        let after = base.clone().scaled_about(0.5, ORIGIN)
            .rotated_about(PI / 6.0, OUT, ORIGIN).shifted(shift);
        let map = |m: VMobject| m.scaled_about(0.5, ORIGIN)
            .rotated_about(PI / 6.0, OUT, ORIGIN).shifted(shift);
        family_near(after.vmob(), &map(base.vmob().clone()));
        family_near(after.faded_lines(), &after.vmob().children()[0]);
        family_near(after.background_lines(), &after.vmob().children()[1]);
        family_near(after.x_axis().vmob(), &after.vmob().children()[2]);
        family_near(after.y_axis().vmob(), &after.vmob().children()[3]);
        assert_eq!(after.x_unit_size(), 0.5 * base.x_unit_size());
        assert_eq!(after.y_unit_size(), 0.5 * base.y_unit_size());
        for p in [[-2.0, 0.5], [0.0, 0.0], [3.0, -2.0]] {
            near(after.p2c(after.c2p(&p)), [p[0], p[1], 0.0]);
        }
        let graph = after.get_graph(|x| 0.25 * x, None).build().unwrap();
        let old = base.get_graph(|x| 0.25 * x, None).build().unwrap();
        family_near(&graph, &map(old));
        let expected = base.vmob().clone().to_edge(UR, 0.4);
        family_near(base.to_edge(UR, 0.4).vmob(), &expected);
    }

    #[test]
    fn three_dimensional_chart_moves_all_three_axes_and_round_trips() {
        let base = ThreeDAxes::new().x_range([-2.0, 2.0, 1.0])
            .y_range([-1.0, 1.0, 1.0]).z_range([-3.0, 3.0, 1.0])
            .build(&FontBook::bundled().unwrap()).unwrap();
        for i in 1..=16 {
            let angle = f64::from(i) * PI / 13.0;
            let after = base.clone().scaled_about(-0.75, [1.0, 0.5, -1.0])
                .rotated_about(angle, [1.0, 2.0, 3.0], ORIGIN).moved_to([2.0, -1.0, 0.5]);
            near(after.vmob().center_point(), [2.0, -1.0, 0.5]);
            for (i, axis) in [after.x_axis(), after.y_axis(), after.z_axis()].into_iter().enumerate() {
                family_near(axis.vmob(), &after.vmob().children()[i]);
                near(axis.n2p(0.0), after.origin());
            }
            for p in [[0.0; 3], [1.0, 0.25, -2.0], [-3.0, 2.0, 4.0]] {
                near(after.p2c(after.c2p(&p)), p);
            }
        }
    }

    #[test]
    fn complex_plane_labels_and_vectors_follow_the_owning_chart() {
        let book = FontBook::bundled().unwrap();
        let mut base = ComplexPlane::new().x_range([-2.0, 2.0, 1.0])
            .y_range([-1.0, 1.0, 1.0]).faded_line_ratio(1).build(&book).unwrap();
        base.add_coordinate_labels_for(&[[1.0, 0.0], [0.0, 1.0]], &book).unwrap();
        let after = base.clone().scaled_about(0.6, ORIGIN)
            .rotated_about(PI / 4.0, OUT, ORIGIN).shifted([-2.0, 1.0, 0.0]);
        let map = |m: VMobject| m.scaled_about(0.6, ORIGIN)
            .rotated_about(PI / 4.0, OUT, ORIGIN).shifted([-2.0, 1.0, 0.0]);
        family_near(after.vmob(), &map(base.vmob().clone()));
        for (before, after) in base.coordinate_labels().iter().zip(after.coordinate_labels()) {
            family_near(after, &map(before.clone()));
        }
        for z in [[1.0, 0.0], [0.0, -1.0], [2.5, -3.0]] {
            let back = after.p2n(after.n2p(z));
            near([back[0], back[1], 0.0], [z[0], z[1], 0.0]);
        }
        let expected = crate::Arrow::new(after.origin(), after.n2p([1.0, 0.5]))
            .buff(0.0).build().unwrap();
        family_near(&after.get_vector(&[1.0, 0.5]), &expected);
    }
}
