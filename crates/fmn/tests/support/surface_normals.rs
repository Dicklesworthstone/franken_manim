//! Native shaded output compared with independently authored affine normals.

use std::path::Path;
use std::sync::Arc;

use fmn::prelude::*;
use fmn_codec::{PngLimits, decode_png};
use fmn_mobject::{Placement, RecordBuffer, RecordSchema, RenderPrimitive};
use fmn_platform::fs::{FileSystem, VirtualFs};

#[derive(Clone, Copy)]
struct Case {
    name: &'static str,
    placement: Placement,
    slope: f64,
    expected_offset: Vec3,
}

const CASES: [Case; 4] = [
    Case {
        name: "stretch",
        placement: Placement::new(
            [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.5]],
            [0.0; 3],
        ),
        slope: 1.0,
        expected_offset: [-0.125, 0.0, 0.5],
    },
    Case {
        name: "reflection",
        placement: Placement::new(
            [[-2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.5]],
            [0.0; 3],
        ),
        slope: 1.0,
        expected_offset: [0.125, 0.0, 0.5],
    },
    Case {
        name: "shear",
        placement: Placement::new(
            [[1.0, 0.0, 1.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            [0.0; 3],
        ),
        slope: 0.0,
        expected_offset: [0.0, 0.0, 0.25],
    },
    Case {
        name: "rank-two-collapse",
        placement: Placement::new(
            [[2.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 0.0]],
            [0.0; 3],
        ),
        slope: 1.0,
        expected_offset: [0.0; 3],
    },
];

#[derive(Clone, Copy, Debug)]
enum Route {
    Authored,
    ForwardNormal,
    Retained,
    Baked,
    LiveView,
}

fn object(case: Case, triangles: bool, route: Route) -> Result<Mobject, String> {
    let schema = RecordSchema::new(
        &[("point", 3), ("d_normal_point", 3), ("rgba", 4)],
        &["point"],
        &["point", "d_normal_point"],
    )
    .map_err(|error| error.to_string())?;
    let indices: &[usize] = if triangles {
        &[0, 2, 1, 1, 2, 3]
    } else {
        &[0, 1, 2, 3]
    };
    let corners = [(-0.5, -0.75), (-0.5, 0.75), (0.5, -0.75), (0.5, 0.75)];
    let mut buffer = RecordBuffer::new(schema, indices.len()).map_err(|error| error.to_string())?;
    for (row, &corner) in indices.iter().enumerate() {
        let (u, v) = corners[corner];
        let local = [u, v, case.slope * u];
        let source_offset = [-0.25 * case.slope, 0.0, 0.25];
        let (point, offset) = match route {
            Route::Authored => (case.placement.apply_point(local), case.expected_offset),
            Route::ForwardNormal => (
                case.placement.apply_point(local),
                case.placement.apply_vector(source_offset),
            ),
            Route::Retained | Route::Baked | Route::LiveView => (local, source_offset),
        };
        let control: Vec3 = std::array::from_fn(|axis| point[axis] + offset[axis]);
        #[allow(clippy::cast_possible_truncation)]
        {
            buffer.write(row, "point", &point.map(|value| value as f32));
            buffer.write(row, "d_normal_point", &control.map(|value| value as f32));
        }
        buffer.write(row, "rgba", &[0.3, 0.55, 0.8, 1.0]);
    }
    let primitive = if triangles {
        RenderPrimitive::TriangleMesh
    } else {
        RenderPrimitive::SurfaceGrid { resolution: (2, 2) }
    };
    let mut object = Mobject::from_buffer(buffer).with_render_primitive(primitive);
    object.uniforms.shading = [0.3, 0.2, 0.4];
    Ok(object)
}

struct SurfaceScene {
    object: Option<Mobject>,
    case: Case,
    route: Route,
    view_attached: bool,
}

impl SceneConstruct for SurfaceScene {
    fn construct(&mut self, stage: &mut Stage<'_>) -> fmn::Result<()> {
        let mob = stage.add(self.object.take().expect("one scene construction"))?;
        let view = matches!(self.route, Route::LiveView)
            .then(|| stage.get_mut(mob).unwrap().buffer.export_view(false));
        if matches!(self.route, Route::Retained | Route::Baked | Route::LiveView) {
            stage.apply_affine(mob, self.case.placement);
        }
        if matches!(self.route, Route::Baked) {
            stage.bake_placement(mob)?;
        }
        self.view_attached =
            view.is_none_or(|view| view.is_attached_to(&stage.get(mob).unwrap().buffer));
        Ok(())
    }
}

fn render(case: Case, triangles: bool, route: Route, threads: u32) -> Result<Vec<u8>, String> {
    let fs = Arc::new(VirtualFs::new());
    let mut options = RenderOptions::new("/surface-normals").map_err(|error| error.to_string())?;
    options.config.camera.resolution = (64, 48);
    options.config.camera.fps = 8;
    options.config.camera.background_opacity = 0.0;
    options.config.sizes.frame_height = 3.0;
    options.config.determinism.mode = DeterminismMode::Certified;
    options.config.render.threads = ThreadPolicy::Fixed(threads);
    let mut camera = options.camera_config().map_err(|error| error.to_string())?;
    camera
        .frame
        .set_euler_angles(Some(0.25), Some(0.5), None)
        .map_err(|error| error.to_string())?;
    camera.light_source_position = [-3.0, 4.0, 5.0];
    options.camera = Some(camera);
    let mut scene = SurfaceScene {
        object: Some(object(case, triangles, route)?),
        case,
        route,
        view_attached: false,
    };
    let report =
        render_with_fs(&mut scene, options, fs.clone()).map_err(|error| error.to_string())?;
    if report.artifact.frame_count != 1 || !scene.view_attached {
        return Err("surface normal materialization lost its frame or detached a live view".into());
    }
    fs.read(Path::new("/surface-normals/frame_000000.png"))
        .map_err(|error| error.to_string())
}

pub struct Witness {
    pub first: Vec<u8>,
    pub last: Vec<u8>,
    pub frames: u64,
}

/// The oracle directly states the plane's post-transform normal. It never
/// calls the new normal-transform code. A singular map has no normal and uses
/// the renderer's existing zero-normal fallback. A forward normal is rendered
/// separately to prove that each fixture can detect the original defect.
pub fn verify() -> Result<Witness, String> {
    let mut frames = 0;
    let mut witness = None;
    for case in CASES {
        for triangles in [false, true] {
            let expected = render(case, triangles, Route::Authored, 1)?;
            let incorrect = render(case, triangles, Route::ForwardNormal, 1)?;
            frames += 2;
            if expected == incorrect {
                return Err(format!(
                    "{} fixture cannot detect a forward-mapped normal",
                    case.name
                ));
            }
            let image =
                decode_png(&expected, &PngLimits::default()).map_err(|error| error.to_string())?;
            if image
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel[3] > 0)
                .count()
                < 64
            {
                return Err(format!(
                    "{} surface did not produce visible output",
                    case.name
                ));
            }
            for route in [Route::Retained, Route::Baked, Route::LiveView] {
                for threads in [1, 4, 16] {
                    let actual = render(case, triangles, route, threads)?;
                    frames += 1;
                    if actual != expected {
                        return Err(format!(
                            "{} normals differ from the authored oracle: triangles={triangles}, route={route:?}, threads={threads}",
                            case.name
                        ));
                    }
                }
            }
            if witness.is_none() {
                witness = Some((expected, incorrect));
            }
        }
    }
    let (first, last) = witness.ok_or("no surface normal witness rendered")?;
    Ok(Witness {
        first,
        last,
        frames,
    })
}
