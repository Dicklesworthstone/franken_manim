//! Native Studio interaction over Proscenium's one event/editing implementation.
//!
//! A preview frame is copied into an independent Scene through Marionette's
//! durable snapshot bridge, then wrapped by [`fmn_scene::InteractiveScene`].
//! Browser/TUI events therefore execute the same selection, grab, resize,
//! grouping, clipboard, color-pick, undo, and nudge semantics as the engine.
//! This module owns no duplicate editing rules and advances no frame clock.

use fmn_scene::studio_bridge::{Snapshot, Stage};
use fmn_scene::{
    EventError, EventPayload, InteractiveClipboard, InteractiveScene, RuntimeConfig, Scene,
    SceneError, SelectionHighlight, SelectionRectangle,
};

/// Failure while materializing or driving one interactive preview frame.
#[derive(Debug)]
pub enum InteractivePreviewError {
    /// The preview frame rate cannot construct a Scene clock.
    Scene(SceneError),
    /// Proscenium rejected an input event or listener registration.
    Event(EventError),
    /// Marionette could not serialize the captured arena snapshot.
    SnapshotEncode(String),
    /// Marionette refused the snapshot while rebinding it to the preview Scene.
    SnapshotDecode(String),
    /// Durable snapshots intentionally contain updater identities, not callables.
    /// A paused Studio edit must not silently discard executable scene behavior.
    UpdatersRequireLiveScene { count: usize },
}

impl std::fmt::Display for InteractivePreviewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scene(error) => write!(f, "interactive preview Scene failed: {error}"),
            Self::Event(error) => write!(f, "interactive preview event failed: {error}"),
            Self::SnapshotEncode(error) => {
                write!(f, "interactive preview snapshot encode failed: {error}")
            }
            Self::SnapshotDecode(error) => {
                write!(f, "interactive preview snapshot decode failed: {error}")
            }
            Self::UpdatersRequireLiveScene { count } => write!(
                f,
                "interactive preview carries {count} updater identities; a live-scene adapter is required"
            ),
        }
    }
}

impl std::error::Error for InteractivePreviewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Scene(error) => Some(error),
            Self::Event(error) => Some(error),
            Self::SnapshotEncode(_)
            | Self::SnapshotDecode(_)
            | Self::UpdatersRequireLiveScene { .. } => None,
        }
    }
}

impl From<SceneError> for InteractivePreviewError {
    fn from(error: SceneError) -> Self {
        Self::Scene(error)
    }
}

impl From<EventError> for InteractivePreviewError {
    fn from(error: EventError) -> Self {
        Self::Event(error)
    }
}

/// Receipt for one event admitted at the paused Studio serial boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractiveDispatch {
    /// Stable Proscenium event sequence allocated before dispatch.
    pub sequence: u64,
    /// Number of queued/replay events dispatched at this boundary.
    pub dispatched: usize,
}

/// Mutable, transient editor state for one captured preview frame.
///
/// Constructing this value does not mutate `source`, advance its clock, or run
/// its updaters. The copied Scene receives only events explicitly dispatched
/// here. A Studio scrub can discard this value and reconstruct from the
/// canonical captured frame, which gives transient editing precise reset
/// semantics without contaminating the replay journal.
pub struct InteractivePreview {
    frame_index: u64,
    scene: InteractiveScene,
}

impl std::fmt::Debug for InteractivePreview {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InteractivePreview")
            .field("frame_index", &self.frame_index)
            .field("selection", &self.scene.selection())
            .finish_non_exhaustive()
    }
}

impl InteractivePreview {
    /// Copy one captured Stage into an independent interactive Scene.
    ///
    /// The durable encode/decode path intentionally rebinds every handle to
    /// the fresh Scene's arena in one operation. This preserves root order,
    /// DAG sharing, placements, styles, images, tracker state, and renderer
    /// identity without per-root copies. Updater-bearing snapshots refuse:
    /// persistence records identities but cannot serialize their callables.
    pub fn from_stage(
        source: &Stage,
        fps: u32,
        seed: u64,
        frame_index: u64,
    ) -> Result<Self, InteractivePreviewError> {
        let bytes = source
            .snapshot()
            .to_bytes()
            .map_err(|error| InteractivePreviewError::SnapshotEncode(error.to_string()))?;
        let mut config = RuntimeConfig::default();
        config.fps = fps;
        let scene = Scene::new(config, seed)?;
        let mut scene = InteractiveScene::new(scene)?;
        let decoded = Snapshot::from_bytes(&bytes, scene.scene().stage())
            .map_err(|error| InteractivePreviewError::SnapshotDecode(error.to_string()))?;
        let updater_count = decoded
            .updaters
            .entries
            .iter()
            .map(|(_, updaters)| updaters.len())
            .sum();
        if updater_count != 0 {
            return Err(InteractivePreviewError::UpdatersRequireLiveScene {
                count: updater_count,
            });
        }
        scene.scene_mut().stage_mut().restore(&decoded.snapshot);
        Ok(Self { frame_index, scene })
    }

    /// Reset all transient edits from a canonical captured Stage.
    pub fn reset(
        &mut self,
        source: &Stage,
        fps: u32,
        seed: u64,
        frame_index: u64,
    ) -> Result<(), InteractivePreviewError> {
        *self = Self::from_stage(source, fps, seed, frame_index)?;
        Ok(())
    }

    /// Dispatch one typed host event immediately at the paused Scene boundary.
    pub fn dispatch(
        &mut self,
        payload: EventPayload,
    ) -> Result<InteractiveDispatch, InteractivePreviewError> {
        let sequence = self.scene.scene_mut().queue_event(payload)?;
        let dispatched = self.scene.scene_mut().dispatch_pending_events()?;
        Ok(InteractiveDispatch {
            sequence,
            dispatched,
        })
    }

