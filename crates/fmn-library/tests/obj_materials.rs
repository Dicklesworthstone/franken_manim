//! Independent metadata, geometry and material binding oracles.
use std::collections::BTreeMap;

use fmn_core::color::Srgb;
use fmn_library::obj_materials::{
    ObjAssetLimits, ObjDocument, ObjMaterial, ObjTextureOptions, parse_mtl,
};
use fmn_mobject::{
    ImageColorSpace, ImageResource, ImageSampler, ImageWrap, Mobject, RenderPrimitive, Snapshot,
    Stage,
};

const GEOMETRY: &str =
    "v -2 -1 0\nv 0 -1 0\nv 0 1 0\nv 2 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nvn 0 0 4\n";
fn image() -> ImageResource {
    ImageResource::rgba8(
        2,
        1,
        vec![255, 0, 0, 255, 0, 0, 255, 128],
        ImageColorSpace::Srgb,
        ImageSampler::default(),
    )
    .unwrap()
}
fn material() -> ObjMaterial {
    ObjMaterial {
        diffuse: Srgb {
            r: 0.2,
            g: 0.5,
            b: 0.7,
        },
        opacity: 0.6,
        texture: None,
        texture_options: ObjTextureOptions::default(),
    }
}

#[test]
fn geometry_authority_and_contiguous_painter_order_are_preserved() {
    let source = format!(
        "mtllib one.mtl two.mtl\n{GEOMETRY}usemtl A\nf 1/1/1 2/2/1 3/3/1\nusemtl B\nf 2/1 4/2 3/3\nusemtl A\nf -4/-4 -3/-3 -2/-2\n"
    );
    let doc = ObjDocument::parse(source.as_bytes()).unwrap();
    assert_eq!(
        doc.mesh(),
        &fmn_library::ObjMesh::parse(source.as_bytes()).unwrap()
    );
    assert_eq!(doc.libraries(), &["one.mtl", "two.mtl"]);
    assert_eq!(
        doc.runs()
            .iter()
            .map(|run| run.material.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("A"), Some("B"), Some("A")]
    );
    assert_eq!(doc.runs()[2].triangles, 2..3);
    assert_eq!(
        doc.material_names().into_iter().collect::<Vec<_>>(),
        vec!["A", "B"]
    );
}

#[test]
fn fan_triangulation_comments_and_redundant_bindings_do_not_shift_materials() {
    let source = format!(
        "{GEOMETRY}usemtl A\r\nf 1/1 2/2 4/3 3/4 # polygon\r\nusemtl A\nf 1 2 3\nusemtl off\nf 2 4 3\n"
    );
    let doc = ObjDocument::parse(source.as_bytes()).unwrap();
    assert_eq!(doc.runs().len(), 2);
    assert_eq!(doc.runs()[0].triangles, 0..3);
    assert_eq!(doc.runs()[1].material, None);
    assert_eq!(doc.runs()[1].triangles, 3..4);
}

#[test]
fn each_material_uses_one_shared_model_normalization() {
    let source = format!("{GEOMETRY}usemtl A\nf 1 2 3\nusemtl B\nf 2 4 3\n");
    let doc = ObjDocument::parse(source.as_bytes()).unwrap();
    let materials = BTreeMap::from([("A".into(), material()), ("B".into(), material())]);
    let model = doc.to_mobject(4.0, &materials, &BTreeMap::new()).unwrap();
    assert_eq!(model.submobjects.len(), 2);
    assert!(model.buffer.is_empty());
    let a = &model.submobjects[0];
    let b = &model.submobjects[1];
    assert_eq!(a.render_primitive, RenderPrimitive::TriangleMesh);
    assert_eq!(a.buffer.read(0, "point").unwrap(), vec![-4.0, -2.0, 0.0]);
    assert_eq!(b.buffer.read(1, "point").unwrap(), vec![4.0, 2.0, 0.0]);
    assert_eq!(a.buffer.read(1, "point"), b.buffer.read(0, "point"));
    assert_eq!(a.buffer.read(0, "rgba").unwrap(), vec![0.2, 0.5, 0.7, 0.6]);
    assert_eq!(
        a.buffer.read(0, "d_normal_point").unwrap(),
        vec![-4.0, -2.0, 1.0]
    );
    assert!(a.uniforms.depth_test);
    assert_eq!(a.uniforms.shading, fmn_library::MODEL_SHADING);
}

