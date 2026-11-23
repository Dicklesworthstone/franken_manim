//! `TransformMatchingParts` / `TransformMatchingShapes`, ported from the
//! pinned Reference's `animation/transform_matching_parts.py`:
//!
//! - [`has_same_shape_as`] (mobject.py:770): both point sets centered and
//!   height-normalized, then `np.isclose(atol = width·1e-2)` — ported
//!   comparison for comparison (a degenerate zero-height side never
//!   matches, exactly as the Reference's NaN propagation decides).
//! - [`transform_matching_parts`] builds the animation list the
//!   Reference's `AnimationGroup` wraps: user-specified pairs first, then
//!   every same-shape pair from the piece product, then the leftovers —
//!   sources fade out to the target's center, targets fade in from the
//!   source's center (the `FadeOutToPoint`/`FadeInFromPoint` calls at
//!   transform_matching_parts.py:57).
//! - [`TransformMatchingParts`] owns the complete native animation:
//!   grouping (`run_time = 2`, `lag_ratio = 0`), lifecycle delegation,
//!   and replacement of the source and temporary group by the original
//!   target. Its prepared group can run on native frame workers while
//!   begin, finish and cleanup remain on the scene owner.
//! - [`TransformMatchingShapes`] is the Reference's literal alias — same
//!   animation, same defaults.
//!
//! [`transform_matching_keys`] is the string/Tex seam. Scribe supplies one
//! semantic key per pointful native-span primitive; Choreo pairs equal keys
//! in stable occurrence order and fades the leftovers. It never shape-matches
//! Tex and never reconstructs provenance from rendered geometry (D-09).

use std::collections::HashSet;

use fmn_mobject::{Mob, Stage, StageError};

use crate::animation::{AnimError, AnimState, Animation, AnimationSignature, RateFunc};
use crate::composition::AnimationGroup;
use crate::fading::{fade_in_from_point, fade_out_to_point};
use crate::transform::Transform;

/// Timing for a complete shape-matching replacement.
#[derive(Debug, Clone)]
pub struct MatchingPartsConfig {
    /// Wall-clock duration of the complete replacement, in seconds.
    pub run_time: f64,
    /// Overlap between successive matched transforms and unmatched fades.
    pub lag_ratio: f64,
    /// Rate curve applied to the group's internal timeline.
    pub rate_func: RateFunc,
}

impl Default for MatchingPartsConfig {
    fn default() -> Self {
        Self {
            run_time: 2.0,
            lag_ratio: 0.0,
            rate_func: RateFunc::linear(),
        }
    }
}

/// The plan binds every family member, including point-free containers.
/// Moving and restyling pieces is valid; regrouping or adding new pieces
/// requires a new matching plan before any animation begins.
#[derive(Clone)]
struct MatchingFamily {
    root: Mob,
    layout: Vec<MatchingFamilyRow>,
}

/// Construction-time member, ordered children, and point-empty observation.
pub(crate) type MatchingFamilyRow = (Mob, Vec<Mob>, bool);

impl MatchingFamily {
    fn read(stage: &Stage, root: Mob) -> Result<Self, AnimError> {
        if !stage.contains(root) {
            return Err(AnimError::StaleHandle(root));
        }
        let layout = stage
            .family(root)
            .into_iter()
            .map(|mob| {
                let entry = stage.get(mob).ok_or(AnimError::StaleHandle(mob))?;
                Ok((mob, entry.submobjects().to_vec(), entry.buffer.is_empty()))
            })
            .collect::<Result<Vec<_>, AnimError>>()?;
        Ok(Self { root, layout })
    }

    fn validate(&self, stage: &Stage) -> Result<(), AnimError> {
        for (mob, children, empty) in &self.layout {
            let entry = stage.get(*mob).ok_or(AnimError::StaleHandle(*mob))?;
            if entry.submobjects() != children.as_slice() || entry.buffer.is_empty() != *empty {
                return Err(AnimError::Stage(StageError::FamilyShapeMismatch));
            }
        }
        Ok(())
    }

