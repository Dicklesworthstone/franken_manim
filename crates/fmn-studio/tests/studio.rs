//! fm-yh0 Studio acceptance: bounded frame fanout, deterministic inspector
//! ids and overlays, transient-vs-committed timeline seeks, terminal protocol
//! bytes, and a real pseudoterminal write on Unix.

use std::sync::Arc;

use fmn_anim::Timeline;
use fmn_core::rng::RngRoot;
use fmn_hash::sha256;
use fmn_mobject::{Mobject, Stage};
use fmn_studio::{
    CapabilityToken, DebugLayerSet, DebugOverlaySnapshot, FrameEncoding, FrameHub, FramePayload,
    FrameStream, InspectError, InspectorLimits, InspectorSnapshot, NativeSpanBinding, ScrubMode,
    SpanKind, SpanRegistry, TerminalPreview, TerminalProtocol, TileOverlay, TuiLimits,
    commit_timeline_frame, preview_timeline_frame,
};

#[test]
fn capability_is_explicit_fixed_width_and_redacted() {
    let token = CapabilityToken::new([0x5a; 32]).unwrap();
    let hex = token.expose_hex();
    assert_eq!(hex.len(), 64);
    assert_eq!(CapabilityToken::from_hex(&hex).unwrap().expose_hex(), hex);
    assert_eq!(format!("{token:?}"), "CapabilityToken([REDACTED])");
    assert!(CapabilityToken::new([0; 32]).is_err());
}

#[test]
fn frame_hub_validates_converts_and_bounds_multipart_png() {
    let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255];
    let stream = FrameStream {
        scene: "Demo".to_owned(),
        frame_index: 7,
        width: 2,
        height: 1,
        stride: 8,
        encoding: FrameEncoding::Rgba8,
        payload: FramePayload::Pipe {
            digest: sha256(&rgba),
            bytes: rgba,
        },
    };
    let hub = FrameHub::new(2, 1024 * 1024).unwrap();
    let frame = hub.publish(&stream, Default::default()).unwrap();
    assert!(frame.png.starts_with(b"\x89PNG\r\n\x1a\n"));
    assert_eq!(frame.digest, sha256(&frame.png));
    assert_eq!(frame.publication_sequence, 0);
    assert_eq!(hub.latest().unwrap().frame_index, 7);
    let part = FrameHub::multipart_part(&frame);
    assert!(part.starts_with(b"--fmn-frame\r\nContent-Type: image/png\r\n"));
    assert!(part.ends_with(b"\r\n"));

    let mut scrubbed = stream;
    scrubbed.frame_index = 2;
    let second = hub.publish(&scrubbed, Default::default()).unwrap();
    assert_eq!(second.publication_sequence, 1);
    assert_eq!(
        hub.wait_after(Some(frame.publication_sequence), std::time::Duration::ZERO)
            .unwrap()
            .frame_index,
        2
    );
}