#[test]
fn texture_seams_preserve_corner_uvs_and_flip_v_exactly_once() {
    let source = format!("{GEOMETRY}usemtl A\nf 1/1/1 2/2/1 3/3/1\nf 1/4/1 3/3/1 4/2/1\n");
    let doc = ObjDocument::parse(source.as_bytes()).unwrap();
    let mut mat = material();
    mat.texture = Some("tiles.png".into());
    let resource = image();
    let model = doc
        .to_mobject(
            2.0,
            &BTreeMap::from([("A".into(), mat)]),
            &BTreeMap::from([("A".into(), resource.clone())]),
        )
        .unwrap();
    let child = &model.submobjects[0];
    assert_eq!(child.buffer.len(), 6);
    assert_eq!(child.image.as_ref(), Some(&resource));
    assert_eq!(child.buffer.read(0, "point"), child.buffer.read(3, "point"));
    assert_eq!(child.buffer.read(0, "im_coords").unwrap(), vec![0.0, 1.0]);
    assert_eq!(child.buffer.read(3, "im_coords").unwrap(), vec![0.0, 0.0]);
    assert_eq!(child.buffer.read(4, "im_coords").unwrap(), vec![1.0, 0.0]);
    assert_eq!(child.buffer.read(0, "opacity").unwrap(), vec![0.6]);
}

#[test]
fn undefined_material_missing_image_and_missing_uv_refuse() {
    let doc = ObjDocument::parse(format!("{GEOMETRY}usemtl A\nf 1 2 3\n").as_bytes()).unwrap();
    assert!(
        doc.to_mobject(2.0, &BTreeMap::new(), &BTreeMap::new())
            .err()
            .unwrap()
            .reason
            .contains("not defined")
    );
    let mut mat = material();
    mat.texture = Some("required.png".into());
    let materials = BTreeMap::from([("A".into(), mat)]);
    assert!(
        doc.to_mobject(2.0, &materials, &BTreeMap::new())
            .err()
            .unwrap()
            .reason
            .contains("not supplied")
    );
    assert!(
        doc.to_mobject(2.0, &materials, &BTreeMap::from([("A".into(), image())]))
            .err()
            .unwrap()
            .reason
            .contains("without UV")
    );
}

#[test]
fn untextured_models_remain_usable_without_any_library() {
    let doc = ObjDocument::parse(format!("{GEOMETRY}f 1 2 3\n").as_bytes()).unwrap();
    let model = doc
        .to_mobject(3.0, &BTreeMap::new(), &BTreeMap::new())
        .unwrap();
    assert_eq!(model.submobjects.len(), 1);
    assert!(model.submobjects[0].image.is_none());
    let plain: fmn_mobject::Mobject =
        fmn_library::ThreeDModel::from_obj(format!("{GEOMETRY}f 1 2 3\n").as_bytes())
            .unwrap()
            .into();
    let imported = &model.submobjects[0].buffer;
    assert_eq!(plain.buffer.schema(), imported.schema());
    for key in ["point", "d_normal_point", "rgba"] {
        assert_eq!(plain.buffer.read_column(key), imported.read_column(key));
    }
}

#[test]
fn native_mtl_rgb_opacity_map_precedence_and_spaced_paths() {
    let materials = parse_mtl(b"# appearance\nnewmtl wood\nKd .2 .4 .6\nd .8\nTr .25\nmap_Ka fallback.png\nmap_Kd textures/wood grain.png\nmap_Ka ignored.png\nnewmtl plain\nKd 1 0 0\n", &ObjAssetLimits::default()).unwrap();
    assert_eq!(
        materials["wood"].diffuse,
        Srgb {
            r: 0.2,
            g: 0.4,
            b: 0.6
        }
    );
    assert_eq!(materials["wood"].opacity, 0.75);
    assert_eq!(
        materials["wood"].texture.as_deref(),
        Some("textures/wood grain.png")
    );
    assert_eq!(materials["plain"].texture, None);
    assert_eq!(materials["plain"].opacity, 1.0);
}