    /// Validate the original manifest without reading today's topology or
    /// point counts. Earlier segments may temporarily reveal or regroup its
    /// members; the ordinary begin hook validates the eventual live family.
    /// The caller charges one index, byte and two-usize stack entry per row,
    /// plus one durable handle key per edge, before this bounded walk.
    fn declaration_index(&self, stage: &Stage) -> Option<Vec<(u32, u32, usize)>> {
        if self.layout.first().map(|row| row.0) != Some(self.root) {
            return None;
        }
        let mut index = Vec::new();
        index.try_reserve_exact(self.layout.len()).ok()?;
        for (ordinal, (mob, children, _)) in self.layout.iter().enumerate() {
            let (slot, generation) = stage.snapshot_handle(*mob)?;
            index.push((slot, generation, ordinal));
            // Canonical Stage families cannot repeat a child in one row.
            // Decoded manifests must retain that invariant independently of
            // any legitimate alignment changes in the current live family.
            let mut child_keys = Vec::new();
            child_keys.try_reserve_exact(children.len()).ok()?;
            for &child in children {
                child_keys.push(stage.snapshot_handle(child)?);
            }
            child_keys.sort_unstable();
            if child_keys.windows(2).any(|keys| keys[0] == keys[1]) {
                return None;
            }
        }
        index.sort_unstable_by_key(|&(slot, generation, _)| (slot, generation));
        if index
            .windows(2)
            .any(|rows| (rows[0].0, rows[0].1) == (rows[1].0, rows[1].1))
        {
            return None;
        }
        let mut visited = Vec::new();
        visited.try_reserve_exact(self.layout.len()).ok()?;
        visited.resize(self.layout.len(), 0_u8);
        let mut stack = Vec::new();
        stack.try_reserve_exact(self.layout.len()).ok()?;
        stack.push((0, 0));
        visited[0] = 1;
        let mut next = 1;
        while let Some((row, edge)) = stack.last_mut() {
            let children = &self.layout[*row].1;
            let Some(child) = children.get(*edge) else {
                visited[*row] = 2;
                stack.pop();
                continue;
            };
            *edge += 1;
            let key = stage.snapshot_handle(*child)?;
            let found = index
                .binary_search_by_key(&key, |&(slot, generation, _)| (slot, generation))
                .ok()?;
            let child_row = index[found].2;
            match visited[child_row] {
                1 => return None, // An ancestor edge is a cycle, not a diamond.
                2 => {}
                _ => {
                    if child_row != next {
                        return None;
                    }
                    next += 1;
                    visited[child_row] = 1;
                    stack.push((child_row, 0));
                }
            }
        }
        (next == self.layout.len()).then_some(index)
    }
}

/// Match shapes, transform paired pieces, fade the leftovers and publish
/// the original target when the animation completes.
///
/// Explicit pairs claim point-bearing family members first; otherwise the
/// normalized shape probe decides the pairs. The source and target must
/// have disjoint families. Their topology is checked again at begin, so a
/// delayed play cannot silently animate a stale matching plan.
pub struct TransformMatchingParts {
    group: AnimationGroup,
    source: MatchingFamily,
    target: MatchingFamily,
    begun: bool,
    finished: bool,
    cleaned: bool,
    error: Option<AnimError>,
}

/// The Reference's literal alias for [`TransformMatchingParts`].
pub type TransformMatchingShapes = TransformMatchingParts;

impl TransformMatchingParts {
    /// Match both families with the default two-second simultaneous timing.
    ///
    /// # Errors
    /// [`AnimError::StaleHandle`] for a dead operand,
    /// [`AnimError::AliasedMatchingFamilies`] for shared family members, or
    /// [`AnimError::Stage`] if the matching group cannot be assembled.
    pub fn new(stage: &mut Stage, source: Mob, target: Mob) -> Result<Self, AnimError> {
        Self::with_config(stage, source, target, &[], MatchingPartsConfig::default())
    }

    /// Give explicit pairs first claim on the available point-bearing
    /// members, preserving [`transform_matching_parts`]'s skip semantics
    /// for unavailable or previously claimed members.
    ///
    /// # Errors
    /// As [`Self::new`].
    pub fn with_matched_pairs(
        stage: &mut Stage,
        source: Mob,
        target: Mob,
        matched_pairs: &[(Mob, Mob)],
    ) -> Result<Self, AnimError> {
        Self::with_config(
            stage,
            source,
            target,
            matched_pairs,
            MatchingPartsConfig::default(),
        )
    }

    /// Build a replacement with explicit pairs and composition timing.
    /// Empty families are valid and still complete the replacement lifecycle.
    ///
    /// # Errors
    /// As [`Self::new`]. Both operand families are checked before the
    /// constructor allocates or attaches any animation-owned mobject.
    pub fn with_config(
        stage: &mut Stage,
        source: Mob,
        target: Mob,
        matched_pairs: &[(Mob, Mob)],
        config: MatchingPartsConfig,
    ) -> Result<Self, AnimError> {
        let source = MatchingFamily::read(stage, source)?;
        let target = MatchingFamily::read(stage, target)?;
        let source_members: HashSet<_> = source.layout.iter().map(|(mob, _, _)| *mob).collect();
        if target
            .layout
            .iter()
            .any(|(mob, _, _)| source_members.contains(mob))
        {
            return Err(AnimError::AliasedMatchingFamilies);
        }
        let animations = transform_matching_parts(stage, source.root, target.root, matched_pairs)?;
        let group = AnimationGroup::with_lag_ratio(stage, animations, config.lag_ratio)?
            .with_run_time(config.run_time)
            .with_rate_func(config.rate_func)
            .with_name("TransformMatchingParts");
        Ok(Self {
            group,
            source,
            target,
            begun: false,
            finished: false,
            cleaned: false,
            error: None,
        })
    }

