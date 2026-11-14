//! Real native frame output for smooth OBJ geometry and material partitions.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use fmn::prelude::*;
use fmn_codec::{PngLimits, decode_png};
use fmn_config::config::{DeterminismMode, ThreadPolicy};
use fmn_library::{ObjAssetLimits, ObjDocument, ThreeDModel, parse_mtl};
use fmn_platform::fs::{FileSystem, VirtualFs};

const VERTICES: &str = concat!(
    "v -1 -1 0\nv 0 -1 0\nv 0 1 0\nv -1 1 0\n",
    "v 1 -1 -1\nv 1 1 -1\n",
);
const FACES: &str = "usemtl A\nf 1 2 3 4\nusemtl B\nf 2 5 6 3\n";
const EXPLICIT_NORMALS: &str = concat!(
    "vn 0 0 1\nvn 1 0 1\nvn 1 0 2\n",
    "usemtl A\nf 1//1 2//3 3//3 4//1\n",
    "usemtl B\nf 2//3 5//2 6//2 3//3\n",
);

struct ModelScene {
    model: Option<Mobject>,
}

impl SceneConstruct for ModelScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let model = stage.add(self.model.take().expect("one scene construction"))?;
        stage.play(
            model
                .animate()
                .set_anim_args(AnimateArgs {
                    run_time_frames: Some(3),
                    rate_func: Some(fmn_core::rate::linear),
                    ..AnimateArgs::default()
                })?
                .rotate_about(0.35, [0.0, 1.0, 0.0], Some([0.0; 3]))?,
        )?;
        Ok(())
    }
}

fn model(source: &str, materials: bool) -> Result<Mobject, String> {
    if materials {
        let document = ObjDocument::parse(source.as_bytes()).map_err(|error| error.to_string())?;
        if document.runs().len() != 2 {
            return Err("the smoothing fixture lost its material boundary".into());
        }
        let materials = parse_mtl(
            b"newmtl A\nKd .2 .65 .9\nnewmtl B\nKd .9 .55 .15\n",
            &ObjAssetLimits::default(),
        )
        .map_err(|error| error.to_string())?;
        document
            .to_mobject(2.0, &materials, &BTreeMap::new())
            .map_err(|error| error.to_string())
    } else {
        Ok(ThreeDModel::from_obj(source.as_bytes())
            .map_err(|error| error.to_string())?
            .with_height(2.0)
            .with_color(BLUE_C, 1.0)
            .into())
    }
}

fn render(source: &str, materials: bool, threads: usize) -> Result<Vec<Vec<u8>>, String> {
    let fs = Arc::new(VirtualFs::new());
    let mut options = RenderOptions::new("/obj-smoothing").map_err(|error| error.to_string())?;
    options.config.camera.resolution = (64, 48);
    options.config.camera.fps = 8;
    options.config.camera.background_opacity = 0.0;
    options.config.sizes.frame_height = 3.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    options.frames_in_flight = 2;
    let mut camera = options.camera_config().map_err(|error| error.to_string())?;
    camera
        .frame
        .set_euler_angles(Some(0.25), Some(0.5), None)
        .map_err(|error| error.to_string())?;
    camera.light_source_position = [-3.0, 4.0, 5.0];
    options.camera = Some(camera);
    let report = render_with_fs(
        &mut ModelScene {
            model: Some(model(source, materials)?),
        },
        options,
        fs.clone(),
    )
    .map_err(|error| error.to_string())?;
    let pipeline = report
        .frame_pipeline
        .ok_or("OBJ render omitted its frame pipeline")?;
    if report.artifact.frame_count != 3
        || report.scene.time.frames() != 3
        || pipeline.submitted != 3
        || pipeline.emitted != 3
        || pipeline.outstanding_slots != 0
    {
        return Err("OBJ smoothing changed animation timing or ordered publication".into());
    }
    (0..3)
        .map(|frame| {
            fs.read(&Path::new("/obj-smoothing").join(format!("frame_{frame:06}.png")))
                .map_err(|error| error.to_string())
        })
        .collect()
}

pub struct Witness {
    pub first: Vec<u8>,
    pub last: Vec<u8>,
    pub frames: u64,
}

/// Source polygons have different areas. The shared corners' normal is
/// proportional to (1, 0, 2), not the average of the two unit face normals.
/// The oracle states those normals explicitly and bypasses their generation.
pub fn verify() -> Result<Witness, String> {
    let smooth = format!("{VERTICES}s 7\n{FACES}");
    let hard = format!("{VERTICES}s off\n{FACES}");
    let explicit = format!("{VERTICES}{EXPLICIT_NORMALS}");
    let mut witness = None;
    let mut frames = 0;
    for materials in [false, true] {
        let expected = render(&explicit, materials, 1)?;
        let flat = render(&hard, materials, 1)?;
        frames += 6;
        if expected == flat {
            return Err("the fixture cannot distinguish smooth normals from flat faces".into());
        }
        if expected[0] == expected[2] {
            return Err("the fixture failed to animate the model's geometry and normals".into());
        }
        let first =
            decode_png(&expected[0], &PngLimits::default()).map_err(|error| error.to_string())?;
        if first
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|pixel| pixel[3] > 0)
            .count()
            < 64
        {
            return Err("the explicit-normal model did not cover enough visible pixels".into());
        }
        for threads in [1, 4, 16] {
            let actual = render(&smooth, materials, threads)?;
            frames += 3;
            if actual != expected {
                return Err(format!(
                    "generated OBJ normals differ from explicit normals: materials={materials}, threads={threads}"
                ));
            }
        }
        if materials {
            witness = Some((expected[0].clone(), expected[2].clone()));
        }
    }
    let (first, last) = witness.ok_or("no material smoothing witness rendered")?;
    Ok(Witness {
        first,
        last,
        frames,
    })
}
