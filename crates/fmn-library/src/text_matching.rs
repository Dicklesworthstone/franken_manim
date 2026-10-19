//! Source-span matching for native Rust scenes (§11.3, D-09).
//!
//! Save [`TexMobject::span_map`](crate::TexMobject::span_map) or
//! [`TextMobject::span_map`](crate::TextMobject::span_map) before moving the
//! built value into a Stage. The map binds to that root's direct children;
//! moving or restyling them is fine, but changing their topology invalidates
//! the binding. Text decorations after the mapped glyphs fade as unmatched
//! pieces. Matching never infers text identity from glyph geometry.
//!
//! [`TransformMatchingTex`] pairs equal native source slices in occurrence
//! order. [`TransformMatchingStrings`] repeatedly claims the longest common
//! contiguous run, so reordered words can move together. Claimed slots remain
//! barriers: removing them would invent substrings that never existed.
//! Explicit `with_key_map` mappings claim complete source-span occurrences
//! first, including expressions containing different numbers of primitives.
//! Both are ordinary Choreo animations: groups of Transform and directional
//! fades, followed by removal of the source and publication of the real target.
//! Their prepared interpolation can run on native frame workers; source/target
//! validation, begin, finish and replacement cleanup stay on the scene owner.
//! A failed callback prevents replacement publication. Its first diagnostic
//! remains attached to the failed run until a validated new begin.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use fmn_anim::{
    AnimError, AnimState, Animation, AnimationGroup, AnimationSignature, RateFunc, Transform,
    fade_in_from_point, fade_out_to_point,
};
use fmn_mobject::{IdBuildHasher, Mob, Mobject, Stage, StageError};

use crate::spans::SpanMapData;

/// Maximum direct children on either operand of a matching animation.
pub const MAX_MATCHING_PARTS: usize = 65_536;
/// Maximum equal-key comparisons in a longest-block search, across all rounds.
pub const MAX_MATCHING_COMPARISONS: usize = 1_048_576;
/// Maximum authored expression mappings in one animation.
pub const MAX_MATCHING_KEY_MAP_ENTRIES: usize = 4096;
/// Maximum source/pattern bytes and native-part probes in a mapping search.
pub const MAX_MATCHING_MAPPING_WORK: usize = 16_777_216;

/// Timing of a native text-matching composition. Child transforms retain their
/// normal smooth rate curve; `rate_func` shapes the composition's timeline.
#[derive(Clone, Debug)]
pub struct TextMatchingConfig {
    /// Total duration in seconds (the Reference's matching default is two).
    pub run_time: f64,
    /// Stagger between matched blocks and unmatched pieces.
    pub lag_ratio: f64,
    /// Composition rate curve; linear avoids applying smooth twice.
    pub rate_func: RateFunc,
}

impl Default for TextMatchingConfig {
    fn default() -> Self {
        Self {
            run_time: 2.0,
            lag_ratio: 0.0,
            rate_func: RateFunc::linear(),
        }
    }
}

/// A matching plan was refused before any groups or animations were created.
#[derive(Debug)]
pub enum TextMatchingError {
    /// A Choreo or arena operation failed.
    Animation(AnimError),
    /// A native source span is empty, out of bounds, or splits UTF-8.
    InvalidSpan {
        /// `source` or `target`.
        side: &'static str,
        /// Span ordinal.
        index: usize,
    },
    /// The family no longer has the native map's direct-child layout.
    InvalidLayout {
        /// `source` or `target`.
        side: &'static str,
        /// The violated binding contract.
        reason: &'static str,
    },
    /// An explicit mapping has an empty key or repeats a source key.
    InvalidKeyMap {
        /// Mapping ordinal, in caller order.
        index: usize,
        /// The violated mapping contract.
        reason: &'static str,
    },
    /// Both operands contain the same live member. Copy the target first.
    AliasedFamilies,
    /// Matching exceeded a fixed resource ceiling.
    BudgetExceeded {
        /// The bounded resource.
        resource: &'static str,
        /// Its ceiling.
        limit: usize,
    },
}

impl std::fmt::Display for TextMatchingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Animation(error) => error.fmt(f),
            Self::InvalidSpan { side, index } => {
                write!(
                    f,
                    "{side} native span {index} is not a nonempty UTF-8 source range"
                )
            }
            Self::InvalidLayout { side, reason } => {
                write!(f, "{side} native span layout is invalid: {reason}")
            }
            Self::InvalidKeyMap { index, reason } => {
                write!(f, "native text key mapping {index} is invalid: {reason}")
            }
            Self::AliasedFamilies => {
                write!(
                    f,
                    "matching operands share live members; copy the target first"
                )
            }
            Self::BudgetExceeded { resource, limit } => {
                write!(f, "text matching exceeds the {limit} {resource} budget")
            }
        }
    }
}

impl std::error::Error for TextMatchingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Animation(error) => Some(error),
            _ => None,
        }
    }
}