    /// The actual Transform/fade plan, in composition order.
    #[must_use]
    pub fn animations(&self) -> &[Box<dyn Animation>] {
        self.group.animations()
    }

    /// The original target, published on successful cleanup.
    #[must_use]
    pub fn target(&self) -> Mob {
        self.target.root
    }

    /// A future declaration carries no already-run lifecycle or prepared
    /// timing. Child state is checked recursively by the closed-tree owner.
    pub(crate) fn declaration_is_fresh(&self) -> bool {
        !self.begun
            && !self.finished
            && !self.cleaned
            && self.error.is_none()
            && self.group.declaration_is_fresh()
    }

    pub(crate) fn declaration_group(&self) -> &AnimationGroup {
        &self.group
    }

    pub(crate) fn declaration_families(&self) -> [(Mob, &[MatchingFamilyRow]); 2] {
        [
            (self.source.root, &self.source.layout),
            (self.target.root, &self.target.layout),
        ]
    }

    /// Preserve the matching plan's original identity while an earlier play
    /// is active. Live geometry/topology is intentionally checked only when
    /// the future matching animation begins, not at an earlier checkpoint.
    pub(crate) fn declaration_families_valid(&self, stage: &Stage) -> bool {
        let Some(source) = self.source.declaration_index(stage) else {
            return false;
        };
        let Some(target) = self.target.declaration_index(stage) else {
            return false;
        };
        let Some(group) = stage.snapshot_handle(self.group.state().mobject()) else {
            return false;
        };
        if source
            .iter()
            .chain(&target)
            .any(|&(slot, generation, _)| (slot, generation) == group)
        {
            return false;
        }
        let (mut a, mut b) = (0, 0);
        while a < source.len() && b < target.len() {
            match (source[a].0, source[a].1).cmp(&(target[b].0, target[b].1)) {
                std::cmp::Ordering::Less => a += 1,
                std::cmp::Ordering::Greater => b += 1,
                std::cmp::Ordering::Equal => return false,
            }
        }
        true
    }

    /// Clone only after the declaration owner has verified each child's
    /// concrete native type. No matching search, hooks or arena edits run.
    pub(crate) fn clone_for_declaration(&self, children: Vec<Box<dyn Animation>>) -> Self {
        self.clone_for_continuation(children)
    }

    /// Only a successfully begun, unfinished lifetime has an active carrier.
    /// Its original manifests and each prepared child are validated separately
    /// by the continuation owner after their allocation budgets are charged.
    pub(crate) fn continuation_lifecycle_valid(&self, stage: &Stage) -> bool {
        self.begun
            && !self.finished
            && !self.cleaned
            && self.error.is_none()
            && self.group.continuation_lifecycle_valid(stage)
    }

    /// Copy a dynamically verified native child tree without invoking hooks.
    /// This does not authorize isolated preparation of a matching animation.
    pub(crate) fn clone_for_continuation(&self, children: Vec<Box<dyn Animation>>) -> Self {
        Self {
            group: self.group.clone_for_continuation(children),
            source: self.source.clone(),
            target: self.target.clone(),
            begun: self.begun,
            finished: self.finished,
            cleaned: self.cleaned,
            error: self.error.clone(),
        }
    }

    /// Rebind a decoded eager group and its original operand manifests. All
    /// children already own their prepared starting/target data; rerunning
    /// begin or the matching search would discard the saved lifetime.
    pub(crate) fn from_continuation(
        group: AnimationGroup,
        source: (Mob, Vec<MatchingFamilyRow>),
        target: (Mob, Vec<MatchingFamilyRow>),
    ) -> Self {
        Self {
            group,
            source: MatchingFamily {
                root: source.0,
                layout: source.1,
            },
            target: MatchingFamily {
                root: target.0,
                layout: target.1,
            },
            begun: true,
            finished: false,
            cleaned: false,
            error: None,
        }
    }

    fn inventory(&self, mut members: Vec<Mob>) -> Vec<Mob> {
        for mob in [self.source.root, self.target.root] {
            if !members.contains(&mob) {
                members.push(mob);
            }
        }
        members
    }
}

