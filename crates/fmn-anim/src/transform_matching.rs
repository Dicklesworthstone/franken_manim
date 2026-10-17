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
struct MatchingFamily {
    root: Mob,
    layout: Vec<(Mob, Vec<Mob>, bool)>,
}

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