impl From<AnimError> for TextMatchingError {
    fn from(error: AnimError) -> Self {
        Self::Animation(error)
    }
}

struct Binding {
    root: Mob,
    children: Vec<Mob>,
}

impl Binding {
    fn read<'a>(
        stage: &Stage,
        root: Mob,
        spans: &'a SpanMapData,
        side: &'static str,
    ) -> Result<(Self, Vec<&'a str>), TextMatchingError> {
        let entry = stage.get(root).ok_or(AnimError::StaleHandle(root))?;
        let children = entry.submobjects();
        let invalid = |reason| TextMatchingError::InvalidLayout { side, reason };
        if children.len() > MAX_MATCHING_PARTS {
            return Err(TextMatchingError::BudgetExceeded {
                resource: "parts per operand",
                limit: MAX_MATCHING_PARTS,
            });
        }
        if !entry.buffer.is_empty() {
            return Err(invalid(
                "the source-mapped root must be a point-free container",
            ));
        }
        if spans.entries.len() > children.len() {
            return Err(invalid("there are more spans than direct children"));
        }
        for &child in children {
            let part = stage.get(child).ok_or(AnimError::StaleHandle(child))?;
            if !part.submobjects().is_empty() {
                return Err(invalid(
                    "a native primitive was regrouped; rebuild its span map",
                ));
            }
            if spans.entries.is_empty() && !part.buffer.is_empty() {
                return Err(invalid("point-bearing strings require a native span map"));
            }
        }
        let keys = spans
            .entries
            .iter()
            .enumerate()
            .map(|(index, span)| {
                spans
                    .source
                    .get(span.start..span.end)
                    .filter(|key| !key.is_empty())
                    .ok_or(TextMatchingError::InvalidSpan { side, index })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok((
            Self {
                root,
                children: children.to_vec(),
            },
            keys,
        ))
    }

    fn validate(&self, stage: &Stage) -> Result<(), AnimError> {
        let entry = stage
            .get(self.root)
            .ok_or(AnimError::StaleHandle(self.root))?;
        if entry.submobjects() != self.children.as_slice() || !entry.buffer.is_empty() {
            return Err(AnimError::Stage(StageError::FamilyShapeMismatch));
        }
        for &child in &self.children {
            let part = stage.get(child).ok_or(AnimError::StaleHandle(child))?;
            if !part.submobjects().is_empty() {
                return Err(AnimError::Stage(StageError::FamilyShapeMismatch));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum MatchingKind {
    Tex,
    Strings,
}

type PartPair = (Vec<usize>, Vec<usize>);

fn tex_pairs(source: &[&str], target: &[&str]) -> Vec<PartPair> {
    tex_pairs_remaining(
        source,
        target,
        &vec![false; source.len()],
        &vec![false; target.len()],
    )
}

fn tex_pairs_remaining(
    source: &[&str],
    target: &[&str],
    used_source: &[bool],
    used_target: &[bool],
) -> Vec<PartPair> {
    let mut available: BTreeMap<&str, VecDeque<usize>> = BTreeMap::new();
    for (index, key) in target.iter().enumerate() {
        if !used_target[index] {
            available.entry(*key).or_default().push_back(index);
        }
    }
    source
        .iter()
        .enumerate()
        .filter_map(|(index, key)| {
            if used_source[index] {
                return None;
            }
            let other = available.get_mut(*key)?.pop_front()?;
            Some((vec![index], vec![other]))
        })
        .collect()
}

/// SequenceMatcher's longest contiguous block, with deterministic earliest
/// source/target tie breaking. Index equal keys rather than scanning the full
/// Cartesian product; used slots are barriers, not deleted sequence entries.
fn string_pairs(source: &[&str], target: &[&str]) -> Result<Vec<PartPair>, TextMatchingError> {
    string_pairs_remaining(
        source,
        target,
        &vec![false; source.len()],
        &vec![false; target.len()],
    )
}

fn string_pairs_remaining(
    source: &[&str],
    target: &[&str],
    source_claims: &[bool],
    target_claims: &[bool],
) -> Result<Vec<PartPair>, TextMatchingError> {
    if source.is_empty() || target.is_empty() {
        return Ok(Vec::new());
    }
    if source == target
        && !source_claims.iter().any(|used| *used)
        && !target_claims.iter().any(|used| *used)
    {
        return Ok(vec![(
            (0..source.len()).collect(),
            (0..target.len()).collect(),
        )]);
    }
    let mut positions: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (index, key) in target.iter().enumerate() {
        if !target_claims[index] {
            positions.entry(*key).or_default().push(index);
        }
    }
    let mut used_source = source_claims.to_vec();
    let mut used_target = target_claims.to_vec();
    let mut pairs = Vec::new();
    let mut comparisons = 0;
    loop {
        let mut previous: BTreeMap<usize, usize> = BTreeMap::new();
        let mut best = (0, 0, 0);
        for (i, key) in source.iter().enumerate() {
            let mut current = BTreeMap::new();
            if !used_source[i] {
                for &j in positions.get(*key).into_iter().flatten() {
                    if used_target[j] {
                        continue;
                    }
                    comparisons += 1;
                    if comparisons > MAX_MATCHING_COMPARISONS {
                        return Err(TextMatchingError::BudgetExceeded {
                            resource: "equal-key comparisons",
                            limit: MAX_MATCHING_COMPARISONS,
                        });
                    }
                    let length = j
                        .checked_sub(1)
                        .and_then(|before| previous.get(&before))
                        .copied()
                        .unwrap_or(0)
                        + 1;
                    current.insert(j, length);
                    if length > best.2 {
                        best = (i + 1 - length, j + 1 - length, length);
                    }
                }
            }
            previous = current;
        }
        let (a, b, length) = best;
        if length == 0 {
            return Ok(pairs);
        }
        used_source[a..a + length].fill(true);
        used_target[b..b + length].fill(true);
        pairs.push(((a..a + length).collect(), (b..b + length).collect()));
    }
}

fn charge_mapping_work(work: &mut usize, amount: usize) -> Result<(), TextMatchingError> {
    *work = work.saturating_add(amount);
    if *work > MAX_MATCHING_MAPPING_WORK {
        return Err(TextMatchingError::BudgetExceeded {
            resource: "key-map search work",
            limit: MAX_MATCHING_MAPPING_WORK,
        });
    }
    Ok(())
}

/// Locate literal occurrences once, then assign each native part to the
/// containing occurrence by binary search. This avoids rescanning every part
/// for every repeated symbol and retains painter order within each group.
fn available_occurrences(
    spans: &SpanMapData,
    needle: &str,
    used: &[bool],
    work: &mut usize,
) -> Result<Vec<Vec<usize>>, TextMatchingError> {
    charge_mapping_work(work, spans.source.len().saturating_add(needle.len()))?;
    let mut ranges = Vec::new();
    for (start, text) in spans.source.match_indices(needle) {
        if ranges.len() == MAX_MATCHING_PARTS {
            return Err(TextMatchingError::BudgetExceeded {
                resource: "literal occurrences per mapping operand",
                limit: MAX_MATCHING_PARTS,
            });
        }
        ranges.push((start, start + text.len()));
    }
    if ranges.is_empty() {
        return Ok(Vec::new());
    }
    let mut groups = vec![Vec::new(); ranges.len()];
    let mut blocked = vec![false; ranges.len()];
    for (index, span) in spans.entries.iter().enumerate() {
        charge_mapping_work(work, 1)?;
        let occurrence = ranges
            .partition_point(|&(start, _)| start <= span.start)
            .checked_sub(1);
        if let Some(occurrence) = occurrence
            && span.end <= ranges[occurrence].1
        {
            groups[occurrence].push(index);
            blocked[occurrence] |= used[index];
        }
    }
    Ok(groups
        .into_iter()
        .zip(blocked)
        .filter_map(|(group, blocked)| (!blocked && !group.is_empty()).then_some(group))
        .collect())
}

fn matching_pairs(
    source: &SpanMapData,
    target: &SpanMapData,
    source_keys: &[&str],
    target_keys: &[&str],
    kind: MatchingKind,
    key_map: &[(&str, &str)],
) -> Result<Vec<PartPair>, TextMatchingError> {
    if key_map.is_empty() {
        return match kind {
            MatchingKind::Tex => Ok(tex_pairs(source_keys, target_keys)),
            MatchingKind::Strings => string_pairs(source_keys, target_keys),
        };
    }
    if key_map.len() > MAX_MATCHING_KEY_MAP_ENTRIES {
        return Err(TextMatchingError::BudgetExceeded {
            resource: "key-map entries",
            limit: MAX_MATCHING_KEY_MAP_ENTRIES,
        });
    }
    let mut seen = BTreeSet::new();
    let mut work = 0;
    for (index, &(from, to)) in key_map.iter().enumerate() {
        charge_mapping_work(&mut work, from.len().saturating_add(to.len()))?;
        if from.is_empty() || to.is_empty() {
            return Err(TextMatchingError::InvalidKeyMap {
                index,
                reason: "both source and target keys must be nonempty",
            });
        }
        if !seen.insert(from) {
            return Err(TextMatchingError::InvalidKeyMap {
                index,
                reason: "a source key may appear only once",
            });
        }
    }
    let mut used_source = vec![false; source_keys.len()];
    let mut used_target = vec![false; target_keys.len()];
    let mut pairs = Vec::new();
    for &(from, to) in key_map {
        let sources = available_occurrences(source, from, &used_source, &mut work)?;
        let targets = available_occurrences(target, to, &used_target, &mut work)?;
        for (a, b) in sources.into_iter().zip(targets) {
            for &index in &a {
                used_source[index] = true;
            }
            for &index in &b {
                used_target[index] = true;
            }
            pairs.push((a, b));
        }
    }
    let remaining = match kind {
        MatchingKind::Tex => tex_pairs_remaining(source_keys, target_keys, &used_source, &used_target),
        MatchingKind::Strings => {
            string_pairs_remaining(source_keys, target_keys, &used_source, &used_target)?
        }
    };
    pairs.extend(remaining);
    Ok(pairs)
}

fn part_group(
    stage: &mut Stage,
    children: &[Mob],
    indices: &[usize],
    wrap_singleton: bool,
) -> Result<Mob, AnimError> {
    if indices.len() == 1 && !wrap_singleton {
        return Ok(children[indices[0]]);
    }
    let group = stage.add(Mobject::new());
    let members: Vec<Mob> = indices.iter().map(|&index| children[index]).collect();
    stage.replace_children(group, &members)?;
    Ok(group)
}

struct MatchingAnimation {
    group: AnimationGroup,
    source: Binding,
    target: Binding,
    begun: bool,
    finished: bool,
    cleaned: bool,
    error: Option<AnimError>,
}

impl MatchingAnimation {
    fn new(
        stage: &mut Stage,
        source: Mob,
        target: Mob,
        source_spans: &SpanMapData,
        target_spans: &SpanMapData,
        config: TextMatchingConfig,
        plan: (MatchingKind, &[(&str, &str)]),
    ) -> Result<Self, TextMatchingError> {
        let (kind, key_map) = plan;
        let (source, source_keys) = Binding::read(stage, source, source_spans, "source")?;
        let (target, target_keys) = Binding::read(stage, target, target_spans, "target")?;
        let source_family: HashSet<Mob, IdBuildHasher> = std::iter::once(source.root)
            .chain(source.children.iter().copied())
            .collect();
        if std::iter::once(target.root)
            .chain(target.children.iter().copied())
            .any(|mob| source_family.contains(&mob))
        {
            return Err(TextMatchingError::AliasedFamilies);
        }
        // Finish every fallible provenance/search check before editing the arena.
        let pairs = matching_pairs(
            source_spans,
            target_spans,
            &source_keys,
            &target_keys,
            kind,
            key_map,
        )?;
        let source_center = stage.get_center(source.root);
        let target_center = stage.get_center(target.root);
        let mut used_source = vec![false; source.children.len()];
        let mut used_target = vec![false; target.children.len()];
        let mut animations: Vec<Box<dyn Animation>> = Vec::new();
        for (a, b) in pairs {
            // Family alignment for a one-to-many substitution belongs to the
            // temporary groups, not to an original leaf bound by a native map.
            let wrap_singleton = a.len() != b.len();
            let from = part_group(stage, &source.children, &a, wrap_singleton)?;
            let to = part_group(stage, &target.children, &b, wrap_singleton)?;
            animations.push(Box::new(Transform::new(from, to)));
            for index in a {
                used_source[index] = true;
            }
            for index in b {
                used_target[index] = true;
            }
        }
        for (index, &mob) in source.children.iter().enumerate() {
            if !used_source[index] {
                animations.push(Box::new(fade_out_to_point(stage, mob, target_center)?));
            }
        }
        for (index, &mob) in target.children.iter().enumerate() {
            if !used_target[index] {
                animations.push(Box::new(fade_in_from_point(stage, mob, source_center)?));
            }
        }
        // Empty text still replaces its root and consumes the authored time.
        // A self-transform of the point-free source uses the ordinary lifecycle.
        if animations.is_empty() {
            animations.push(Box::new(Transform::new(source.root, source.root)));
        }
        let name = match kind {
            MatchingKind::Tex => "TransformMatchingTex",
            MatchingKind::Strings => "TransformMatchingStrings",
        };
        let group = AnimationGroup::with_lag_ratio(stage, animations, config.lag_ratio)?
            .with_run_time(config.run_time)
            .with_rate_func(config.rate_func)
            .with_name(name);
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

    fn inventory(&self, mut members: Vec<Mob>) -> Vec<Mob> {
        for mob in [self.source.root, self.target.root] {
            if !members.contains(&mob) {
                members.push(mob);
            }
        }
        members
    }

    /// A void callback's first failure belongs to this run even after the
    /// host acknowledges the Stage fault. Cleanup is publication, not recovery.
    fn retain_error(&mut self, stage: &Stage) -> bool {
        if self.error.is_none() {
            self.error = self
                .group
                .deferred_error()
                .or_else(|| stage.updater_error().cloned().map(AnimError::from));
        }
        self.error.is_some()
    }
}

macro_rules! matching_animation {
    ($name:ident, $kind:ident, $description:literal) => {
        #[doc = $description]
        pub struct $name {
            inner: MatchingAnimation,
        }

        impl $name {
            /// Bind the two native source maps and build a two-second animation.
            /// Neither target rooting nor interpolation happens at construction.
            ///
            /// # Errors
            /// Invalid/stale maps, shared operands, or exceeded search budgets.
            pub fn new(
                stage: &mut Stage,
                source: Mob,
                target: Mob,
                source_spans: &SpanMapData,
                target_spans: &SpanMapData,
            ) -> Result<Self, TextMatchingError> {
                Self::with_config(
                    stage,
                    source,
                    target,
                    source_spans,
                    target_spans,
                    TextMatchingConfig::default(),
                )
            }

            /// Build with explicit composition timing.
            ///
            /// # Errors
            /// As [`Self::new`].
            pub fn with_config(
                stage: &mut Stage,
                source: Mob,
                target: Mob,
                source_spans: &SpanMapData,
                target_spans: &SpanMapData,
                config: TextMatchingConfig,
            ) -> Result<Self, TextMatchingError> {
                Self::with_key_map(
                    stage,
                    source,
                    target,
                    source_spans,
                    target_spans,
                    &[],
                    config,
                )
            }

            /// Map literal expressions before automatically matching leftovers.
            ///
            /// For example, `&[("x", "y"), ("ab", "z")]` renames every available
            /// `x` occurrence and transforms each available `ab` run as one unit.
            /// Keys refer to the verbatim UTF-8 source, including TeX commands.
            /// Only whole native spans participate: a substring of a command or
            /// ligature cannot take partial ink. Repeated occurrences pair in
            /// source order; primitive order inside each occurrence is retained.
            ///
            /// Caller order resolves overlapping mappings. Once any primitive
            /// of an occurrence is claimed, that entire occurrence is unavailable
            /// to later mappings. Missing or ink-free occurrences are skipped.
            /// Unpaired occurrences remain available to automatic matching/fades.
            /// An empty map has exactly [`Self::with_config`]'s behavior.
            ///
            /// # Errors
            /// As [`Self::new`], plus empty keys, duplicate source keys, or a
            /// mapping search exceeding its fixed work/storage ceilings. These
            /// refusals happen before any arena mutation.
            pub fn with_key_map(
                stage: &mut Stage,
                source: Mob,
                target: Mob,
                source_spans: &SpanMapData,
                target_spans: &SpanMapData,
                key_map: &[(&str, &str)],
                config: TextMatchingConfig,
            ) -> Result<Self, TextMatchingError> {
                Ok(Self {
                    inner: MatchingAnimation::new(
                        stage,
                        source,
                        target,
                        source_spans,
                        target_spans,
                        config,
                        (MatchingKind::$kind, key_map),
                    )?,
                })
            }

            /// The actual Transform/fade plan, in composition order.
            #[must_use]
            pub fn animations(&self) -> &[Box<dyn Animation>] {
                self.inner.group.animations()
            }

            /// The original target, published on successful cleanup.
            #[must_use]
            pub fn target(&self) -> Mob {
                self.inner.target.root
            }
        }

        impl Animation for $name {
            fn state(&self) -> &AnimState {
                self.inner.group.state()
            }
            fn state_mut(&mut self) -> &mut AnimState {
                self.inner.group.state_mut()
            }
            fn all_mobjects(&self) -> Vec<Mob> {
                self.inner.inventory(self.inner.group.all_mobjects())
            }
            fn preflight_mobjects(&self) -> Vec<Mob> {
                self.inner.inventory(self.inner.group.preflight_mobjects())
            }
            fn validate_begin(&self, stage: &Stage) -> Result<(), AnimError> {
                self.inner.source.validate(stage)?;
                self.inner.target.validate(stage)?;
                self.inner.group.validate_begin(stage)
            }
            fn begin(&mut self, stage: &mut Stage) -> Result<(), AnimError> {
                self.validate_begin(stage)?;
                self.inner.begun = false;
                self.inner.finished = false;
                self.inner.cleaned = false;
                self.inner.error = None;
                if let Err(error) = self.inner.group.begin(stage) {
                    self.inner.error = Some(error.clone());
                    self.inner.group.abort(stage);
                    return Err(error);
                }
                self.inner.begun = true;
                if self.inner.retain_error(stage) {
                    self.inner.group.abort(stage);
                    self.inner.begun = false;
                    return Err(self.inner.error.clone().expect("retained matching failure"));
                }
                Ok(())
            }
            fn interpolate_submobject(&mut self, _stage: &mut Stage, _mobs: &[Mob], _alpha: f64) {}
            fn interpolate(&mut self, stage: &mut Stage, alpha: f64) {
                if !self.inner.retain_error(stage) {
                    self.inner.group.interpolate(stage, alpha);
                    self.inner.retain_error(stage);
                }
            }
            fn update_mobjects(&mut self, stage: &mut Stage, dt: f64) {
                if !self.inner.retain_error(stage) {
                    self.inner.group.update_mobjects(stage, dt);
                    self.inner.retain_error(stage);
                }
            }
            fn finish(&mut self, stage: &mut Stage) {
                if self.inner.begun && !self.inner.finished {
                    if self.inner.retain_error(stage) {
                        self.inner.group.abort(stage);
                        return;
                    }
                    self.inner.group.finish(stage);
                    self.inner.finished = !self.inner.retain_error(stage);
                }
            }
            fn abort(&mut self, stage: &mut Stage) {
                self.inner.retain_error(stage);
                self.inner.group.abort(stage);
                self.inner.retain_error(stage);
                self.inner.begun = false;
                self.inner.finished = false;
            }
            fn clean_up_from_scene(&mut self, stage: &mut Stage) {
                if self.inner.cleaned || self.inner.retain_error(stage) {
                    return;
                }
                if !self.inner.finished {
                    self.inner.error = Some(AnimError::InvalidFramePhase(
                        "text matching must finish before scene cleanup",
                    ));
                    return;
                }
                if !stage.contains(self.inner.target.root) {
                    self.inner.error = Some(AnimError::StaleHandle(self.inner.target.root));
                    return;
                }
                self.inner.group.clean_up_from_scene(stage);
                if self.inner.retain_error(stage) {
                    return;
                }
                stage.remove_many_from_scene(&[self.state().mobject(), self.inner.source.root]);
                if let Err(error) = stage.add_to_scene(self.inner.target.root) {
                    self.inner.error = Some(AnimError::Stage(error));
                    return;
                }
                self.inner.cleaned = true;
            }
            fn deferred_error(&self) -> Option<AnimError> {
                self.inner
                    .error
                    .clone()
                    .or_else(|| self.inner.group.deferred_error())
            }
            fn collect_resumed_updater_mobjects(&self, out: &mut Vec<Mob>) {
                self.inner.group.collect_resumed_updater_mobjects(out);
            }
            fn effect_signature(&self) -> AnimationSignature {
                self.inner.group.effect_signature()
            }
            fn freeze_interpolation(&self) -> Option<fmn_anim::pure_frame::FrozenAnimation> {
                if !self.inner.begun
                    || self.inner.finished
                    || self.inner.cleaned
                    || self.inner.error.is_some()
                    || self.inner.group.deferred_error().is_some()
                {
                    return None;
                }
                // All members began eagerly. Freeze their existing native
                // interpolation; rebuilding the matching plan on a worker
                // would repeat allocation and source/target lifecycle work.
                // PureSegment still checks the full operand inventory for
                // live views, updaters and aliases with other animations.
                self.inner.group.freeze_interpolation()
            }
        }
    };
}

matching_animation!(
    TransformMatchingTex,
    Tex,
    "Match mathematical primitives by native source identity, never by outline similarity."
);
matching_animation!(
    TransformMatchingStrings,
    Strings,
    "Move shared string runs together, including runs reordered between source and target."
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spans::{SpanKindU8, SpanMapEntry};

    fn spans(source: &str) -> SpanMapData {
        SpanMapData {
            source: source.into(),
            entries: source
                .char_indices()
                .map(|(start, ch)| SpanMapEntry {
                    start,
                    end: start + ch.len_utf8(),
                    kind: SpanKindU8::TextGlyph,
                })
                .collect(),
        }
    }

    fn plan(
        source: &SpanMapData,
        target: &SpanMapData,
        kind: MatchingKind,
        mappings: &[(&str, &str)],
    ) -> Result<Vec<PartPair>, TextMatchingError> {
        let keys = |map: &SpanMapData| {
            map.entries
                .iter()
                .map(|entry| map.source[entry.start..entry.end].to_owned())
                .collect::<Vec<_>>()
        };
        let a = keys(source);
        let b = keys(target);
        matching_pairs(
            source,
            target,
            &a.iter().map(String::as_str).collect::<Vec<_>>(),
            &b.iter().map(String::as_str).collect::<Vec<_>>(),
            kind,
            mappings,
        )
    }

    #[test]
    fn tex_repeated_keys_claim_in_occurrence_order() {
        assert_eq!(
            tex_pairs(&["x", "x", "+"], &["x", "+", "x"]),
            [(vec![0], vec![0]), (vec![1], vec![2]), (vec![2], vec![1])]
        );
    }

    #[test]
    fn longest_block_ties_choose_earliest_source_then_target() {
        assert_eq!(
            string_pairs(&["A", "B", "A"], &["B", "A", "B"]).unwrap(),
            [(vec![0, 1], vec![1, 2])]
        );
        assert_eq!(
            string_pairs(&["A"], &["A", "A"]).unwrap(),
            [(vec![0], vec![0])]
        );
    }

    #[test]
    fn identical_large_inputs_have_a_linear_identity_path() {
        let keys = vec!["A"; 65_536];
        let pairs = string_pairs(&keys, &keys).unwrap();
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].0.len(), keys.len());
        assert_eq!(pairs[0].0, pairs[0].1);
    }

    #[test]
    fn pathological_repeated_keys_refuse_bounded_search() {
        assert!(matches!(
            string_pairs(&vec!["A"; 1025], &vec!["A"; 1026]),
            Err(TextMatchingError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn explicit_renames_precede_identity_and_claim_repeated_occurrences() {
        for kind in [MatchingKind::Tex, MatchingKind::Strings] {
            assert_eq!(
                plan(&spans("x+x"), &spans("y+y+x"), kind, &[("x", "y")]).unwrap(),
                [(vec![0], vec![0]), (vec![2], vec![2]), (vec![1], vec![1])]
            );
        }
    }

    #[test]
    fn explicit_swaps_do_not_lose_targets_to_automatic_matching() {
        assert_eq!(
            plan(
                &spans("xy"),
                &spans("xy"),
                MatchingKind::Tex,
                &[("x", "y"), ("y", "x")],
            )
            .unwrap(),
            [(vec![0], vec![1]), (vec![1], vec![0])]
        );
    }

    #[test]
    fn unequal_expression_lengths_match_as_whole_occurrences() {
        assert_eq!(
            plan(
                &spans("ab+ab"),
                &spans("z+uvw"),
                MatchingKind::Strings,
                &[("ab", "z")],
            )
            .unwrap(),
            [(vec![0, 1], vec![0]), (vec![2], vec![1])]
        );
        assert_eq!(
            plan(&spans("z"), &spans("uvw"), MatchingKind::Tex, &[("z", "uvw")]).unwrap(),
            [(vec![0], vec![0, 1, 2])]
        );
    }

    #[test]
    fn claims_are_barriers_not_deleted_characters() {
        assert_eq!(
            plan(
                &spans("axb"),
                &spans("abz"),
                MatchingKind::Strings,
                &[("x", "z")],
            )
            .unwrap(),
            [(vec![1], vec![2]), (vec![0], vec![0]), (vec![2], vec![1])]
        );
    }

    #[test]
    fn caller_order_resolves_overlaps_without_partial_expressions() {
        assert_eq!(
            plan(
                &spans("abc"),
                &spans("xy"),
                MatchingKind::Tex,
                &[("ab", "x"), ("bc", "y")],
            )
            .unwrap(),
            [(vec![0, 1], vec![0])]
        );
    }

    #[test]
    fn shared_target_keys_consume_distinct_available_occurrences() {
        assert_eq!(
            plan(
                &spans("ab"),
                &spans("xx"),
                MatchingKind::Tex,
                &[("a", "x"), ("b", "x")],
            )
            .unwrap(),
            [(vec![0], vec![0]), (vec![1], vec![1])]
        );
    }

    #[test]
    fn unicode_mapping_uses_byte_spans_not_character_offsets() {
        assert_eq!(
            plan(
                &spans("αβ+αβ"),
                &spans("界+界"),
                MatchingKind::Strings,
                &[("αβ", "界")],
            )
            .unwrap(),
            [
                (vec![0, 1], vec![0]),
                (vec![3, 4], vec![2]),
                (vec![2], vec![1]),
            ]
        );
    }

    #[test]
    fn native_commands_are_indivisible_but_full_commands_can_be_mapped() {
        let source = SpanMapData {
            source: "\\alpha".into(),
            entries: vec![SpanMapEntry {
                start: 0,
                end: 6,
                kind: SpanKindU8::MathGlyph,
            }],
        };
        assert!(
            plan(&source, &spans("x"), MatchingKind::Tex, &[("alpha", "x")])
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            plan(&source, &spans("x"), MatchingKind::Tex, &[("\\alpha", "x")]).unwrap(),
            [(vec![0], vec![0])]
        );
    }

    #[test]
    fn native_macro_ink_and_painter_order_stay_together() {
        let source = SpanMapData {
            source: "ab".into(),
            entries: vec![
                SpanMapEntry {
                    start: 1,
                    end: 2,
                    kind: SpanKindU8::MathGlyph,
                },
                SpanMapEntry {
                    start: 0,
                    end: 1,
                    kind: SpanKindU8::MathGlyph,
                },
                SpanMapEntry {
                    start: 0,
                    end: 2,
                    kind: SpanKindU8::MathRule,
                },
            ],
        };
        assert_eq!(
            plan(&source, &spans("z"), MatchingKind::Tex, &[("ab", "z")]).unwrap(),
            [(vec![0, 1, 2], vec![0])]
        );
    }

    #[test]
    fn nonexistent_mapping_leaves_automatic_matching_unchanged() {
        let source = spans("aba");
        let target = spans("bab");
        for kind in [MatchingKind::Tex, MatchingKind::Strings] {
            assert_eq!(
                plan(&source, &target, kind, &[("missing", "absent")]).unwrap(),
                plan(&source, &target, kind, &[]).unwrap()
            );
        }
    }

    #[test]
    fn invalid_maps_refuse_before_planning() {
        for mappings in [
            vec![("", "x")],
            vec![("x", "")],
            vec![("x", "y"), ("x", "z")],
        ] {
            assert!(matches!(
                plan(&spans("x"), &spans("y"), MatchingKind::Tex, &mappings),
                Err(TextMatchingError::InvalidKeyMap { .. })
            ));
        }
    }

    #[test]
    fn mapping_count_and_work_have_named_ceilings() {
        let mappings = vec![("x", "y"); MAX_MATCHING_KEY_MAP_ENTRIES + 1];
        assert!(matches!(
            plan(&spans("x"), &spans("y"), MatchingKind::Tex, &mappings),
            Err(TextMatchingError::BudgetExceeded {
                resource: "key-map entries",
                ..
            })
        ));
        let mut work = MAX_MATCHING_MAPPING_WORK;
        assert!(matches!(
            charge_mapping_work(&mut work, usize::MAX),
            Err(TextMatchingError::BudgetExceeded {
                resource: "key-map search work",
                ..
            })
        ));
    }

    fn native_fixture(stage: &mut Stage, key: &str, x: f64) -> (Mob, Mob, SpanMapData) {
        let root = stage.add(Mobject::new());
        let child = stage.add(Mobject::from_points(&[
            [x, 0.0, 0.0],
            [x + 0.5, 1.0, 0.0],
            [x + 1.0, 0.0, 0.0],
        ]));
        stage.attach(root, child).unwrap();
        (root, child, spans(key))
    }

    #[test]
    fn callback_failures_block_replacement_until_an_explicit_new_run() {
        for strings in [false, true] {
            for phase in 0..3 {
                let mut stage = Stage::new();
                let (source, source_part, sm) = native_fixture(&mut stage, "x", 0.0);
                let (target, target_part, tm) = native_fixture(&mut stage, "y", 10.0);
                stage.add_to_scene(source).unwrap();
                let target_ink = stage.get_points(target_part);
                let trigger = stage.add(Mobject::new());
                stage
                    .add_try_updater(
                        trigger,
                        |_, _| {
                            Err(fmn_mobject::UpdaterError::callback(
                                "matching callback",
                                "preserve this first diagnostic",
                            ))
                        },
                        false,
                    )
                    .unwrap();
                let mut animation: Box<dyn Animation> = if strings {
                    Box::new(
                        TransformMatchingStrings::with_key_map(
                            &mut stage,
                            source,
                            target,
                            &sm,
                            &tm,
                            &[("x", "y")],
                            TextMatchingConfig::default(),
                        )
                        .unwrap(),
                    )
                } else {
                    Box::new(
                        TransformMatchingTex::with_key_map(
                            &mut stage,
                            source,
                            target,
                            &sm,
                            &tm,
                            &[("x", "y")],
                            TextMatchingConfig::default(),
                        )
                        .unwrap(),
                    )
                };
                animation.begin(&mut stage).unwrap();
                if phase == 1 {
                    animation.finish(&mut stage);
                }
                stage.update_mobject(trigger, 0.1);
                let expected = AnimError::from(stage.updater_error().unwrap().clone());
                match phase {
                    0 => animation.finish(&mut stage),
                    1 => animation.clean_up_from_scene(&mut stage),
                    _ => animation.update_mobjects(&mut stage, 0.1),
                }
                assert_eq!(animation.deferred_error(), Some(expected.clone()));
                assert_eq!(stage.roots(), &[source]);
                assert_eq!(stage.get_points(target_part), target_ink);
                assert!(animation.freeze_interpolation().is_none());

                // Acknowledging the Stage fault is not permission to publish
                // the target of this failed animation or replay its endpoint.
                stage.take_updater_error().unwrap().unwrap();
                let points_before_retry = stage.get_points(source_part);
                animation.interpolate(&mut stage, 0.75);
                animation.update_mobjects(&mut stage, 0.1);
                animation.finish(&mut stage);
                animation.clean_up_from_scene(&mut stage);
                animation.abort(&mut stage);
                animation.clean_up_from_scene(&mut stage);
                assert_eq!(animation.deferred_error(), Some(expected));
                assert_eq!(stage.roots(), &[source]);
                assert_eq!(stage.get_points(source_part), points_before_retry);
                assert!(!stage.is_animating(source_part));
                assert!(!stage.is_updating_suspended(source_part));

                stage.clear_updaters(trigger, false);
                animation.begin(&mut stage).unwrap();
                assert!(animation.deferred_error().is_none());
                animation.finish(&mut stage);
                animation.clean_up_from_scene(&mut stage);
                assert_eq!(stage.roots(), &[target]);
                assert_eq!(stage.get_points(target_part), target_ink);
                assert!(animation.deferred_error().is_none());
            }
        }
    }
}