impl Animation for TransformMatchingParts {
    fn state(&self) -> &AnimState {
        self.group.state()
    }
    fn state_mut(&mut self) -> &mut AnimState {
        self.group.state_mut()
    }
    fn duration_on_grid(&self, fps: u32) -> Result<crate::FrameDuration, AnimError> {
        self.group.duration_on_grid(fps)
    }
    fn prepare_timing(&mut self, fps: u32) -> Result<(), AnimError> {
        self.group.prepare_timing(fps)
    }
    fn composition_timing(&self) -> Option<crate::composition::CompositionTiming> {
        self.group.composition_timing()
    }
    fn update_rate_info(
        &mut self,
        run_time: Option<f64>,
        rate_func: Option<RateFunc>,
        lag_ratio: Option<f64>,
    ) {
        self.group.update_rate_info(run_time, rate_func, lag_ratio);
    }
    fn all_mobjects(&self) -> Vec<Mob> {
        self.inventory(self.group.all_mobjects())
    }
    fn preflight_mobjects(&self) -> Vec<Mob> {
        self.inventory(self.group.preflight_mobjects())
    }
    fn validate_begin(&self, stage: &Stage) -> Result<(), AnimError> {
        self.source.validate(stage)?;
        self.target.validate(stage)?;
        self.group.validate_begin(stage)
    }
    fn begin(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
        self.validate_begin(stage)?;
        self.begun = false;
        self.finished = false;
        self.cleaned = false;
        self.error = None;
        self.group.begin(stage)?;
        self.begun = true;
        Ok(())
    }
    fn interpolate_submobject(&mut self, _stage: &mut Stage, _mobs: &[Mob], _alpha: f64) {}
    fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
        self.group.interpolate(stage, alpha);
    }
    fn interpolate_at_frame(&mut self, stage: &mut Stage, frame: f64, fps: u32) {
        self.group.interpolate_at_frame(stage, frame, fps);
    }
    fn interpolate_at_position(
        &mut self,
        stage: &mut Stage,
        position: &crate::FramePosition,
        fps: u32,
    ) -> Result<(), AnimError> {
        self.group.interpolate_at_position(stage, position, fps)
    }
    fn interpolate_at_time(
        &mut self,
        stage: &mut Stage,
        time: crate::RationalTime,
    ) -> Result<(), AnimError> {
        self.group.interpolate_at_time(stage, time)
    }
    fn update_mobjects(&mut self, stage: &mut Stage, dt: f64) {
        self.group.update_mobjects(stage, dt);
    }
    fn finish(&mut self, stage: &mut Stage) {
        if self.begun && !self.finished {
            self.group.finish(stage);
            self.finished = true;
        }
    }
    fn abort(&mut self, stage: &mut Stage) {
        self.group.abort(stage);
        self.begun = false;
        self.finished = false;
    }
    fn clean_up_from_scene(&mut self, stage: &mut Stage) {
        if self.cleaned || self.deferred_error().is_some() {
            return;
        }
        if !self.finished {
            self.error = Some(AnimError::InvalidFramePhase(
                "shape matching must finish before scene cleanup",
            ));
            return;
        }
        if !stage.contains(self.target.root) {
            self.error = Some(AnimError::StaleHandle(self.target.root));
            return;
        }
        self.group.clean_up_from_scene(stage);
        stage.remove_many_from_scene(&[self.state().mobject(), self.source.root]);
        if let Err(error) = stage.add_to_scene(self.target.root) {
            self.error = Some(AnimError::Stage(error));
            return;
        }
        self.cleaned = true;
    }
    fn deferred_error(&self) -> Option<AnimError> {
        self.error.clone().or_else(|| self.group.deferred_error())
    }
    fn collect_resumed_updater_mobjects(&self, out: &mut Vec<Mob>) {
        self.group.collect_resumed_updater_mobjects(out);
    }
    fn effect_signature(&self) -> AnimationSignature {
        self.group.effect_signature()
    }
    fn has_preparing_rate(&self) -> bool {
        self.group.has_preparing_rate()
    }
    fn freeze_interpolation(&self) -> Option<crate::pure_frame::FrozenAnimation> {
        if !self.begun || self.finished || self.cleaned || self.deferred_error().is_some() {
            return None;
        }
        self.group.freeze_interpolation()
    }
    fn can_replay_interpolation(&self) -> bool {
        self.group.can_replay_interpolation()
    }
    fn prepare_interpolation_replay(&mut self) {
        self.group.prepare_interpolation_replay();
    }
}

/// The family members with points, in family order (mobject.py:435).
fn pieces_of(stage: &Stage, mob: Mob) -> Vec<Mob> {
    stage
        .family(mob)
        .into_iter()
        .filter(|&m| stage.get(m).is_some_and(|e| !e.buffer.is_empty()))
        .collect()
}

/// All family points of `mob`, concatenated in family order (the
/// Reference's `get_all_points`).
fn all_points(stage: &Stage, mob: Mob) -> Vec<f64> {
    let mut out = Vec::new();
    for member in stage.family(mob) {
        if let Some(points) = stage.get_points(member) {
            out.extend(points.into_iter().flatten());
        }
    }
    out
}

