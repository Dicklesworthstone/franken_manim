#![feature(test)]
#![forbid(unsafe_code)]

extern crate test;

use fmn_core::color::LinearRgba;
use fmn_mobject::{Mobject, RecordBuffer, RecordSchema, Stage};
use fmn_render::{
    Binning, EngineIdentity, EngineKind, FrameConfig, FrameJob, MonoPiece, MonoTable, RenderPlan,
    RowScratch, ScreenMap, Tier, Tiling, Viewport,
};
use std::hint::black_box;
use test::Bencher;

const WIDTH: u32 = 512;
const HEIGHT: u32 = 512;
const LAYERS: usize = 32;

fn rectangle(x0: f64, y0: f64, x1: f64, y1: f64, fill: [f32; 4]) -> Mobject {
    let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1], [x0, y0]];
    let mut points = vec![[corners[0][0], corners[0][1], 0.0]];
    for pair in corners.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        points.push([0.5 * (a[0] + b[0]), 0.5 * (a[1] + b[1]), 0.0]);
        points.push([b[0], b[1], 0.0]);
    }

    let mut buffer = RecordBuffer::new(RecordSchema::vmobject(), points.len());
    for (i, point) in points.iter().enumerate() {
        buffer.write(
            i,
            "point",
            &[point[0] as f32, point[1] as f32, point[2] as f32],
        );
        buffer.write(i, "fill_rgba", &fill);
    }
    Mobject::from_buffer(buffer)
}

struct Fixture {
    plan: RenderPlan,
    mono: MonoTable,
    binning: Binning,
    config: FrameConfig,
}

impl Fixture {
    fn new() -> Self {
        let mut stage = Stage::new();
        for layer in 0..LAYERS {
            let t = layer as f32 / LAYERS as f32;
            let mob = stage.add(rectangle(
                -1.0,
                -1.0,
                f64::from(WIDTH) + 1.0,
                f64::from(HEIGHT) + 1.0,
                [0.2 + 0.6 * t, 0.7 - 0.5 * t, 0.3 + 0.4 * t, 0.08],
            ));
            stage.add_to_scene(mob).expect("live benchmark layer");
        }

        let config = FrameConfig::new(
            Viewport {
                width: WIDTH,
                height: HEIGHT,
            },
            ScreenMap::default(),
            LinearRgba {
                r: 0.02,
                g: 0.02,
                b: 0.02,
                a: 1.0,
            },
        );
        let mut plan = RenderPlan::new();
        plan.sync(&stage, 0);
        let mono = MonoTable::build(&plan, config.map);
        let binning = Binning::build(
            &plan,
            config.viewport,
            Tiling {
                macro_tile: 128,
                fine_tile: 16,
            },
            config.map,
        );
        Self {
            plan,
            mono,
            binning,
            config,
        }
    }

    fn render(&self, identity: EngineIdentity) {
        let job =
            FrameJob::with_identity(&self.plan, &self.mono, &self.binning, self.config, identity)
                .expect("coherent benchmark artifacts");
        black_box(job.render(1).expect("benchmark render"));
    }
}

#[bench]
fn layered_translucent_compositor(bench: &mut Bencher) {
    let fixture = Fixture::new();
    bench.iter(|| {
        fixture.render(EngineIdentity::certified());
    });
}

#[bench]
fn layered_translucent_compositor_compiled_tier(bench: &mut Bencher) {
    let fixture = Fixture::new();
    bench.iter(|| {
        fixture.render(EngineIdentity {
            tier: Tier::COMPILED,
            ..EngineIdentity::certified()
        });
    });
}

#[bench]
fn layered_translucent_compositor_fast_scalar(bench: &mut Bencher) {
    let fixture = Fixture::new();
    bench.iter(|| {
        fixture.render(EngineIdentity {
            engine: EngineKind::FastCpu,
            ..EngineIdentity::certified()
        });
    });
}

#[bench]
fn layered_translucent_compositor_fast_compiled_tier(bench: &mut Bencher) {
    let fixture = Fixture::new();
    bench.iter(|| {
        fixture.render(EngineIdentity::fast());
    });
}

#[test]
fn profile_one_layered_frame() {
    Fixture::new().render(EngineIdentity::certified());
}

fn benchmark_column_roots(bench: &mut Bencher, width: u32) {
    let mut pieces = Vec::with_capacity(64);
    for index in 0_u32..64 {
        let nudge = f64::from(index % 8) * 0.000_125;
        let (x0, x2) = if index % 2 == 0 {
            (0.125 + nudge, f64::from(width) - 0.125 - nudge)
        } else {
            (f64::from(width) - 0.125 - nudge, 0.125 + nudge)
        };
        pieces.push(MonoPiece {
            p0: [x0, 0.031_25],
            p1: [0.5 * (x0 + x2), 0.375 + nudge],
            p2: [x2, 0.968_75],
        });
    }
    let mut scratch = RowScratch::for_tile(width);
    bench.iter(|| {
        black_box(scratch.fill_row(black_box(&pieces), [0.0, 0.0], 0, 0, width));
    });
}

macro_rules! column_root_benches {
    ($name:ident, $width:expr) => {
        #[bench]
        fn $name(bench: &mut Bencher) {
            benchmark_column_roots(bench, $width);
        }
    };
}

column_root_benches!(column_roots_02, 2);
column_root_benches!(column_roots_04, 4);
column_root_benches!(column_roots_08, 8);
column_root_benches!(column_roots_16, 16);
column_root_benches!(column_roots_32, 32);
column_root_benches!(column_roots_64, 64);
