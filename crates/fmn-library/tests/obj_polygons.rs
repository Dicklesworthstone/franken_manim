//! Independent coverage, corner-attribute and source-face accounting oracles.
use std::collections::BTreeMap;
use std::fmt::Write;

use fmn_library::{ObjDocument, ObjError, ObjLimits, ObjMaterial, ObjMesh, ThreeDModel};
use fmn_mobject::{ImageColorSpace, ImageResource, ImageSampler, Mobject, RenderPrimitive};

const NOTCH: [[f64; 2]; 9] = [
    [0.0, 0.0],
    [2.0, 0.0],
    [4.0, 0.0],
    [4.0, 4.0],
    [3.0, 4.0],
    [3.0, 1.0],
    [1.0, 1.0],
    [1.0, 4.0],
    [0.0, 4.0],
];

// Corner1 is geometrically collinear but its UV is deliberately not the
// interpolation of corners0 and2. Dropping it would change the authored map.
const UVS: [[f64; 2]; 9] = [
    [0.0, 0.0],
    [0.875, 0.25],
    [1.0, 0.0],
    [1.0, 1.0],
    [0.75, 1.0],
    [0.75, 0.25],
    [0.25, 0.25],
    [0.25, 1.0],
    [0.0, 1.0],
];
const NORMALS: [[f64; 3]; 9] = [
    [0.0, 0.0, 1.0],
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [-1.0, 0.0, 0.0],
    [0.0, -1.0, 0.0],
    [0.0, 0.0, -1.0],
    [0.0, 0.0, 1.0],
    [1.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
];

fn positions(axis: usize) -> [[f64; 3]; 9] {
    NOTCH.map(|[x, y]| match axis {
        0 => [0.0, x, y],
        1 => [y, 0.0, x],
        _ => [x, y, 0.0],
    })
}

fn attributed_source(
    points: &[[f64; 3]; 9],
    clockwise: bool,
    close: bool,
    negative: bool,
) -> String {
    let mut source = String::new();
    for [x, y, z] in points {
        writeln!(source, "v {x} {y} {z}").unwrap();
    }
    for [u, v] in UVS {
        writeln!(source, "vt {u} {v}").unwrap();
    }
    for [x, y, z] in NORMALS {
        writeln!(source, "vn {x} {y} {z}").unwrap();
    }
    let mut order: Vec<usize> = (0..9).collect();
    if clockwise {
        order.reverse();
    }
    if close {
        order.push(order[0]);
    }
    source.push_str("usemtl polygon\nf");
    for index in order {
        let index = if negative {
            i64::try_from(index).unwrap() - 9
        } else {
            i64::try_from(index + 1).unwrap()
        };
        write!(source, " {index}/{index}/{index}").unwrap();
    }
    source.push('\n');
    source
}

fn polygon_source(points: &[[f64; 3]]) -> String {
    let mut source = String::new();
    for [x, y, z] in points {
        writeln!(source, "v {x} {y} {z}").unwrap();
    }
    source.push('f');
    for index in 1..=points.len() {
        write!(source, " {index}").unwrap();
    }
    source.push('\n');
    source
}

fn record(child: &Mobject, row: usize, field: &str) -> Vec<f64> {
    child
        .buffer
        .read(row, field)
        .unwrap()
        .into_iter()
        .map(f64::from)
        .collect()
}

fn area2(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

#[test]
fn concave_faces_cover_the_authored_region_in_all_dominant_planes_and_windings() {
    for axis in 0..3 {
        for clockwise in [false, true] {
            for negative in [false, true] {
                let source = attributed_source(&positions(axis), clockwise, true, negative);
                let mesh = ObjMesh::parse(source.as_bytes()).unwrap();
                assert_eq!(mesh.triangle_count(), 7);
                assert_eq!(ObjMesh::parse(source.as_bytes()).unwrap(), mesh);
                let sign = if clockwise { -1.0 } else { 1.0 };
                let mut area = 0.0;
                let mut seen = [false; 9];
                for (index, triangle) in mesh.triangles.iter().enumerate() {
                    let [a, b, c] = triangle.map(|corner| NOTCH[corner.vertex]);
                    let signed = area2(a, b, c) * sign;
                    assert!(signed > 0.0, "{axis} {clockwise}: {triangle:?}");
                    area += signed * 0.5;
                    let mut normal = [0.0; 3];
                    normal[axis] = sign;
                    assert_eq!(mesh.face_normal(index), normal);
                    for corner in triangle {
                        seen[corner.vertex] = true;
                        assert_eq!(corner.tex_coord, Some(corner.vertex));
                        assert_eq!(corner.normal, Some(corner.vertex));
                    }
                }
                assert!(seen.into_iter().all(|value| value));
                assert!((area - 10.0).abs() < 1e-12);
                for x in 0..16 {
                    for y in 0..16 {
                        let point = [(f64::from(x) + 0.5) / 4.0, (f64::from(y) + 0.5) / 4.0];
                        let expected = point[1] < 1.0 || point[0] < 1.0 || point[0] > 3.0;
                        let covered = mesh.triangles.iter().any(|triangle| {
                            let [a, b, c] = triangle.map(|corner| NOTCH[corner.vertex]);
                            area2(a, b, point) * sign >= 0.0
                                && area2(b, c, point) * sign >= 0.0
                                && area2(c, a, point) * sign >= 0.0
                        });
                        assert_eq!(covered, expected, "{axis} {clockwise}: {point:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn convex_fans_keep_exact_source_triangle_order_in_both_windings() {
    for clockwise in [false, true] {
        let mut points = [
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
        ];
        if clockwise {
            points.reverse();
        }
        let mesh = ObjMesh::parse(polygon_source(&points).as_bytes()).unwrap();
        assert_eq!(
            mesh.triangles
                .iter()
                .map(|triangle| triangle.map(|corner| corner.vertex))
                .collect::<Vec<_>>(),
            vec![[0, 1, 2], [0, 2, 3]]
        );
    }
}

#[test]
fn actual_face_ranges_keep_materials_aligned_after_a_repeated_closing_corner() {
    let mut source = attributed_source(&positions(2), false, true, false);
    source.push_str("usemtl detail\nf 1/1/1 3/3/3 9/9/9\nusemtl detail\nf 1/1/1 3/3/3 6/6/6\n");
    let doc = ObjDocument::parse(source.as_bytes()).unwrap();
    assert_eq!(doc.mesh().triangle_count(), 9);
    assert_eq!(doc.mesh(), &ObjMesh::parse(source.as_bytes()).unwrap());
    assert_eq!(doc.runs().len(), 2);
    assert_eq!(doc.runs()[0].material.as_deref(), Some("polygon"));
    assert_eq!(doc.runs()[0].triangles, 0..7);
    assert_eq!(doc.runs()[1].material.as_deref(), Some("detail"));
    assert_eq!(doc.runs()[1].triangles, 7..9);
    assert_eq!(
        doc.mesh().triangles[7].map(|corner| corner.vertex),
        [0, 2, 8]
    );
    assert_eq!(
        doc.mesh().triangles[8].map(|corner| corner.vertex),
        [0, 2, 5]
    );
}

#[test]
fn native_textured_children_retain_collinear_uv_and_normal_records() {
    let mut source = attributed_source(&positions(2), false, true, false);
    source.push_str("usemtl detail\nf 1/1/1 3/3/3 9/9/9\n");
    let doc = ObjDocument::parse(source.as_bytes()).unwrap();
    let materials = BTreeMap::from([
        (
            "polygon".into(),
            ObjMaterial {
                texture: Some("map.png".into()),
                ..ObjMaterial::default()
            },
        ),
        ("detail".into(), ObjMaterial::default()),
    ]);
    let resource = ImageResource::rgba8(
        2,
        1,
        vec![255, 0, 0, 255, 0, 0, 255, 255],
        ImageColorSpace::Srgb,
        ImageSampler::default(),
    )
    .unwrap();
    let model = doc
        .to_mobject(
            4.0,
            &materials,
            &BTreeMap::from([("polygon".into(), resource.clone())]),
        )
        .unwrap();
    assert_eq!(model.submobjects.len(), 2);
    let child = &model.submobjects[0];
    assert_eq!(child.render_primitive, RenderPrimitive::TriangleMesh);
    assert_eq!(child.image.as_ref(), Some(&resource));
    assert_eq!(child.buffer.len(), 21);
    assert_eq!(model.submobjects[1].buffer.len(), 3);
    assert!(model.submobjects[1].image.is_none());
    let mut collinear_rows = 0;
    for (row, corner) in doc.mesh().triangles[..7].iter().flatten().enumerate() {
        let index = corner.vertex;
        let point = [NOTCH[index][0] - 2.0, NOTCH[index][1] - 2.0, 0.0];
        let normal = NORMALS[index];
        assert_eq!(record(child, row, "point"), point);
        assert_eq!(
            record(child, row, "d_normal_point"),
            [point[0] + normal[0], point[1] + normal[1], normal[2]]
        );
        assert_eq!(
            record(child, row, "im_coords"),
            [UVS[index][0], 1.0 - UVS[index][1]]
        );
        collinear_rows += usize::from(index == 1);
    }
    assert!(collinear_rows > 0);
    let plain: Mobject = ThreeDModel::from_obj(source.as_bytes())
        .unwrap()
        .with_height(4.0)
        .into();
    assert_eq!(plain.buffer.len(), 24);
    for row in 0..21 {
        assert_eq!(
            plain.buffer.read(row, "point"),
            child.buffer.read(row, "point")
        );
    }
}

#[test]
fn polygon_projection_handles_extreme_finite_scales_and_offsets() {
    let base = positions(2);
    for points in [
        base.map(|p| p.map(|coordinate| coordinate * 1e-150)),
        base.map(|p| [1e150 + p[0] * 1e140, -1e150 + p[1] * 1e140, 1e150]),
        base.map(|p| [p[0] * 1e-100, p[1] * 1e-100, 1e300]),
        base.map(|p| [(p[0] - 2.0) * 5e307, (p[1] - 2.0) * 5e307, 0.0]),
    ] {
        let mesh =
            ObjMesh::parse(attributed_source(&points, false, true, false).as_bytes()).unwrap();
        assert_eq!(mesh.triangle_count(), 7);
        assert_eq!(mesh.vertices, points);
        let mut seen = [false; 9];
        for corner in mesh.triangles.iter().flatten() {
            seen[corner.vertex] = true;
        }
        assert!(seen.into_iter().all(|value| value));
    }
}

#[test]
fn closed_face_budget_counts_only_the_emitted_triangles() {
    let source = attributed_source(&positions(2), false, true, false);
    let accepted = ObjLimits {
        max_triangles: 7,
        ..ObjLimits::default()
    };
    assert_eq!(
        ObjMesh::parse_with_limits(source.as_bytes(), &accepted)
            .unwrap()
            .triangle_count(),
        7
    );
    let refused = ObjLimits {
        max_triangles: 6,
        ..ObjLimits::default()
    };
    let line = source
        .lines()
        .position(|line| line.starts_with("f "))
        .unwrap()
        + 1;
    assert_eq!(
        ObjMesh::parse_with_limits(source.as_bytes(), &refused),
        Err(ObjError::TooManyTriangles { line, limit: 6 })
    );
}

#[test]
fn nonplanar_and_nonsimple_polygons_refuse_with_the_source_face_line() {
    let invalid = [
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.1],
            [0.0, 1.0, 0.0],
        ],
        vec![
            [0.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
            [2.0, 2.0, 0.0],
        ],
        vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [2.0, 2.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 2.0, 0.0],
        ],
        vec![
            [0.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 2.0, 0.0],
            [0.0, 2.0, 0.0],
        ],
        vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [2.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
        ],
    ];
    for points in invalid {
        let source = polygon_source(&points);
        let error = ObjMesh::parse(source.as_bytes()).unwrap_err();
        let line = match &error {
            ObjError::InvalidPolygon { line, .. } | ObjError::Triangulation { line, .. } => *line,
            other => panic!("unexpected refusal: {other}"),
        };
        assert_eq!(line, points.len() + 1);
        assert!(error.to_string().contains(&format!("line {line}")));
    }
}

#[test]
fn coincident_corners_with_distinct_attributes_are_never_silently_dropped() {
    let mut source = attributed_source(&positions(2), false, false, false);
    source.pop();
    source.push_str(" 1/2/1\n");
    assert!(matches!(
        ObjMesh::parse(source.as_bytes()),
        Err(ObjError::Triangulation { .. })
    ));
    assert!(ObjDocument::parse(source.as_bytes()).is_err());
}

#[test]
fn small_planarity_roundoff_is_admitted_without_moving_original_vertices() {
    let points = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 1e-7],
        [0.0, 1.0, 0.0],
    ];
    let mesh = ObjMesh::parse(polygon_source(&points).as_bytes()).unwrap();
    assert_eq!(mesh.vertices, points);
    assert_eq!(mesh.triangle_count(), 2);
}

#[test]
fn explicit_degenerate_triangles_keep_their_defined_unlit_behavior() {
    let mesh = ObjMesh::parse(b"v 0 0 0\nv 0 0 0\nv 0 0 0\nf 1 2 3\n").unwrap();
    assert_eq!(mesh.triangle_count(), 1);
    assert_eq!(mesh.face_normal(0), [0.0; 3]);
}

#[test]
fn excessive_face_corner_storage_refuses_before_index_resolution() {
    let mut source = String::from("f");
    for _ in 0..=fmn_geom::MAX_EARCLIP_VERTICES {
        source.push_str(" 1");
    }
    assert_eq!(
        ObjMesh::parse(source.as_bytes()),
        Err(ObjError::FaceTooLarge {
            line: 1,
            limit: fmn_geom::MAX_EARCLIP_VERTICES
        })
    );
}
