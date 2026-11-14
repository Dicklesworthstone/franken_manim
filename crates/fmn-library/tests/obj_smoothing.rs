//! OBJ smoothing is a source-face property, independent of triangulation,
//! authored normals, texture seams and the renderer that consumes the mesh.
use std::collections::BTreeMap;

use fmn_library::{ObjDocument, ObjError, ObjLimits, ObjMaterial, ObjMesh, ThreeDModel};
use fmn_mobject::Mobject;

// First face: area 1, +Z. Second face: area 2, +Y.
const WEDGE: &str = "v 0 0 0\nv 2 0 0\nv 0 1 0\nv 0 0 2\n";

fn near(actual: [f64; 3], expected: [f64; 3]) {
    for axis in 0..3 {
        assert!(
            (actual[axis] - expected[axis]).abs() < 1e-12,
            "{actual:?} != {expected:?}"
        );
    }
}

fn blended() -> [f64; 3] {
    let root = 5.0_f64.sqrt();
    [0.0, 2.0 / root, 1.0 / root]
}

#[test]
fn smoothing_uses_polygon_area_and_shared_vertex_identity() {
    let mesh = ObjMesh::parse(format!("{WEDGE}s 7\nf 1 2 3\nf 1 4 2\n").as_bytes()).unwrap();
    assert_eq!(mesh.normals.len(), 4);
    near(mesh.corner_normal(0, 0), blended());
    near(mesh.corner_normal(0, 1), blended());
    near(mesh.corner_normal(1, 0), blended());
    near(mesh.corner_normal(1, 2), blended());
    near(mesh.corner_normal(0, 2), [0.0, 0.0, 1.0]);
    near(mesh.corner_normal(1, 1), [0.0, 1.0, 0.0]);
    assert_eq!(mesh.triangles[0][0].normal, mesh.triangles[1][0].normal);
    assert_eq!(mesh.triangles[0][1].normal, mesh.triangles[1][2].normal);
}

#[test]
fn default_off_zero_and_distinct_groups_preserve_hard_edges() {
    for (first, second) in [
        ("", ""),
        ("s off\n", ""),
        ("s 0\n", ""),
        ("s 7\n", "s off\n"),
        ("s 7\n", "s 0\n"),
        ("s 7\n", "s 8\n"),
    ] {
        let source = format!("{WEDGE}{first}f 1 2 3\n{second}f 1 4 2\n");
        let mesh = ObjMesh::parse(source.as_bytes()).unwrap();
        for corner in 0..3 {
            near(mesh.corner_normal(0, corner), [0.0, 0.0, 1.0]);
            near(mesh.corner_normal(1, corner), [0.0, 1.0, 0.0]);
        }
    }
    let implicit = ObjMesh::parse(format!("{WEDGE}f 1 2 3\nf 1 4 2\n").as_bytes()).unwrap();
    let off = ObjMesh::parse(format!("{WEDGE}s off\nf 1 2 3\nf 1 4 2\n").as_bytes()).unwrap();
    assert_eq!(implicit, off, "ordinary unsmoothed meshes remain unchanged");
    assert!(off.normals.is_empty());
}

#[test]
fn on_alias_and_reselected_groups_have_stable_normals() {
    let numbered = ObjMesh::parse(format!("{WEDGE}s 1\nf 1 2 3\nf 1 4 2\n").as_bytes()).unwrap();
    for directive in ["s on", "s 01", "s 1 # a comment"] {
        let source = format!("{WEDGE}{directive}\nf 1 2 3\ns off\ns 1\nf 1 4 2\n");
        let mesh = ObjMesh::parse(source.as_bytes()).unwrap();
        assert_eq!(mesh, numbered);
    }
    let maximal =
        ObjMesh::parse(format!("{WEDGE}s {}\nf 1 2 3\nf 1 4 2\n", u64::MAX).as_bytes()).unwrap();
    assert_eq!(maximal.normals, numbered.normals);
    assert_eq!(maximal.triangles, numbered.triangles);
}