/// Reference `has_same_shape_as` (mobject.py:770): center both point
/// sets, normalize by height, compare with
/// `np.isclose(atol = self.width·1e-2)` (default `rtol = 1e-5`).
#[must_use]
pub fn has_same_shape_as(stage: &Stage, a: Mob, b: Mob) -> bool {
    let pa = all_points(stage, a);
    let pb = all_points(stage, b);
    if pa.len() != pb.len() {
        return false;
    }
    if pa.is_empty() {
        return true; // np.isclose(...).all() over empty arrays is True
    }
    let normalize = |stage: &Stage, mob: Mob, points: &[f64]| -> Vec<f64> {
        let center = stage.get_center(mob);
        let height = stage.get_height(mob);
        points
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| {
                [
                    (p[0] - center[0]) / height,
                    (p[1] - center[1]) / height,
                    (p[2] - center[2]) / height,
                ]
            })
            .collect()
    };
    let na = normalize(stage, a, &pa);
    let nb = normalize(stage, b, &pb);
    let atol = stage.get_width(a) * 1e-2;
    // np.isclose: |x − y| <= atol + rtol·|y|. NaN (zero height) fails
    // every comparison, exactly as the Reference decides degenerates.
    na.iter()
        .zip(&nb)
        .all(|(&x, &y)| (x - y).abs() <= atol + 1e-5 * y.abs())
}

/// `TransformMatchingParts` (transform_matching_parts.py:21) /
/// `TransformMatchingShapes` (its literal alias): the matched-pair
/// animation list. `matched_pairs` claims first (in order), the
/// same-shape product second, and the leftovers fade. Use
/// [`TransformMatchingParts`] for grouping, timing and scene replacement.
///
/// # Errors
/// [`AnimError::StaleHandle`] / [`AnimError::Stage`].
pub fn transform_matching_parts(
    stage: &mut Stage,
    source: Mob,
    target: Mob,
    matched_pairs: &[(Mob, Mob)],
) -> Result<Vec<Box<dyn Animation>>, AnimError> {
    if !stage.contains(source) {
        return Err(AnimError::StaleHandle(source));
    }
    if !stage.contains(target) {
        return Err(AnimError::StaleHandle(target));
    }
    let mut source_pieces = pieces_of(stage, source);
    let mut target_pieces = pieces_of(stage, target);
    let mut anims: Vec<Box<dyn Animation>> = Vec::new();

    // transform_matching_parts.py:76 add_transform, call for call. Both
    // `match_animation` and `mismatch_animation` are Transform at the
    // Reference defaults, so the shape probe decides nothing here — the
    // parameterized variants that override one of them arrive with their
    // consumers.
    fn add_transform(
        stage: &Stage,
        s: Mob,
        t: Mob,
        anims: &mut Vec<Box<dyn Animation>>,
        source_pieces: &mut Vec<Mob>,
        target_pieces: &mut Vec<Mob>,
    ) {
        let new_source = pieces_of(stage, s);
        let new_target = pieces_of(stage, t);
        if new_source.is_empty() || new_target.is_empty() {
            return; // never animate null sources or targets
        }
        if !new_source.iter().all(|m| source_pieces.contains(m))
            || !new_target.iter().all(|m| target_pieces.contains(m))
        {
            return; // already claimed
        }
        anims.push(Box::new(Transform::new(s, t)));
        source_pieces.retain(|m| !new_source.contains(m));
        target_pieces.retain(|m| !new_target.contains(m));
    }

    for &(s, t) in matched_pairs {
        add_transform(
            stage,
            s,
            t,
            &mut anims,
            &mut source_pieces,
            &mut target_pieces,
        );
    }
    // The same-shape product over the piece lists as they stand after
    // user pairs claimed theirs (transform_matching_parts.py:50).
    let product_pairs: Vec<(Mob, Mob)> = source_pieces
        .iter()
        .flat_map(|&s| target_pieces.iter().map(move |&t| (s, t)))
        .filter(|&(s, t)| has_same_shape_as(stage, s, t))
        .collect();
    for (s, t) in product_pairs {
        add_transform(
            stage,
            s,
            t,
            &mut anims,
            &mut source_pieces,
            &mut target_pieces,
        );
    }

    // Leftovers: sources fade out to the target's center, targets fade
    // in from the source's center.
    let target_center = stage.get_center(target);
    for piece in source_pieces {
        anims.push(Box::new(fade_out_to_point(stage, piece, target_center)?));
    }
    let source_center = stage.get_center(source);
    for piece in target_pieces {
        anims.push(Box::new(fade_in_from_point(stage, piece, source_center)?));
    }
    Ok(anims)
}