#[test]
fn inspector_and_debug_overlays_follow_visible_family_order() {
    let first = Mobject::from_points(&[[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]);
    let second = Mobject::from_points(&[[2.0, 2.0, 1.0]]);
    let mut stage = Stage::new();
    let root = stage.add(Mobject::group(vec![first, second]));
    stage.add_to_scene(root).unwrap();
    let children = stage.get(root).unwrap().submobjects().to_vec();

    let mut spans = SpanRegistry::new();
    spans
        .bind_native(
            Arc::from("α + β"),
            &children,
            &[NativeSpanBinding {
                submobject_index: 0,
                start: 0,
                end: "α".len(),
                kind: SpanKind::TextGlyph,
            }],
        )
        .unwrap();
    let limits = InspectorLimits::default();
    let inspection = InspectorSnapshot::capture(&stage, &spans, limits).unwrap();
    assert_eq!(inspection.nodes.len(), 3);
    assert_eq!(inspection.nodes[0].children, vec![1, 2]);
    assert_eq!(
        inspection.nodes[1].source_span.as_ref().unwrap().excerpt,
        "α"
    );
    let json = inspection.to_json(limits).unwrap();
    assert!(json.starts_with(b"{\"version\":1"));
    assert!(std::str::from_utf8(&json).unwrap().contains("\"point\""));

    let overlays = DebugOverlaySnapshot::capture(
        &stage,
        None,
        DebugLayerSet::CONTROL_POINTS
            | DebugLayerSet::BOUNDING_BOXES
            | DebugLayerSet::WINDING
            | DebugLayerSet::DEPTH,
        limits,
    )
    .unwrap();
    assert_eq!(overlays.nodes.len(), 3);
    assert_eq!(
        overlays.nodes[1].winding,
        Some(fmn_studio::WindingDirection::CounterClockwise)
    );
    assert_eq!(overlays.nodes[2].center_z, Some(1.0));
    assert!(!overlays.to_json(limits).unwrap().is_empty());

    let bounded = InspectorLimits {
        max_links_per_node: 1,
        max_total_links: 1,
        max_points_per_node: 1,
        max_total_points: 1,
        ..limits
    };
    let bounded_inspection = InspectorSnapshot::capture(&stage, &spans, bounded).unwrap();
    assert!(bounded_inspection.truncated);
    assert_eq!(bounded_inspection.nodes[0].children, vec![1]);
    let bounded_overlays =
        DebugOverlaySnapshot::capture(&stage, None, DebugLayerSet::WINDING, bounded).unwrap();
    assert!(bounded_overlays.truncated);
    assert_eq!(bounded_overlays.nodes[1].winding, None);
}

#[test]
fn overlay_json_budget_stops_during_tile_encoding() {
    let limits = InspectorLimits {
        max_json_bytes: 256,
        ..InspectorLimits::default()
    };
    let overrun = |tile_count| {
        let overlays = DebugOverlaySnapshot {
            version: 1,
            layers: DebugLayerSet::TILES,
            tiles: (0..tile_count)
                .map(|index| TileOverlay {
                    index,
                    rect: [0, 0, 1, 1],
                    draws: 1,
                    partial: 1,
                    interior: 0,
                })
                .collect(),
            nodes: Vec::new(),
            truncated: false,
        };
        match overlays.to_json(limits).unwrap_err() {
            InspectError::JsonLimit { limit, needed } => (limit, needed),
            error => panic!("unexpected overlay encoding error: {error}"),
        }
    };

    let first_overrun = overrun(8);
    assert_eq!(first_overrun.0, 256);
    assert!(first_overrun.1 > first_overrun.0);
    assert_eq!(first_overrun, overrun(1_000));
}

#[test]
fn timeline_preview_restores_live_stage_while_commit_leaves_target() {
    let mut timeline = Timeline::new(10).unwrap();
    timeline.wait(1.0).unwrap();
    let rng = RngRoot::from_seed(7);
    let mut stage = Stage::new();

    let preview = preview_timeline_frame(&mut timeline, &mut stage, &rng, 5).unwrap();
    assert_eq!(preview.mode(), ScrubMode::Preview);
    assert_eq!(preview.packet().frame_index(), 5);
    assert_eq!(stage.time(), 0.0);
    assert_eq!(preview.checkpointed_segments(), &[0]);

    let committed = commit_timeline_frame(&mut timeline, &mut stage, &rng, 5).unwrap();
    assert_eq!(committed.mode(), ScrubMode::Commit);
    assert_eq!(stage.time(), 0.5);
}

#[test]
fn terminal_adapters_emit_bounded_protocol_bytes() {
    let rgba = [255, 0, 0, 255, 0, 255, 0, 255];
    for (protocol, prefix) in [
        (TerminalProtocol::Kitty, b"\x1b_G".as_slice()),
        (TerminalProtocol::Sixel, b"\x1bP0;0;0q".as_slice()),
    ] {
        let mut out = Vec::new();
        TerminalPreview::new(protocol, TuiLimits::default())
            .unwrap()
            .write_rgba8(&mut out, 2, 1, &rgba)
            .unwrap();
        assert!(out.starts_with(prefix));
        assert!(out.ends_with(b"\x1b\\"));
    }
}

#[cfg(feature = "metal")]
#[test]
fn studio_metal_feature_uses_the_truthful_preview_selector() {
    let renderer = fmn_studio::PreviewRenderer::new().unwrap();
    #[cfg(not(target_os = "macos"))]
    assert!(matches!(
        renderer,
        fmn_studio::PreviewRenderer::FastCpu(fmn_studio::PreviewFallback::Unavailable)
    ));
    #[cfg(target_os = "macos")]
    assert!(matches!(
        renderer,
        fmn_studio::PreviewRenderer::Metal(_)
            | fmn_studio::PreviewRenderer::FastCpu(fmn_studio::PreviewFallback::Unavailable)
    ));
}

#[cfg(all(feature = "metal", not(target_os = "macos")))]
#[test]
fn native_preview_unavailability_uses_the_cpu_visible_studio_stream() {
    let mut renderer = fmn_studio::StudioPreviewRenderer::new(
        fmn_studio::StudioPreviewConfig::new(16, 16, "fallback", 1),
    )
    .unwrap();
    assert_eq!(
        renderer.route(),
        fmn_studio::StudioPreviewRoute::CpuStream(fmn_studio::PreviewFallback::Unavailable)
    );
    assert!(renderer.presentation_pipeline_info().is_none());
    assert_eq!(renderer.poll_events().unwrap(), None);

    let stage = Stage::new();
    let config = fmn_render::FrameConfig::new(
        fmn_render::Viewport {
            width: 16,
            height: 16,
        },
        fmn_render::ScreenMap {
            scale: 1.0,
            origin: [8.0, 8.0],
        },
        fmn_core::color::Srgb::from_rgb8(12, 18, 24).to_linear(1.0),
    );
    let mut plan = fmn_render::RenderPlan::new();
    plan.sync(&stage, 0);
    let mono = fmn_render::MonoTable::build(&plan, config.map);
    let binning = fmn_render::Binning::build(
        &plan,
        config.viewport,
        fmn_render::Tiling::default(),
        config.map,
    );
    let output = renderer.render(&plan, &mono, &binning, config).unwrap();
    let frame = output
        .into_stream()
        .expect("off-macOS Studio preview returns CPU-visible bytes");
    assert_eq!(
        frame.route,
        fmn_studio::PreviewRoute::FastCpu(fmn_studio::PreviewFallback::Unavailable)
    );
    assert_eq!(frame.frame.as_bytes().len(), 16 * 16 * 4);
    assert!(frame.metal.is_none());
    renderer.close().unwrap();
}

#[cfg(unix)]
#[test]
fn kitty_preview_crosses_a_real_pseudoterminal() {
    use std::fs::{File, OpenOptions};
    use std::io::Read;

    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    let master = openpt(OpenptFlags::RDWR).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave_name = ptsname(&master, Vec::new()).unwrap();
    let mut slave = OpenOptions::new()
        .read(true)
        .write(true)
        .open(slave_name.to_str().unwrap())
        .unwrap();
    let mut master = File::from(master);

    let preview = TerminalPreview::new(TerminalProtocol::Kitty, TuiLimits::default()).unwrap();
    let rgba = [12, 34, 56, 255];
    let mut expected = Vec::new();
    preview.write_rgba8(&mut expected, 1, 1, &rgba).unwrap();
    preview.write_rgba8(&mut slave, 1, 1, &rgba).unwrap();

    let mut observed = vec![0; expected.len()];
    master.read_exact(&mut observed).unwrap();
    assert_eq!(observed, expected);
}