#[test]
fn authored_normals_and_negative_indices_keep_declaration_time_meaning() {
    let source = format!("{WEDGE}vn 5 0 0\ns 1\nf 1//-1 2 3\nvn 0 0 -3\nf 1//-1 4 2\n");
    let mesh = ObjMesh::parse(source.as_bytes()).unwrap();
    assert_eq!(&mesh.normals[..2], &[[5.0, 0.0, 0.0], [0.0, 0.0, -3.0]]);
    assert_eq!(mesh.triangles[0][0].normal, Some(0));
    assert_eq!(mesh.triangles[1][0].normal, Some(1));
    near(mesh.corner_normal(0, 0), [1.0, 0.0, 0.0]);
    near(mesh.corner_normal(1, 0), [0.0, 0.0, -1.0]);
    near(mesh.corner_normal(0, 1), blended());
    assert_eq!(
        mesh.normals.len(),
        5,
        "only missing vertex/group pairs synthesize normals"
    );

    // A later missing normal also sees earlier geometry carrying explicit vn.
    let source = format!("{WEDGE}vn 1 0 0\ns 1\nf 1//1 2//1 3//1\nf 1 4 2\n");
    let mesh = ObjMesh::parse(source.as_bytes()).unwrap();
    near(mesh.corner_normal(0, 0), [1.0, 0.0, 0.0]);
    near(mesh.corner_normal(1, 0), blended());
    near(mesh.corner_normal(1, 2), blended());
}

#[test]
fn source_polygon_weights_do_not_depend_on_fan_root_or_ear_clipping() {
    let models = [
        (
            "v 0 0 0\nv 2 0 0\nv 2 1 0\nv 0 1 0\nv 0 0 2\n",
            "1 2 3 4",
            "2 3 4 1",
            "1 5 2",
        ),
        (
            "v 0 0 0\nv 2 0 0\nv 2 2 0\nv 1 1 0\nv 0 2 0\nv 0 0 3\n",
            "1 2 3 4 5",
            "2 3 4 5 1",
            "1 6 2",
        ),
    ];
    let expected = [0.0, 0.5_f64.sqrt(), 0.5_f64.sqrt()];
    for (vertices, polygon, rotated_polygon, neighbor) in models {
        for polygon in [polygon, rotated_polygon] {
            let mesh =
                ObjMesh::parse(format!("{vertices}s 1\nf {polygon}\nf {neighbor}\n").as_bytes())
                    .unwrap();
            for (triangle, corners) in mesh.triangles.iter().enumerate() {
                for (corner, value) in corners.iter().enumerate() {
                    if value.vertex == 0 || value.vertex == 1 {
                        near(mesh.corner_normal(triangle, corner), expected);
                    }
                }
            }
        }
    }
}

#[test]
fn smooth_normals_cross_uv_and_material_seams_without_changing_painter_order() {
    let source = format!(
        "{WEDGE}vt 0 0\nvt 1 1\ns 3\nusemtl front\nf 1/1 2/1 3/1\no second\ng back\nusemtl back\nf 1/2 4/2 2/2\n"
    );
    let document = ObjDocument::parse(source.as_bytes()).unwrap();
    assert_eq!(document.runs().len(), 2);
    assert_eq!(document.runs()[0].triangles, 0..1);
    assert_eq!(document.runs()[1].triangles, 1..2);
    let mesh = document.mesh();
    assert_eq!(mesh.triangles[0][0].tex_coord, Some(0));
    assert_eq!(mesh.triangles[1][0].tex_coord, Some(1));
    near(mesh.corner_normal(0, 0), blended());
    near(mesh.corner_normal(1, 0), blended());
    let materials = BTreeMap::from([
        ("front".to_owned(), ObjMaterial::default()),
        ("back".to_owned(), ObjMaterial::default()),
    ]);
    let material_model = document
        .to_mobject(3.0, &materials, &BTreeMap::new())
        .unwrap();
    let plain_model: Mobject = ThreeDModel::from_obj(source.as_bytes()).unwrap().into();
    for (index, child) in material_model.submobjects.iter().enumerate() {
        for corner in 0..3 {
            assert_eq!(
                child.buffer.read(corner, "point"),
                plain_model.buffer.read(index * 3 + corner, "point")
            );
            assert_eq!(
                child.buffer.read(corner, "d_normal_point"),
                plain_model
                    .buffer
                    .read(index * 3 + corner, "d_normal_point")
            );
        }
    }
}