/// Match Scribe primitives by native source-span identity.
///
/// Equal keys are paired in stable source/target occurrence order. A target
/// occurrence is claimed at most once; unpaired source and target primitives
/// use the same directional fades as [`transform_matching_parts`]. The keys
/// are intentionally opaque to Choreo: `fmn-tex` owns their provenance and
/// the portal merely carries that already-native identity across the crate
/// boundary.
///
/// # Errors
/// [`AnimError::StaleHandle`] when either root or any keyed primitive is not
/// live in `stage`, or [`AnimError::Stage`] from a fade constructor.
pub fn transform_matching_keys(
    stage: &mut Stage,
    source: Mob,
    target: Mob,
    source_parts: &[(Mob, String)],
    target_parts: &[(Mob, String)],
) -> Result<Vec<Box<dyn Animation>>, AnimError> {
    for mob in std::iter::once(source)
        .chain(std::iter::once(target))
        .chain(source_parts.iter().map(|(mob, _)| *mob))
        .chain(target_parts.iter().map(|(mob, _)| *mob))
    {
        if !stage.contains(mob) {
            return Err(AnimError::StaleHandle(mob));
        }
    }

    let mut target_claimed = vec![false; target_parts.len()];
    let mut source_unmatched = Vec::new();
    let mut anims: Vec<Box<dyn Animation>> = Vec::new();
    for (source_mob, source_key) in source_parts {
        let target_index = target_parts
            .iter()
            .enumerate()
            .position(|(index, (_, target_key))| {
                !target_claimed[index] && source_key == target_key
            });
        if let Some(index) = target_index {
            target_claimed[index] = true;
            anims.push(Box::new(Transform::new(*source_mob, target_parts[index].0)));
        } else {
            source_unmatched.push(*source_mob);
        }
    }

    let target_center = stage.get_center(target);
    for mob in source_unmatched {
        anims.push(Box::new(fade_out_to_point(stage, mob, target_center)?));
    }
    let source_center = stage.get_center(source);
    for (index, (mob, _)) in target_parts.iter().enumerate() {
        if !target_claimed[index] {
            anims.push(Box::new(fade_in_from_point(stage, *mob, source_center)?));
        }
    }
    Ok(anims)
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    use fmn_mobject::{Mobject, RecordBuffer, RecordSchema};

    fn fixture() -> (Stage, TransformMatchingParts) {
        let mut stage = Stage::new();
        let source = stage.add(Mobject::new());
        for x in [-1.2, 0.2] {
            let child = stage.add(Mobject::vector_from_points(&[
                [x, -0.4, 0.0],
                [x + 0.4, 0.6, 0.0],
                [x + 0.8, -0.4, 0.0],
            ]));
            stage.attach(source, child).unwrap();
        }
        let target = stage.copy_family(source).unwrap();
        stage.shift(target, [0.3, 0.7, 0.0]);
        stage.add_to_scene(source).unwrap();
        let matching = TransformMatchingParts::with_config(
            &mut stage,
            source,
            target,
            &[],
            MatchingPartsConfig {
                lag_ratio: 0.25,
                ..MatchingPartsConfig::default()
            },
        )
        .unwrap();
        (stage, matching)
    }

    #[test]
    fn fresh_matching_clone_preserves_later_playback_and_target_publication() {
        let (stage, mut original) = fixture();
        let before = stage.snapshot_bytes().unwrap();
        let children = original
            .animations()
            .iter()
            .map(|child| {
                let any = child.as_ref() as &dyn std::any::Any;
                Box::new(any.downcast_ref::<Transform>().unwrap().clone()) as Box<dyn Animation>
            })
            .collect();
        let mut cloned = original.clone_for_declaration(children);
        assert!(original.declaration_is_fresh());
        assert!(cloned.declaration_is_fresh());
        assert!(cloned.declaration_families_valid(&stage));
        assert_eq!(stage.snapshot_bytes().unwrap(), before);

        let snapshot = stage.snapshot();
        let mut uninterrupted = snapshot.materialize();
        let mut restored = snapshot.materialize();
        // The source changes after plan construction. Neither declaration
        // cloning nor the frozen target may capture its later start early.
        uninterrupted.shift(original.source.root, [-0.2, 0.1, 0.0]);
        restored.shift(cloned.source.root, [-0.2, 0.1, 0.0]);
        original.prepare_timing(30).unwrap();
        cloned.prepare_timing(30).unwrap();
        original.begin(&mut uninterrupted).unwrap();
        cloned.begin(&mut restored).unwrap();
        assert!(!original.declaration_is_fresh());
        assert!(!cloned.declaration_is_fresh());
        for alpha in [0.0, 0.2, 0.55, 0.9, 1.0] {
            original.interpolate(&mut uninterrupted, alpha);
            cloned.interpolate(&mut restored, alpha);
            assert_eq!(
                uninterrupted.snapshot_bytes().unwrap(),
                restored.snapshot_bytes().unwrap()
            );
        }
        original.finish(&mut uninterrupted);
        cloned.finish(&mut restored);
        original.clean_up_from_scene(&mut uninterrupted);
        cloned.clean_up_from_scene(&mut restored);
        assert_eq!(uninterrupted.roots(), &[original.target()]);
        assert_eq!(restored.roots(), &[cloned.target()]);
        assert_eq!(
            uninterrupted.snapshot_bytes().unwrap(),
            restored.snapshot_bytes().unwrap()
        );
        assert!(!cloned.declaration_is_fresh());
    }

    #[test]
    fn future_matching_manifest_survives_temporary_prefix_geometry_and_topology() {
        let (mut stage, mut matching) = fixture();
        let source = matching.source.root;
        let children = stage.get(source).unwrap().submobjects().to_vec();
        let child = children[0];
        let points = stage.get(child).unwrap().buffer.deep_clone();
        stage.get_mut(child).unwrap().buffer =
            RecordBuffer::new(RecordSchema::vmobject(), 0).unwrap();
        stage.replace_children(source, &[]).unwrap();
        assert!(matching.declaration_is_fresh());
        assert!(matching.declaration_families_valid(&stage));
        assert_eq!(
            matching.validate_begin(&stage),
            Err(AnimError::Stage(StageError::FamilyShapeMismatch))
        );
        stage.get_mut(child).unwrap().buffer = points;
        stage.replace_children(source, &children).unwrap();
        matching.begin(&mut stage).unwrap();
        assert!(!matching.declaration_is_fresh());
    }

    #[test]
    fn declaration_manifest_retains_diamond_order_and_rejects_invalid_inventory() {
        let mut stage = Stage::new();
        let root = stage.add(Mobject::new());
        let left = stage.add(Mobject::new());
        let right = stage.add(Mobject::new());
        let shared = stage.add(Mobject::vector_from_points(&[[0.0; 3]]));
        stage.attach(root, left).unwrap();
        stage.attach(root, right).unwrap();
        stage.attach(left, shared).unwrap();
        stage.attach(right, shared).unwrap();
        let manifest = MatchingFamily::read(&stage, root).unwrap();
        assert!(manifest.declaration_index(&stage).is_some());

        for invalid in 0..7 {
            let (stage, mut matching) = fixture();
            let before = stage.snapshot_bytes().unwrap();
            match invalid {
                0 => matching.source.root = matching.target.root,
                1 => matching
                    .source
                    .layout
                    .push(matching.source.layout[0].clone()),
                2 => matching.source.layout[1].1.push(matching.source.root),
                3 => matching
                    .source
                    .layout
                    .push((matching.target.root, Vec::new(), true)),
                4 => matching.source.layout.swap(1, 2),
                5 => matching.target = matching.source.clone(),
                _ => {
                    let child = matching.source.layout[0].1[0];
                    matching.source.layout[0].1.push(child);
                }
            }
            assert!(
                !matching.declaration_families_valid(&stage),
                "case {invalid}"
            );
            assert_eq!(stage.snapshot_bytes().unwrap(), before);
        }
        let (mut stage, matching) = fixture();
        stage.delete(matching.source.layout[1].0).unwrap();
        assert!(!matching.declaration_families_valid(&stage));
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use fmn_mobject::Mobject;

    fn explicit_family_fixture(padded: bool) -> (Stage, TransformMatchingParts) {
        let mut stage = Stage::new();
        let source = stage.add(Mobject::vector_from_points(&[]));
        let source_child = stage.add(Mobject::vector_from_points(&[
            [-1.0, -0.4, 0.0],
            [-0.6, 0.5, 0.0],
            [-0.2, -0.4, 0.0],
        ]));
        stage.attach(source, source_child).unwrap();
        let target = stage.add(Mobject::vector_from_points(&[
            [0.2, -0.2, 0.0],
            [0.6, 0.8, 0.0],
            [1.0, -0.2, 0.0],
        ]));
        let target_child = stage.copy_family(source_child).unwrap();
        stage.shift(target_child, [0.4, 0.6, 0.0]);
        stage.attach(target, target_child).unwrap();
        if padded {
            let extra = stage.copy_family(target_child).unwrap();
            stage.shift(extra, [0.0, -0.7, 0.0]);
            stage.attach(target, extra).unwrap();
        }
        stage.add_to_scene(source).unwrap();
        let mut matching = TransformMatchingParts::with_matched_pairs(
            &mut stage,
            source,
            target,
            &[(source, target)],
        )
        .unwrap();
        // Explicit pairs claim all point-bearing descendants of a root,
        // even when that root itself starts with an empty vector buffer.
        assert_eq!(matching.animations().len(), 1);
        let child = (matching.animations()[0].as_ref() as &dyn std::any::Any)
            .downcast_ref::<Transform>()
            .unwrap();
        assert_eq!(child.state.mobject(), source);
        assert_eq!(child.target, target);
        matching.state_mut().config.run_time_frames = Some(7);
        matching.state_mut().config.final_alpha_value = 0.35;
        (stage, matching)
    }

    fn cloned_children(matching: &TransformMatchingParts) -> Vec<Box<dyn Animation>> {
        matching
            .animations()
            .iter()
            .map(|child| {
                let any = child.as_ref() as &dyn std::any::Any;
                Box::new(any.downcast_ref::<Transform>().unwrap().clone()) as Box<dyn Animation>
            })
            .collect()
    }

    #[test]
    fn active_matching_reconstruction_preserves_aligned_state_and_original_target_cleanup() {
        let (mut stage, mut original) = explicit_family_fixture(false);
        let original_target_points = stage.get_points(original.target()).unwrap();
        original.prepare_timing(30).unwrap();
        original.begin(&mut stage).unwrap();
        original.interpolate(&mut stage, 0.3);
        assert!(!stage.has_padding_sources());
        assert!(!stage.get(original.source.root).unwrap().buffer.is_empty());
        assert_eq!(
            original.source.validate(&stage),
            Err(AnimError::Stage(StageError::FamilyShapeMismatch))
        );
        assert!(original.declaration_families_valid(&stage));
        assert!(original.continuation_lifecycle_valid(&stage));

        let before = stage.snapshot_bytes().unwrap();
        let group = AnimationGroup::from_continuation(
            original.state().clone(),
            cloned_children(&original),
            original.group.continuation_controls(),
            30,
        )
        .unwrap();
        let mut continued = TransformMatchingParts::from_continuation(
            group,
            (original.source.root, original.source.layout.clone()),
            (original.target.root, original.target.layout.clone()),
        );
        assert_eq!(stage.snapshot_bytes().unwrap(), before);
        assert!(continued.continuation_lifecycle_valid(&stage));
        assert!(continued.group.continuation_timing_valid(30));
        assert!(continued.declaration_families_valid(&stage));
        assert!(!continued.declaration_is_fresh());
        assert!(continued.freeze_interpolation().is_some());

        let mut restored = stage.snapshot().materialize();
        for alpha in [0.4, 0.75, 1.0] {
            original.interpolate(&mut stage, alpha);
            continued.interpolate(&mut restored, alpha);
            assert_eq!(
                stage.snapshot_bytes().unwrap(),
                restored.snapshot_bytes().unwrap()
            );
        }
        original.finish(&mut stage);
        continued.finish(&mut restored);
        assert!(!continued.continuation_lifecycle_valid(&restored));
        original.clean_up_from_scene(&mut stage);
        continued.clean_up_from_scene(&mut restored);
        assert_eq!(restored.roots(), &[original.target()]);
        assert_eq!(
            restored.get_points(original.target()).unwrap(),
            original_target_points
        );
        assert_eq!(
            stage.snapshot_bytes().unwrap(),
            restored.snapshot_bytes().unwrap()
        );
        for member in restored.family(continued.state().mobject()) {
            assert!(!restored.is_animating(member));
            assert!(!restored.is_updating_suspended(member));
        }
        let completed = restored.snapshot_bytes().unwrap();
        continued.finish(&mut restored);
        continued.clean_up_from_scene(&mut restored);
        assert_eq!(restored.snapshot_bytes().unwrap(), completed);
    }

    #[test]
    fn active_matching_requires_pristine_unfinished_lifecycle_flags() {
        let (mut stage, mut matching) = explicit_family_fixture(false);
        assert!(!matching.continuation_lifecycle_valid(&stage));
        matching.prepare_timing(30).unwrap();
        assert!(!matching.continuation_lifecycle_valid(&stage));
        matching.begin(&mut stage).unwrap();
        assert!(matching.continuation_lifecycle_valid(&stage));
        let before = stage.snapshot_bytes().unwrap();
        for invalid in 0..4 {
            let mut bad = matching.clone_for_continuation(cloned_children(&matching));
            match invalid {
                0 => bad.begun = false,
                1 => bad.finished = true,
                2 => bad.cleaned = true,
                _ => bad.error = Some(AnimError::InvalidFramePhase("latched failure")),
            }
            assert!(!bad.continuation_lifecycle_valid(&stage), "case {invalid}");
            assert!(bad.freeze_interpolation().is_none(), "case {invalid}");
            assert_eq!(stage.snapshot_bytes().unwrap(), before);
        }
        matching.abort(&mut stage);
        assert!(!matching.continuation_lifecycle_valid(&stage));
        assert!(!stage.roots().contains(&matching.target()));
    }

    #[test]
    fn active_matching_manifest_survives_unequal_family_alignment_without_hiding_padding() {
        let (mut stage, mut matching) = explicit_family_fixture(true);
        let original_members = matching.source.layout.len();
        matching.prepare_timing(30).unwrap();
        matching.begin(&mut stage).unwrap();
        assert!(stage.family(matching.source.root).len() > original_members);
        assert!(stage.has_padding_sources());
        assert!(matching.source.validate(&stage).is_err());
        assert!(matching.declaration_families_valid(&stage));
        assert!(matching.continuation_lifecycle_valid(&stage));
        // These links are deliberately outside the durable SceneState
        // document. Studio must continue to choose verified cold recovery.
        assert!(
            stage
                .family(matching.source.root)
                .iter()
                .any(|&member| { stage.padding_source(member).is_some() })
        );
    }
}