    /// Canonical timeline frame this transient editor was materialized from.
    #[must_use]
    pub const fn frame_index(&self) -> u64 {
        self.frame_index
    }

    /// Mutated arena rendered and inspected by Studio after input.
    #[must_use]
    pub fn stage(&self) -> &Stage {
        self.scene.scene().stage()
    }

    /// Mutable arena for narrowly scoped Studio integration helpers.
    pub fn stage_mut(&mut self) -> &mut Stage {
        self.scene.scene_mut().stage_mut()
    }

    /// Selected mobjects in engine selection order.
    #[must_use]
    pub fn selection(&self) -> Vec<fmn_scene::studio_bridge::Mob> {
        self.scene.selection()
    }

    /// Selection outlines for the Studio overlay layer.
    #[must_use]
    pub fn selection_highlights(&self) -> Vec<SelectionHighlight> {
        self.scene.selection_highlights()
    }

    /// Active sweep-selection rectangle, if any.
    #[must_use]
    pub fn selection_rectangle(&self) -> Option<SelectionRectangle> {
        self.scene.selection_rectangle()
    }

    /// Scene-owned clipboard, never an OS clipboard dependency.
    #[must_use]
    pub fn clipboard(&self) -> InteractiveClipboard {
        self.scene.clipboard()
    }

    /// Whether the engine information overlay is active.
    #[must_use]
    pub fn information_visible(&self) -> bool {
        self.scene.information_visible()
    }

    /// Whether the engine cursor/crosshair overlay is active.
    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.scene.cursor_visible()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_mobject::{Mobject, Stage};
    use fmn_scene::{Key, Modifiers, MouseButton};

    fn point_stage() -> Stage {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
        stage.add_to_scene(root).expect("fresh root");
        stage
    }

    fn primary_click(point: [f64; 3]) -> EventPayload {
        EventPayload::MousePress {
            point,
            button: MouseButton::Left,
            modifiers: Modifiers::PRIMARY,
        }
    }

    #[test]
    fn preview_uses_engine_selection_and_nudge_semantics() {
        let source = point_stage();
        let source_bounds = source.get_bounding_box(source.roots()[0]);
        let mut preview = InteractivePreview::from_stage(&source, 30, 7, 12).expect("preview");
        let receipt = preview
            .dispatch(primary_click([0.0, 0.0, 0.0]))
            .expect("select");
        assert_eq!(receipt, InteractiveDispatch { sequence: 0, dispatched: 1 });
        assert_eq!(preview.selection().len(), 1);
        preview
            .dispatch(EventPayload::KeyPress {
                key: Key::ArrowRight,
                modifiers: Modifiers::empty(),
            })
            .expect("nudge");
        let root = preview.stage().roots()[0];
        let moved = preview.stage().get_bounding_box(root);
        assert!((moved.mid[0] - source_bounds.mid[0] - 0.05).abs() < 1.0e-9);
        assert_eq!(source.get_bounding_box(source.roots()[0]), source_bounds);
    }

    #[test]
    fn reset_discards_transient_edits_and_selection() {
        let source = point_stage();
        let mut preview = InteractivePreview::from_stage(&source, 24, 9, 3).expect("preview");
        preview
            .dispatch(primary_click([0.0, 0.0, 0.0]))
            .expect("select");
        preview
            .dispatch(EventPayload::KeyPress {
                key: Key::ArrowUp,
                modifiers: Modifiers::empty(),
            })
            .expect("nudge");
        assert_eq!(preview.selection().len(), 1);
        preview.reset(&source, 24, 9, 4).expect("reset");
        assert!(preview.selection().is_empty());
        assert_eq!(preview.frame_index(), 4);
        let bounds = preview.stage().get_bounding_box(preview.stage().roots()[0]);
        assert!((bounds.mid[1]).abs() < 1.0e-9);
    }

    #[test]
    fn source_graph_sharing_survives_one_snapshot_rebind() {
        let mut source = Stage::new();
        let shared = source.add(Mobject::from_points(&[[0.0, 0.0, 0.0]]));
        let left = source.add(Mobject::new());
        let right = source.add(Mobject::new());
        source.attach(left, shared).expect("left edge");
        source.attach(right, shared).expect("right edge");
        source
            .add_many_to_scene(&[left, right])
            .expect("shared roots");
        let preview = InteractivePreview::from_stage(&source, 30, 1, 0).expect("preview");
        let roots = preview.stage().roots();
        assert_eq!(roots.len(), 2);
        let left_child = preview.stage().get(roots[0]).unwrap().submobjects()[0];
        let right_child = preview.stage().get(roots[1]).unwrap().submobjects()[0];
        assert_eq!(left_child, right_child);
        assert_eq!(preview.stage().get(left_child).unwrap().parents().len(), 2);
    }

    #[test]
    fn updater_snapshots_refuse_instead_of_dropping_callables() {
        let mut source = point_stage();
        let root = source.roots()[0];
        source
            .add_updater(root, |_stage, _mob| {}, false)
            .expect("updater");
        let error = InteractivePreview::from_stage(&source, 30, 1, 0)
            .expect_err("durable preview cannot recreate callables");
        assert!(matches!(
            error,
            InteractivePreviewError::UpdatersRequireLiveScene { count: 1 }
        ));
    }

    #[test]
    fn invalid_fps_is_a_typed_scene_error() {
        let source = point_stage();
        let error = InteractivePreview::from_stage(&source, 0, 1, 0)
            .expect_err("zero fps must fail");
        assert!(matches!(error, InteractivePreviewError::Scene(_)));
    }
}