#[test]
fn finite_extreme_scales_generate_finite_consistent_normals() {
    for scale in [1e-200, 1.0, 1e200] {
        let source = format!(
            "v 0 0 0\nv {} 0 0\nv 0 {} 0\nv 0 0 {}\ns 1\nf 1 2 3\nf 1 4 2\n",
            2.0 * scale,
            scale,
            2.0 * scale
        );
        let mesh = ObjMesh::parse(source.as_bytes()).unwrap();
        near(mesh.corner_normal(0, 0), blended());
        assert!(mesh.normals.iter().flatten().all(|value| value.is_finite()));
    }
    let mesh = ObjMesh::parse(
        b"v -1e308 0 0\nv 1e308 0 0\nv -1e308 1e308 0\nv -1e308 0 1e308\ns 1\nf 1 2 3\nf 1 4 2\n",
    )
    .unwrap();
    near(
        mesh.corner_normal(0, 0),
        [0.0, 0.5_f64.sqrt(), 0.5_f64.sqrt()],
    );
    assert!(mesh.normals.iter().flatten().all(|value| value.is_finite()));
}

#[test]
fn degenerate_and_cancelling_faces_keep_defined_face_fallbacks() {
    let mesh =
        ObjMesh::parse(b"v 0 0 0\nv 1 0 0\nv 0 1 0\ns 1\nf 1 2 3\nf 1 3 2\nf 1 1 1\n").unwrap();
    near(mesh.corner_normal(0, 0), [0.0, 0.0, 1.0]);
    near(mesh.corner_normal(1, 0), [0.0, 0.0, -1.0]);
    near(mesh.corner_normal(2, 0), [0.0; 3]);
    assert!(mesh.normals.iter().flatten().all(|value| value.is_finite()));
}

#[test]
fn zero_area_extreme_faces_do_not_erase_valid_shared_normals() {
    let vertices = "v 0 0 0\nv 1 0 0\nv 0 1 0\nv 1e200 0 0\nv 2e200 0 0\nv 0 1e200 0\n";
    for faces in [
        // A huge collinear face must contribute nothing in either order.
        "f 1 2 3\nf 1 4 5\n",
        "f 1 4 5\nf 1 2 3\n",
        // Two large opposite areas cancel, then a small real face remains.
        "f 1 4 6\nf 1 6 4\nf 1 2 3\n",
    ] {
        let mesh = ObjMesh::parse(format!("{vertices}s 1\n{faces}").as_bytes()).unwrap();
        for (triangle, corners) in mesh.triangles.iter().enumerate() {
            if corners.iter().any(|corner| corner.vertex == 1) {
                near(mesh.corner_normal(triangle, 0), [0.0, 0.0, 1.0]);
                near(mesh.normals[corners[0].normal.unwrap()], [0.0, 0.0, 1.0]);
            }
        }
        assert!(mesh.normals.iter().flatten().all(|value| value.is_finite()));
    }
}

#[test]
fn malformed_smoothing_and_generated_normal_budgets_refuse_by_line() {
    for bad in [
        "",
        "-1",
        "+1",
        "one",
        "1 2",
        "on off",
        "1.5",
        "18446744073709551616",
    ] {
        assert!(matches!(
            ObjMesh::parse(format!("# heading\ns {bad}\n").as_bytes()),
            Err(ObjError::BadSmoothingGroup { line: 2, .. })
        ));
    }
    let source = format!("{WEDGE}s 1\nf 1 2 3\nf 1 4 2\n");
    let exact = ObjLimits {
        max_normals: 4,
        ..ObjLimits::default()
    };
    assert_eq!(
        ObjMesh::parse_with_limits(source.as_bytes(), &exact)
            .unwrap()
            .normals
            .len(),
        4
    );
    let short = ObjLimits {
        max_normals: 3,
        ..exact
    };
    assert!(matches!(
        ObjMesh::parse_with_limits(source.as_bytes(), &short),
        Err(ObjError::TooManyNormals { line: 7, limit: 3 })
    ));
    let late_declaration = format!("{WEDGE}s 1\nf 1 2 3\nvn 0 0 1\n");
    assert!(matches!(
        ObjMesh::parse_with_limits(late_declaration.as_bytes(), &short),
        Err(ObjError::TooManyNormals { line: 7, limit: 3 })
    ));
    let explicit = format!("{WEDGE}vn 0 0 1\ns 1\nf 1//1 2//1 3//1\nf 1//1 4//1 2//1\n");
    let explicit_only = ObjLimits {
        max_normals: 1,
        ..exact
    };
    assert_eq!(
        ObjMesh::parse_with_limits(explicit.as_bytes(), &explicit_only)
            .unwrap()
            .normals
            .len(),
        1
    );
}