#[test]
fn malformed_mtl_and_unsupported_diffuse_options_are_named_errors() {
    for source in [
        "Kd 1 0 0\n",
        "newmtl\n",
        "newmtl A\nKd nan 0 0\n",
        "newmtl A\nKd 1 2 0\n",
        "newmtl A\nd -1\n",
        "newmtl A\nKd spectral a.rfl\n",
        "newmtl A\nmap_Kd -t 2 2 1 a.png\n",
        "newmtl A\nmap_Kd\n",
    ] {
        assert!(
            parse_mtl(source.as_bytes(), &ObjAssetLimits::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn import_budgets_precede_unbounded_geometry_and_metadata_allocation() {
    let tiny = ObjAssetLimits {
        max_source_bytes: 8,
        ..ObjAssetLimits::default()
    };
    assert!(
        ObjDocument::parse_with_limits(GEOMETRY.as_bytes(), &tiny)
            .err()
            .unwrap()
            .reason
            .contains("byte budget")
    );
    let tiny = ObjAssetLimits {
        max_materials: 1,
        ..ObjAssetLimits::default()
    };
    assert!(ObjDocument::parse_with_limits(b"mtllib a.mtl b.mtl", &tiny).is_err());
    assert!(parse_mtl(b"newmtl A\nnewmtl B\n", &tiny).is_err());
    let tiny = ObjAssetLimits {
        max_line_bytes: 4,
        ..ObjAssetLimits::default()
    };
    assert!(ObjDocument::parse_with_limits(b"v 1 2 3", &tiny).is_err());
    let tiny = ObjAssetLimits {
        max_name_bytes: 1,
        ..ObjAssetLimits::default()
    };
    assert!(ObjDocument::parse_with_limits(b"usemtl AB", &tiny).is_err());
    assert!(ObjDocument::parse(b"v 0 0 0\0").is_err());
    assert!(ObjDocument::parse(b"f 1 2 \\\n3").is_err());
}

#[test]
fn failed_conversion_never_changes_a_parsed_document_or_bound_resources() {
    let doc = ObjDocument::parse(format!("{GEOMETRY}f 1 2 3\n").as_bytes()).unwrap();
    let mesh = doc.mesh().clone();
    for height in [f64::NAN, 0.0, -1.0, f64::INFINITY, f64::MAX] {
        assert!(
            doc.to_mobject(height, &BTreeMap::new(), &BTreeMap::new())
                .is_err()
        );
    }
    assert_eq!(doc.mesh(), &mesh);
    assert!(
        doc.to_mobject(3.0, &BTreeMap::new(), &BTreeMap::new())
            .is_ok()
    );
    let large = ObjDocument::parse(b"v -1e308 0 0\nv 1e308 0 0\nv 0 1 0\nf 1 2 3\n").unwrap();
    assert!(
        large
            .to_mobject(3.0, &BTreeMap::new(), &BTreeMap::new())
            .is_err()
    );
}

#[test]
fn map_options_keep_defaults_signed_values_and_exact_filename_spelling() {
    let materials = parse_mtl(
        b"newmtl A\nmap_Kd -o -.25 -s -2 -clamp on maps/two  spaces.png\n\
          newmtl B\nmap_Kd -s 2 .5 1 -o .125 -.25 0 -clamp off ./-label.png\n\
          newmtl C\nmap_Kd -s 2 -s .5 -o 3 4 -o -1 ./4 5.png\n",
        &ObjAssetLimits::default(),
    )
    .unwrap();
    assert_eq!(
        materials["A"].texture.as_deref(),
        Some("maps/two  spaces.png")
    );
    assert_eq!(
        materials["A"].texture_options,
        ObjTextureOptions {
            offset: [-0.25, 0.0],
            scale: [-2.0, 1.0],
            clamp: Some(true),
        }
    );
    assert_eq!(materials["B"].texture.as_deref(), Some("./-label.png"));
    assert_eq!(
        materials["B"].texture_options,
        ObjTextureOptions {
            offset: [0.125, -0.25],
            scale: [2.0, 0.5],
            clamp: Some(false),
        }
    );
    assert_eq!(materials["C"].texture.as_deref(), Some("./4 5.png"));
    assert_eq!(materials["C"].texture_options.offset, [-1.0, 0.0]);
    assert_eq!(materials["C"].texture_options.scale, [0.5, 1.0]);
    assert_eq!(materials["C"].texture_options.clamp, None);
}

#[test]
fn selected_map_replaces_its_complete_transform_and_addressing_policy() {
    let materials = parse_mtl(
        b"newmtl A\nmap_Ka -o 2 3 -clamp on fallback.png\n\
          map_Kd -s 2 selected.png\nmap_Ka -s 8 -clamp on ignored.png\n\
          newmtl B\nmap_Ka -o -.5 .25 -clamp on fallback.png\n\
          newmtl C\nmap_Kd -s 2 -clamp on first.png\nmap_Kd final.png\n",
        &ObjAssetLimits::default(),
    )
    .unwrap();
    assert_eq!(materials["A"].texture.as_deref(), Some("selected.png"));
    assert_eq!(
        materials["A"].texture_options,
        ObjTextureOptions {
            scale: [2.0, 1.0],
            ..ObjTextureOptions::default()
        }
    );
    assert_eq!(materials["B"].texture.as_deref(), Some("fallback.png"));
    assert_eq!(materials["B"].texture_options.offset, [-0.5, 0.25]);
    assert_eq!(materials["B"].texture_options.clamp, Some(true));
    assert_eq!(materials["C"].texture.as_deref(), Some("final.png"));
    assert_eq!(materials["C"].texture_options, ObjTextureOptions::default());
}

#[test]
fn malformed_map_options_report_the_source_line_before_binding_assets() {
    for (map, expected) in [
        ("-o tiles.png", "numeric U"),
        ("-s -clamp on tiles.png", "numeric U"),
        ("-s 1 1 1 2 tiles.png", "at most three"),
        ("-o 0 0 1 tiles.png", "depth component"),
        ("-s 1 1 0 tiles.png", "depth component"),
        ("-o nan tiles.png", "finite"),
        ("-s 1 inf tiles.png", "finite"),
        ("-o 0 0 -inf tiles.png", "finite"),
        ("-clamp", "on or off"),
        ("-clamp yes tiles.png", "on or off"),
        ("-clamp on -mm 0 1 tiles.png", "unsupported"),
        ("-s 1 -t 0 0 0 tiles.png", "unsupported"),
        ("-unknown 1 tiles.png", "unsupported"),
        ("-s 1 1 1", "empty"),
        ("-clamp on", "empty"),
    ] {
        let source = format!("# material\nnewmtl A\nmap_Kd {map}\n");
        let failure = parse_mtl(source.as_bytes(), &ObjAssetLimits::default()).unwrap_err();
        assert_eq!(failure.line, 3, "{map}");
        assert!(failure.reason.contains(expected), "{map}: {failure}");
    }
    let limits = ObjAssetLimits {
        max_name_bytes: 5,
        ..ObjAssetLimits::default()
    };
    assert!(parse_mtl(b"newmtl A\nmap_Kd -s 2 -o 0 -clamp off a.png", &limits).is_ok());
    assert!(
        parse_mtl(b"newmtl A\nmap_Kd -s 2 -o 0 -clamp off ab.png", &limits)
            .unwrap_err()
            .reason
            .contains("name/path")
    );
    let limits = ObjAssetLimits {
        max_line_bytes: 20,
        ..ObjAssetLimits::default()
    };
    assert!(
        parse_mtl(b"newmtl A\nmap_Kd -o 0 -o 0 -o 0 a.png", &limits)
            .unwrap_err()
            .reason
            .contains("line byte budget")
    );
}

#[test]
fn map_transform_is_applied_before_v_flip_and_preserves_per_corner_seams() {
    let source = format!("{GEOMETRY}usemtl A\nf 1/1/1 2/2/1 3/3/1\nf 1/4/1 3/3/1 4/2/1\n");
    let doc = ObjDocument::parse(source.as_bytes()).unwrap();
    let materials = parse_mtl(
        b"newmtl A\nmap_Kd -s -2 .5 -o .25 -.125 -clamp on tiles.png\n",
        &ObjAssetLimits::default(),
    )
    .unwrap();
    let resource = image();
    let model = doc
        .to_mobject(
            2.0,
            &materials,
            &BTreeMap::from([("A".into(), resource.clone())]),
        )
        .unwrap();
    let child = &model.submobjects[0];
    let expected: [[f32; 2]; 6] = [
        [0.25, 1.125],
        [-1.75, 1.125],
        [-1.75, 0.625],
        [0.25, 0.625],
        [-1.75, 0.625],
        [-1.75, 1.125],
    ];
    for (index, uv) in expected.iter().enumerate() {
        assert_eq!(child.buffer.read(index, "im_coords").unwrap(), uv.to_vec());
    }
    let bound = child.image.as_ref().unwrap();
    assert_eq!(bound.sampler().wrap_u, ImageWrap::ClampToEdge);
    assert_eq!(bound.sampler().wrap_v, ImageWrap::ClampToEdge);
    assert_eq!(bound.pixels().as_ptr(), resource.pixels().as_ptr());
    assert_eq!(bound.content_digest(), resource.content_digest());
    assert_eq!(resource.sampler(), ImageSampler::default());
}

#[test]
fn unspecified_map_sampler_is_preserved_but_explicit_off_overrides_both_axes() {
    let doc =
        ObjDocument::parse(format!("{GEOMETRY}usemtl A\nf 1/1 2/2 3/3\n").as_bytes()).unwrap();
    let sampler = ImageSampler {
        wrap_u: ImageWrap::MirroredRepeat,
        wrap_v: ImageWrap::ClampToEdge,
    };
    let resource = image().with_sampler(sampler);
    for (statement, expected) in [
        ("-s 2 tiles.png", sampler),
        ("-clamp off tiles.png", ImageSampler::default()),
    ] {
        let materials = parse_mtl(
            format!("newmtl A\nmap_Kd {statement}\n").as_bytes(),
            &ObjAssetLimits::default(),
        )
        .unwrap();
        let model = doc
            .to_mobject(
                2.0,
                &materials,
                &BTreeMap::from([("A".into(), resource.clone())]),
            )
            .unwrap();
        assert_eq!(
            model.submobjects[0].image.as_ref().unwrap().sampler(),
            expected
        );
    }
    assert_eq!(resource.sampler(), sampler);
}

#[test]
fn zero_scale_is_valid_and_transformed_overflow_refuses_without_mutation() {
    let doc =
        ObjDocument::parse(format!("{GEOMETRY}usemtl A\nf 1/1 2/2 3/3\n").as_bytes()).unwrap();
    let resource = image();
    let images = BTreeMap::from([("A".into(), resource.clone())]);
    let mut materials = parse_mtl(
        b"newmtl A\nmap_Kd -s 0 0 -o .25 .75 tiles.png\n",
        &ObjAssetLimits::default(),
    )
    .unwrap();
    let model = doc.to_mobject(2.0, &materials, &images).unwrap();
    for index in 0..3 {
        assert_eq!(
            model.submobjects[0]
                .buffer
                .read(index, "im_coords")
                .unwrap(),
            [0.25; 2]
        );
    }
    let mesh = doc.mesh().clone();
    for invalid in [f64::NAN, f64::INFINITY] {
        materials.get_mut("A").unwrap().texture_options.scale[0] = invalid;
        assert!(
            doc.to_mobject(2.0, &materials, &images)
                .err()
                .unwrap()
                .reason
                .contains("finite")
        );
    }
    materials.get_mut("A").unwrap().texture_options.scale[0] = f64::MAX;
    assert!(
        doc.to_mobject(2.0, &materials, &images)
            .err()
            .unwrap()
            .reason
            .contains("transformed texture coordinates")
    );
    materials.get_mut("A").unwrap().texture_options.offset[0] = f64::MAX;
    assert!(doc.to_mobject(2.0, &materials, &images).is_err());
    let overflowing =
        ObjDocument::parse(format!("{GEOMETRY}vt 2 0\nusemtl A\nf 1/5 2/2 3/3\n").as_bytes())
            .unwrap();
    materials.get_mut("A").unwrap().texture_options.offset[0] = 0.0;
    assert!(
        overflowing
            .to_mobject(2.0, &materials, &images)
            .err()
            .unwrap()
            .reason
            .contains("transformed texture coordinates")
    );
    assert_eq!(doc.mesh(), &mesh);
    assert_eq!(images["A"], resource);
}

const MAP_QUAD: &str = "v -2 -1 0\nv 2 -1 0\nv 2 1 0\nv -2 1 0\n\
    vt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nvn 0 0 1\nusemtl A\nf 1/1/1 2/2/1 3/3/1 4/4/1\n";

fn map_image(sampler: ImageSampler) -> ImageResource {
    ImageResource::rgba8(
        2,
        2,
        vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 128,
        ],
        ImageColorSpace::Srgb,
        sampler,
    )
    .unwrap()
}

fn independently_mapped_quad(sampler: ImageSampler) -> Mobject {
    use fmn_mobject::record::RecordBuffer;
    use fmn_mobject::uniforms::Uniforms;
    let corners = [
        ([-2.0, -1.0, 0.0], [-0.25, 0.875]),
        ([2.0, -1.0, 0.0], [1.75, 0.875]),
        ([2.0, 1.0, 0.0], [1.75, 0.375]),
        ([-2.0, -1.0, 0.0], [-0.25, 0.875]),
        ([2.0, 1.0, 0.0], [1.75, 0.375]),
        ([-2.0, 1.0, 0.0], [-0.25, 0.375]),
    ];
    let mut buffer = RecordBuffer::new(fmn_library::textured_surface_schema(), 6).unwrap();
    for (index, (point, uv)) in corners.iter().enumerate() {
        buffer.write(index, "point", point);
        buffer.write(index, "d_normal_point", &[point[0], point[1], 1.0]);
        buffer.write(index, "im_coords", uv);
        buffer.write(index, "opacity", &[1.0]);
    }
    Mobject::from_buffer(buffer)
        .with_uniforms(Uniforms {
            shading: fmn_library::MODEL_SHADING,
            depth_test: true,
            ..Uniforms::default()
        })
        .with_image_resource(map_image(sampler))
        .with_render_primitive(RenderPrimitive::TriangleMesh)
}

fn map_camera_renderer(threads: usize) -> (fmn_render::Camera, fmn_render::RetainedFrameRenderer) {
    use fmn_core::color::LinearRgba;
    use fmn_render::{
        Camera, CameraConfig, EngineIdentity, FrameConfig, RetainedFrameRenderer,
        RetainedFrameRendererConfig, ScreenMap, Tiling, Viewport,
    };
    let background = LinearRgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 0.0,
    };
    let camera = Camera::new(CameraConfig {
        resolution: (96, 64),
        background,
        ..CameraConfig::default()
    })
    .unwrap();
    let renderer = RetainedFrameRenderer::new(RetainedFrameRendererConfig {
        frame: FrameConfig::new(
            Viewport {
                width: 96,
                height: 64,
            },
            ScreenMap {
                scale: 8.0,
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
    (camera, renderer)
}

fn render_model(stage: &Stage, threads: usize) -> Vec<u8> {
    let (camera, mut renderer) = map_camera_renderer(threads);
    renderer.render_with_camera(stage, &camera).unwrap();
    renderer.frame().plane(0).to_vec()
}

#[test]
fn imported_map_options_match_independent_camera_pixels_and_durable_restore() {
    let doc = ObjDocument::parse(MAP_QUAD.as_bytes()).unwrap();
    let mut policy_frames = Vec::new();
    for (clamp, wrap) in [("off", ImageWrap::Repeat), ("on", ImageWrap::ClampToEdge)] {
        let materials = parse_mtl(
            format!("newmtl A\nmap_Kd -s 2 .5 1 -o -.25 .125 0 -clamp {clamp} tiles.png\n")
                .as_bytes(),
            &ObjAssetLimits::default(),
        )
        .unwrap();
        let model = doc
            .to_mobject(
                2.0,
                &materials,
                &BTreeMap::from([("A".into(), map_image(ImageSampler::default()))]),
            )
            .unwrap();
        let mut actual = Stage::new();
        let root = actual.add(model);
        actual.add_to_scene(root).unwrap();
        let mut oracle = Stage::new();
        let root = oracle.add(independently_mapped_quad(ImageSampler {
            wrap_u: wrap,
            wrap_v: wrap,
        }));
        oracle.add_to_scene(root).unwrap();
        let bytes = actual.snapshot_bytes().unwrap();
        let mut restored = Stage::new();
        let decoded = Snapshot::from_bytes(&bytes, &restored).unwrap();
        restored.restore(&decoded.snapshot);
        assert_eq!(restored.snapshot_bytes().unwrap(), bytes);
        let expected = render_model(&oracle, 1);
        assert!(expected.iter().any(|&byte| byte != 0));
        for threads in [1, 4, 16] {
            assert_eq!(
                render_model(&actual, threads),
                expected,
                "{clamp}, {threads}"
            );
            assert_eq!(
                render_model(&restored, threads),
                expected,
                "restored {clamp}, {threads}"
            );
        }
        policy_frames.push(expected);
    }
    assert_ne!(
        policy_frames[0], policy_frames[1],
        "addressing must affect rendered pixels"
    );
}

#[test]
fn changing_only_sampler_refreshes_retained_pixels_and_snapshot_restore() {
    let repeat = ImageSampler::default();
    let clamp = ImageSampler {
        wrap_u: ImageWrap::ClampToEdge,
        wrap_v: ImageWrap::ClampToEdge,
    };
    let mut oracle = Stage::new();
    let oracle_mob = oracle.add(independently_mapped_quad(clamp));
    oracle.add_to_scene(oracle_mob).unwrap();
    let clamped_pixels = render_model(&oracle, 1);
    for threads in [1, 4, 16] {
        let mut stage = Stage::new();
        let mob = stage.add(independently_mapped_quad(repeat));
        stage.add_to_scene(mob).unwrap();
        let before = stage.snapshot();
        let before_bytes = stage.snapshot_bytes().unwrap();
        let resource = stage.get(mob).unwrap().image_resource().unwrap().clone();
        let (camera, mut renderer) = map_camera_renderer(threads);
        renderer.render_with_camera(&stage, &camera).unwrap();
        let repeated_pixels = renderer.frame().plane(0).to_vec();
        assert_ne!(repeated_pixels, clamped_pixels);
        assert!(
            stage
                .set_image_resource(mob, Some(resource.with_sampler(clamp)))
                .unwrap()
        );
        assert_ne!(stage.snapshot_bytes().unwrap(), before_bytes);
        renderer.render_with_camera(&stage, &camera).unwrap();
        assert_eq!(renderer.frame().plane(0), clamped_pixels);
        stage.restore(&before);
        assert_eq!(stage.snapshot_bytes().unwrap(), before_bytes);
        renderer.render_with_camera(&stage, &camera).unwrap();
        assert_eq!(renderer.frame().plane(0), repeated_pixels);
    }
}
