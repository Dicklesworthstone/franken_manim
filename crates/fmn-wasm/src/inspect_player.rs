//! Page through a reconstructed frame's roots or one object's direct children.
//! Each query follows the recorded order without walking the entire object DAG.
use super::{FmnPlayer, PlayerError};
use fmn_mobject::{RenderPrimitive, Stage};
use wasm_bindgen::prelude::{JsError, wasm_bindgen};

const MAX_PAGE: u32 = 128;
const ROW_WORDS: usize = 6;

fn page(
    stage: &Stage,
    parent: Option<(u32, u32)>,
    offset: u32,
    limit: u32,
) -> Result<Vec<u32>, PlayerError> {
    let siblings = match parent {
        Some((slot, generation)) => {
            let entry = stage
                .resolve_snapshot_handle(slot, generation)
                .and_then(|mob| stage.get(mob))
                .ok_or(PlayerError::Inspection("parent is absent from this frame"))?;
            entry.submobjects()
        }
        None => stage.roots(),
    };
    let total = u32::try_from(siblings.len())
        .map_err(|_| PlayerError::Inspection("object count exceeds the inspector range"))?;
    if offset > total {
        return Err(PlayerError::Inspection("page offset exceeds sibling count"));
    }
    let count = limit.min(total - offset) as usize;
    let mut words = Vec::with_capacity(1 + count * ROW_WORDS);
    words.push(total);
    for &mob in &siblings[offset as usize..][..count] {
        let entry = stage
            .get(mob)
            .ok_or(PlayerError::Inspection("missing recorded object"))?;
        let (slot, generation) = stage
            .snapshot_handle(mob)
            .ok_or(PlayerError::Inspection("missing recorded object"))?;
        let kind = match entry.render_primitive() {
            RenderPrimitive::Vector if !entry.paints() => 0,
            RenderPrimitive::Vector => 1,
            RenderPrimitive::SurfaceGrid { .. } => 2,
            RenderPrimitive::CameraSortedSurfaceGrid { .. } => 3,
            RenderPrimitive::TriangleMesh => 4,
            RenderPrimitive::DotCloud => 5,
            RenderPrimitive::ImageQuad => 6,
        };
        let points = u32::try_from(entry.buffer.len())
            .map_err(|_| PlayerError::Inspection("record count exceeds the inspector range"))?;
        let children = u32::try_from(entry.submobjects().len())
            .map_err(|_| PlayerError::Inspection("child count exceeds the inspector range"))?;
        words.extend([
            slot,
            generation,
            kind,
            points,
            children,
            stage.z_index(mob) as u32,
        ]);
    }
    Ok(words)
}

#[wasm_bindgen]
impl FmnPlayer {
    /// Inspect an ordered page of roots (omit parent_slot) or direct children.
    /// Handles are local to the selected recording and frame. The first word
    /// is the total sibling count. Each six-word row carries slot, generation,
    /// primitive kind (0 data, 1 vector, 2 grid, 3 camera-sorted grid, 4 mesh,
    /// 5 dots, 6 image), record count, child count and signed z-index bits.
    /// Set a viewport first. Limit must be 1..=128. No cursor is moved.
    pub fn inspect_objects(
        &self,
        frame: u32,
        parent_slot: Option<u32>,
        parent_generation: u32,
        offset: u32,
        limit: u32,
    ) -> Result<Vec<u32>, JsError> {
        if !(1..=MAX_PAGE).contains(&limit) {
            return Err(PlayerError::Inspection("page size must be in 1..=128").into());
        }
        if self.core.width == 0 || self.core.height == 0 {
            return Err(PlayerError::Viewport("set_viewport before inspecting").into());
        }
        let (stage, _) = self
            .core
            .bundle
            .stage_at_with_camera(frame, (self.core.width, self.core.height))
            .map_err(PlayerError::from)?;
        Ok(page(
            &stage,
            parent_slot.map(|slot| (slot, parent_generation)),
            offset,
            limit,
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fmn_mobject::Mobject;

    #[test]
    fn sibling_pages_preserve_shared_children_order_and_signed_z_index() {
        let mut stage = Stage::new();
        let first = stage.add(Mobject::new());
        let second = stage.add(Mobject::new());
        let leaf = stage.add(Mobject::new());
        stage.attach(first, leaf).unwrap();
        stage.attach(second, leaf).unwrap();
        stage.add_many_to_scene(&[first, second]).unwrap();
        stage.set_z_index(leaf, -7, false);
        let roots = page(&stage, None, 0, 1).unwrap();
        assert_eq!(roots[0], 2);
        assert_eq!(roots.len(), 1 + ROW_WORDS);
        assert_eq!(roots[5], 1);
        let a = page(&stage, Some((roots[1], roots[2])), 0, 5).unwrap();
        let roots = page(&stage, None, 1, 1).unwrap();
        let b = page(&stage, Some((roots[1], roots[2])), 0, 5).unwrap();
        assert_eq!(a, b);
        assert_eq!((a[0], a[5], a[6] as i32), (1, 0, -7));
        assert_eq!(page(&stage, None, 2, 1).unwrap(), [2]);
        assert!(page(&stage, Some((u32::MAX, 0)), 0, 1).is_err());
        assert!(page(&stage, None, 3, 1).is_err());
    }
}