#[test]
fn smooth_imports_render_like_explicit_normals_across_materials_threads_and_restore() {
    use fmn_core::color::LinearRgba;
    use fmn_library::{ObjAssetLimits, parse_mtl};
    use fmn_mobject::{Snapshot, Stage};
    use fmn_render::{
        Camera, CameraConfig, EngineIdentity, FrameConfig, RetainedFrameRenderer,
        RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
    };

    fn stage(source: &str, materials: bool) -> Stage {
        let model: Mobject = if materials {
            let document = ObjDocument::parse(source.as_bytes()).unwrap();
            assert_eq!(document.runs().len(), 2);
            let materials = parse_mtl(
                b"newmtl A\nKd .2 .65 .9\nnewmtl B\nKd .9 .55 .15\n",
                &ObjAssetLimits::default(),
            )
            .unwrap();
            document
                .to_mobject(2.0, &materials, &BTreeMap::new())
                .unwrap()
        } else {
            ThreeDModel::from_obj(source.as_bytes())
                .unwrap()
                .with_height(2.0)
                .into()
        };
        let mut stage = Stage::new();
        let root = stage.add(model);
        stage.add_to_scene(root).unwrap();
        stage.rotate(root, 0.35, [0.0, 1.0, 0.0], Some([0.0; 3]), None);
        stage
    }

    fn render(stage: &Stage, threads: usize) -> Vec<u8> {
        let background = LinearRgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 0.0,
        };
        let mut config = CameraConfig {
            resolution: (96, 64),
            background,
            light_source_position: [-3.0, 4.0, 5.0],
            ..CameraConfig::default()
        };
        config.frame.set_shape([4.5, 3.0]).unwrap();
        config
            .frame
            .set_euler_angles(Some(0.25), Some(0.5), None)
            .unwrap();
        let camera = Camera::new(config).unwrap();
        let mut renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
            frame: FrameConfig::new(
                Viewport {
                    width: 96,
                    height: 64,
                },
                ScreenMap {
                    scale: 16.0,
                    origin: [48.0, 32.0],
                    y_up: false,
                },
                background,
            ),
            tiling: Tiling {
                macro_tile: 32,
                fine_tile: 8,
            },
            engine: EngineIdentity::certified(),
            threads,
        })
        .unwrap();
        renderer.render_with_camera(stage, &camera).unwrap();
        renderer.frame().plane(0).to_vec()
    }

    // The first quad has area 2 and normal +Z. The second has area
    // 2*sqrt(2) and normal (1,0,1)/sqrt(2). Their shared normal is thus
    // (1,0,2)/sqrt(5), independently of how either quad is triangulated.
    let vertices = concat!(
        "v -1 -1 0\nv 0 -1 0\nv 0 1 0\nv -1 1 0\n",
        "v 1 -1 -1\nv 1 1 -1\n",
    );
    let faces = "usemtl A\nf 1 2 3 4\nusemtl B\nf 2 5 6 3\n";
    let explicit = format!(
        "{vertices}vn 0 0 1\nvn 1 0 1\nvn 1 0 2\n\
         usemtl A\nf 1//1 2//3 3//3 4//1\n\
         usemtl B\nf 2//3 5//2 6//2 3//3\n"
    );
    let smooth = format!("{vertices}s 7\n{faces}");
    let flat = format!("{vertices}s off\n{faces}");
    for materials in [false, true] {
        let expected = render(&stage(&explicit, materials), 1);
        assert!(expected.iter().any(|&byte| byte != 0));
        assert_ne!(
            render(&stage(&flat, materials), 1),
            expected,
            "the fixture must distinguish smoothing from flat shading"
        );
        let actual = stage(&smooth, materials);
        let bytes = actual.snapshot_bytes().unwrap();
        let mut restored = Stage::new();
        let decoded = Snapshot::from_bytes(&bytes, &restored).unwrap();
        restored.restore(&decoded.snapshot);
        assert_eq!(restored.snapshot_bytes().unwrap(), bytes);
        for threads in [1, 4, 16] {
            assert_eq!(
                render(&actual, threads),
                expected,
                "materials={materials}, threads={threads}"
            );
            assert_eq!(
                render(&restored, threads),
                expected,
                "restored materials={materials}, threads={threads}"
            );
        }
    }
}
