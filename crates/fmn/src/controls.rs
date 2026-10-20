//! Native scene bindings for the interactive library controls.
//!
//! The detached constructors remain ordinary library values. These helpers
//! add their visible families to a [`Scene`], register its pointer listeners,
//! and keep scalar values synchronized through an ordinary arena updater.
//! Input uses scene-space coordinates and the Scene's recorded event stream;
//! slider drags retain the dispatcher's pointer capture until release.
//!
//! A scalar control owns a separate, point-free tracker inside its family.
//! Animate that tracker to change the value while the visible control keeps
//! updating. Moving, rotating, or scaling the container moves the live hit
//! geometry and slider axis together. Rebuilt marks/readouts preserve their
//! affine placement; arbitrary pointwise edits of a rebuilt part are replaced
//! by the next value change. Unchanged controls do not rebuild geometry.
//!
//! Arena copies remap the internal handles and copy mutable control state.
//! Scene listeners are separate: use the binding's `bind_copy` method to make
//! a copied family interactive. In-memory snapshots retain control state;
//! durable replay reconstructs the bindings and feeds their recorded events
//! through [`Scene::queue_replay_events`]. Call `unbind` to remove listeners
//! and synchronization without destroying the displayed family.

use fmn_core::types::Vec3;
use fmn_library::controls::{
    Button, Checkbox, EnableDisableButton, LinearNumberSlider, SliderError,
};
use fmn_library::{Style, VMobject, VStyle, v_group};
use fmn_mobject::{
    CopyMap, Mob, Placement, Stage as Arena, StageError, Uniforms, UpdaterError, UpdaterId,
};
use fmn_scene::{
    EventListener, EventListenerId, EventPayload, EventPropagation, EventTarget, EventType,
    MouseButton, Scene, SceneError,
};
use fmn_text::FontBook;

use crate::Result;

/// A scalar control's stable scene, visual, and tracker handles.
#[derive(Debug)]
pub struct ScalarControlBinding {
    connection: Connection,
    tracker: Mob,
    visual: Mob,
    components: Vec<Mob>,
}

impl ScalarControlBinding {
    /// The whole family. Position or animate this handle to move the widget.
    #[must_use]
    pub fn container(&self) -> Mob {
        self.connection.container
    }

    /// The independent value tracker. Its animations update the visible widget
    /// during playback rather than suspending the widget's updater.
    #[must_use]
    pub fn tracker(&self) -> Mob {
        self.tracker
    }

    /// The visible group, excluding the point-free value tracker.
    #[must_use]
    pub fn visual(&self) -> Mob {
        self.visual
    }

    /// Stable direct visual children, in the detached constructor's order:
    /// checkbox `[box, mark]`, enable button `[box]`, slider
    /// `[bar, handle, axis, optional readout]`.
    #[must_use]
    pub fn components(&self) -> &[Mob] {
        &self.components
    }

    /// Read the requested scalar value. Booleans use zero and one.
    pub fn value(&self, stage: &Arena) -> Result<f64> {
        stage
            .tracker_value(self.tracker)
            .ok_or(StageError::StaleHandle.into())
    }

    /// Apply a value immediately, including while ordinary updating is
    /// suspended. Slider values clamp into range; drag input also snaps to the
    /// configured step. Validation and text layout precede publication.
    ///
    /// Direct tracker edits instead take effect on the next updater pass.
    /// A pending native failure refuses this operation until acknowledged.
    pub fn set_value(&self, stage: &mut Arena, value: f64) -> Result<()> {
        check_pending(stage)?;
        self.connection.check_registered(stage)?;
        sync_scalar(
            stage,
            self.container(),
            self.connection.updater,
            Some(ScalarInput::Set(value)),
        )?;
        Ok(())
    }

    /// Register listeners on an arena family copy and add it to this Scene.
    /// Mutable values and geometry remain independent; updater callables keep
    /// the arena's ordinary shared-callable copy semantics.
    pub fn bind_copy(&self, scene: &mut Scene, copied: &CopyMap) -> Result<Self> {
        check_pending(scene.stage())?;
        let container = copied
            .get(self.container())
            .ok_or(StageError::StaleHandle)?;
        let state = scalar_state(scene.stage(), container, self.connection.updater)?;
        validate_scalar_family(scene.stage(), container, &state)?;
        self.connection
            .check_registered_at(scene.stage(), container)?;
        let listeners = scalar_listeners(scene, container, self.connection.updater, &state)?;
        scene.add(&[container])?;
        Ok(Self {
            connection: Connection {
                container,
                updater: self.connection.updater,
                listeners,
            },
            tracker: state.tracker,
            visual: state.visual,
            components: state.components,
        })
    }

    /// Remove this binding's listeners and updater, retaining its geometry and
    /// tracker. Idempotent; a different Scene's coincident listener ids are
    /// untouched. This also removes listeners after the container was deleted.
    pub fn unbind(&mut self, scene: &mut Scene) -> bool {
        self.connection.unbind(scene)
    }
}

/// A scene button's stable container and callback content.
#[derive(Debug)]
pub struct ButtonBinding {
    connection: Connection,
    content: Mob,
}

impl ButtonBinding {
    /// The complete button, including its clickable background rectangle.
    #[must_use]
    pub fn container(&self) -> Mob {
        self.connection.container
    }

    /// The content handle passed to the click callback.
    #[must_use]
    pub fn content(&self) -> Mob {
        self.content
    }

    /// Attach the original callback to a copied button's remapped content.
    pub fn bind_copy(&self, scene: &mut Scene, copied: &CopyMap) -> Result<Self> {
        check_pending(scene.stage())?;
        let container = copied
            .get(self.container())
            .ok_or(StageError::StaleHandle)?;
        self.connection
            .check_registered_at(scene.stage(), container)?;
        let state = button_state(scene.stage(), container, self.connection.updater)?;
        validate_button_family(scene.stage(), container, &state)?;
        let listeners =
            button_listeners(scene, container, self.connection.updater, state.background)?;
        scene.add(&[container])?;
        Ok(Self {
            connection: Connection {
                container,
                updater: self.connection.updater,
                listeners,
            },
            content: state.content,
        })
    }

    /// Remove this button's listeners and updater, leaving the displayed family.
    pub fn unbind(&mut self, scene: &mut Scene) -> bool {
        self.connection.unbind(scene)
    }
}

/// Add a clickable checkbox and its scalar tracker to a Scene.
pub fn add_checkbox(scene: &mut Scene, checkbox: Checkbox) -> Result<ScalarControlBinding> {
    add_scalar(scene, ScalarModel::Checkbox(Box::new(checkbox)))
}

/// Add the Reference's colored enable/disable toggle to a Scene.
/// Its constructor's white fill remains until the first value mutation.
pub fn add_enable_disable_button(
    scene: &mut Scene,
    button: EnableDisableButton,
) -> Result<ScalarControlBinding> {
    add_scalar(scene, ScalarModel::Enable(Box::new(button)))
}

/// Add a slider with captured primary-button dragging and tracker animation.
/// The detached constructor's initial handle position is retained until its
/// first value mutation; `binding.set_value` can explicitly synchronize it.
pub fn add_slider(scene: &mut Scene, slider: LinearNumberSlider) -> Result<ScalarControlBinding> {
    add_scalar(scene, ScalarModel::Slider(Box::new(slider)))
}

/// Add a button with an infallible callback on each primary-button press.
/// The callback receives the live arena and this button's content handle.
pub fn add_button(
    scene: &mut Scene,
    button: Button,
    book: &FontBook,
    mut on_click: impl FnMut(&mut Arena, Mob) + 'static,
) -> Result<ButtonBinding> {
    add_try_button(scene, button, book, move |stage, content| {
        on_click(stage, content);
        Ok(())
    })
}

/// Add a button whose callback can return a native updater failure.
/// A refused click stops later listeners and frame publication through the
/// Scene's retained failure boundary. Each press is consumed before calling
/// user code, so acknowledging a failure does not repeat that same click.
pub fn add_try_button(
    scene: &mut Scene,
    button: Button,
    book: &FontBook,
    mut on_click: impl FnMut(&mut Arena, Mob) -> std::result::Result<(), UpdaterError> + 'static,
) -> Result<ButtonBinding> {
    check_pending(scene.stage())?;
    let composition = button.build(book)?;
    validate_detached(&composition, Placement::IDENTITY)?;
    let container = scene.stage_mut().add(composition);
    let parts = scene.stage().try_get(container)?.submobjects();
    let state = ButtonState {
        background: parts[0],
        content: parts[1],
        pending: false,
    };
    let content = state.content;
    let background = state.background;
    let registration = scene.stage_mut().add_try_dt_updater_with_state(
        container,
        state,
        remap_button,
        move |stage, receiver, updater, _| {
            let state = button_state(stage, receiver, updater)?;
            if !state.pending {
                return Ok(());
            }
            validate_button_family(stage, receiver, &state)?;
            stage
                .updater_state_mut::<ButtonState>(receiver, updater)
                .ok_or(StageError::UpdaterBindingMismatch)?
                .pending = false;
            on_click(stage, state.content)
        },
        false,
    );
    let updater = match registration {
        Ok(updater) => updater,
        Err(error) => {
            discard_new_family(scene.stage_mut(), container);
            return Err(error.into());
        }
    };
    let listeners = match button_listeners(scene, container, updater, background) {
        Ok(listeners) => listeners,
        Err(error) => {
            discard_new_family(scene.stage_mut(), container);
            return Err(error);
        }
    };
    scene.add(&[container])?;
    Ok(ButtonBinding {
        connection: Connection {
            container,
            updater,
            listeners,
        },
        content,
    })
}

#[derive(Debug)]
struct Connection {
    container: Mob,
    updater: UpdaterId,
    listeners: Vec<(EventListenerId, EventTarget)>,
}

impl Connection {
    fn check_registered(&self, stage: &Arena) -> std::result::Result<(), UpdaterError> {
        self.check_registered_at(stage, self.container)
    }

    fn check_registered_at(
        &self,
        stage: &Arena,
        container: Mob,
    ) -> std::result::Result<(), UpdaterError> {
        stage.try_get(container)?;
        if !stage.updater_ids(container).contains(&self.updater) {
            return Err(StageError::UpdaterBindingMismatch.into());
        }
        Ok(())
    }

    fn unbind(&mut self, scene: &mut Scene) -> bool {
        let mut removed = false;
        self.listeners.retain(|&(id, target)| {
            let matched = scene
                .event_dispatcher_mut()
                .remove_listener_for_target(id, target);
            removed |= matched;
            !matched
        });
        if scene
            .stage()
            .updater_ids(self.container)
            .contains(&self.updater)
        {
            scene
                .stage_mut()
                .remove_updater(self.container, self.updater);
            removed = true;
        }
        removed
    }
}

#[derive(Clone)]
enum ScalarModel {
    Checkbox(Box<Checkbox>),
    Enable(Box<EnableDisableButton>),
    Slider(Box<LinearNumberSlider>),
}

impl ScalarModel {
    fn value(&self) -> f64 {
        match self {
            Self::Checkbox(model) => f64::from(u8::from(model.value())),
            Self::Enable(model) => f64::from(u8::from(model.value())),
            Self::Slider(model) => model.value(),
        }
    }

    fn composition(&self) -> VMobject {
        match self {
            Self::Checkbox(model) => model.composition(),
            Self::Enable(model) => model.composition(),
            Self::Slider(model) => model.composition(),
        }
    }

    fn set_value(&mut self, value: f64) -> std::result::Result<(), UpdaterError> {
        if !value.is_finite() {
            return Err(control_error(SliderError::NonFiniteValue));
        }
        match self {
            Self::Checkbox(model) => model.set_value(value != 0.0),
            Self::Enable(model) => model.set_value(value != 0.0),
            Self::Slider(model) => model.set_value(value).map_err(control_error)?,
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum ScalarInput {
    Toggle,
    Drag(Vec3),
    Set(f64),
}

#[derive(Clone)]
struct ScalarState {
    model: ScalarModel,
    tracker: Mob,
    visual: Mob,
    components: Vec<Mob>,
    pending: Option<ScalarInput>,
}

fn remap_scalar(state: &mut ScalarState, copied: &CopyMap) {
    state.tracker = copied.get(state.tracker).unwrap_or(state.tracker);
    state.visual = copied.get(state.visual).unwrap_or(state.visual);
    for part in &mut state.components {
        *part = copied.get(*part).unwrap_or(*part);
    }
}

fn scalar_state(
    stage: &Arena,
    container: Mob,
    updater: UpdaterId,
) -> std::result::Result<ScalarState, UpdaterError> {
    stage
        .updater_state::<ScalarState>(container, updater)
        .cloned()
        .ok_or(StageError::UpdaterBindingMismatch.into())
}

fn validate_scalar_family(
    stage: &Arena,
    container: Mob,
    state: &ScalarState,
) -> std::result::Result<(), UpdaterError> {
    let children = stage.try_get(container)?.submobjects();
    if !children.contains(&state.visual)
        || !children.contains(&state.tracker)
        || stage.try_get(state.visual)?.submobjects() != state.components.as_slice()
    {
        return Err(StageError::FamilyShapeMismatch.into());
    }
    stage.try_get(state.tracker)?;
    for &part in &state.components {
        stage.try_get(part)?;
    }
    Ok(())
}

fn add_scalar(scene: &mut Scene, model: ScalarModel) -> Result<ScalarControlBinding> {
    check_pending(scene.stage())?;
    let value = model.value();
    let composition = model.composition();
    validate_detached(&composition, Placement::IDENTITY)?;
    let container = scene
        .stage_mut()
        .add(v_group([composition, VMobject::new()]));
    let children = scene.stage().try_get(container)?.submobjects();
    let (visual, tracker) = (children[0], children[1]);
    scene.stage_mut().set_tracker_state(
        tracker,
        Some(fmn_mobject::Tracker {
            kind: fmn_mobject::TrackerKind::Plain,
            lanes: [value, 0.0],
        }),
    )?;
    let components = scene.stage().try_get(visual)?.submobjects().to_vec();
    let state = ScalarState {
        model,
        tracker,
        visual,
        components: components.clone(),
        pending: None,
    };
    let registration = scene.stage_mut().add_try_dt_updater_with_state(
        container,
        state.clone(),
        remap_scalar,
        |stage, receiver, updater, _| sync_scalar(stage, receiver, updater, None),
        false,
    );
    let updater = match registration {
        Ok(updater) => updater,
        Err(error) => {
            discard_new_family(scene.stage_mut(), container);
            return Err(error.into());
        }
    };
    let listeners = match scalar_listeners(scene, container, updater, &state) {
        Ok(listeners) => listeners,
        Err(error) => {
            discard_new_family(scene.stage_mut(), container);
            return Err(error);
        }
    };
    scene.add(&[container])?;
    Ok(ScalarControlBinding {
        connection: Connection {
            container,
            updater,
            listeners,
        },
        tracker,
        visual,
        components,
    })
}

fn scalar_listeners(
    scene: &mut Scene,
    container: Mob,
    updater: UpdaterId,
    state: &ScalarState,
) -> Result<Vec<(EventListenerId, EventTarget)>> {
    let (event_type, target) = match state.model {
        ScalarModel::Slider(_) => (
            EventType::MouseDrag,
            EventTarget::Mobject(state.components[1]),
        ),
        _ => (
            EventType::MousePress,
            EventTarget::Mobject(state.components[0]),
        ),
    };
    let id = scene
        .event_dispatcher_mut()
        .add_listener(EventListener::new(
            event_type,
            target,
            move |event, _, _, stage| {
                if !accepts_input(stage, container, updater) {
                    return EventPropagation::Continue;
                }
                let input = match event.payload {
                    EventPayload::MousePress {
                        button: MouseButton::Left,
                        ..
                    } => ScalarInput::Toggle,
                    EventPayload::MouseDrag {
                        button: MouseButton::Left,
                        point,
                        ..
                    } => ScalarInput::Drag(point),
                    _ => return EventPropagation::Continue,
                };
                let Some(state) = stage.updater_state_mut::<ScalarState>(container, updater) else {
                    return EventPropagation::Continue;
                };
                state.pending = Some(input);
                // A self-only pass avoids invoking unrelated child updaters just to
                // deliver input. The registered fallible callback owns the diagnostic.
                stage.update_mobject_with_recurse(container, 0.0, false);
                EventPropagation::Stop
            },
        ))
        .map_err(SceneError::from)?;
    Ok(vec![(id, target)])
}

fn sync_scalar(
    stage: &mut Arena,
    container: Mob,
    updater: UpdaterId,
    input: Option<ScalarInput>,
) -> std::result::Result<(), UpdaterError> {
    let current = stage
        .updater_state::<ScalarState>(container, updater)
        .ok_or(StageError::UpdaterBindingMismatch)?;
    let input = input.or(current.pending);
    let tracker_value = stage
        .tracker_value(current.tracker)
        .ok_or(StageError::StaleHandle)?;
    if input.is_none() && tracker_value.to_bits() == current.model.value().to_bits() {
        return Ok(());
    }
    let mut next = current.clone();
    validate_scalar_family(stage, container, &next)?;
    let requested = match input {
        Some(ScalarInput::Set(value)) => value,
        Some(ScalarInput::Toggle) => {
            if tracker_value == 0.0 {
                1.0
            } else {
                0.0
            }
        }
        Some(ScalarInput::Drag(point)) => {
            let ScalarModel::Slider(model) = &next.model else {
                return Err(StageError::FamilyShapeMismatch.into());
            };
            let (start, end) = staged_axis(stage, next.components[2])?;
            let fraction = axis_fraction(start, end, point)?;
            let original = model.axis().points();
            let start = *original
                .first()
                .ok_or_else(|| control_error(SliderError::InvalidAxis))?;
            let end = *original
                .last()
                .ok_or_else(|| control_error(SliderError::InvalidAxis))?;
            model
                .value_from_point(interpolate(start, end, fraction))
                .map_err(control_error)?
        }
        None => tracker_value,
    };
    next.model.set_value(requested)?;

    // Every fallible build and placement check precedes changes to the live
    // tracker, its model, or any visible part.
    match &next.model {
        ScalarModel::Checkbox(model) => {
            PreparedReplacement::new(stage, next.components[1], model.content().clone(), false)?
                .install(stage)?;
        }
        ScalarModel::Enable(model) => {
            let style = model.rect().style();
            stage.set_fill(
                next.components[0],
                Some(style.fill_color),
                Some(style.fill_opacity),
                None,
                true,
            );
        }
        ScalarModel::Slider(model) => {
            let (start, end) = staged_axis(stage, next.components[2])?;
            let span = model.max() - model.min();
            let fraction = if span == 0.0 {
                0.0
            } else {
                (model.value() - model.min()) / span
            };
            let target = interpolate(start, end, fraction);
            let center = stage.get_center(next.components[1]);
            let shift = subtract(target, center);
            if !target.into_iter().chain(shift).all(f64::is_finite) {
                return Err(control_error(SliderError::InvalidAxis));
            }
            let replacement = if next.components.len() == 4 {
                Some(PreparedReplacement::new(
                    stage,
                    next.components[3],
                    model.composition().children()[3].clone(),
                    true,
                )?)
            } else {
                None
            };
            if let Some(replacement) = replacement {
                replacement.install(stage)?;
            }
            stage.shift(next.components[1], shift);
        }
    }
    stage.set_tracker_value(next.tracker, next.model.value())?;
    next.pending = None;
    *stage
        .updater_state_mut::<ScalarState>(container, updater)
        .ok_or(StageError::UpdaterBindingMismatch)? = next;
    Ok(())
}

struct PreparedReplacement {
    target: Mob,
    value: VMobject,
    placement: Placement,
    uniforms: Uniforms,
    z_index: i32,
    paint: Option<Style>,
}

impl PreparedReplacement {
    fn new(
        stage: &Arena,
        target: Mob,
        value: VMobject,
        preserve_paint: bool,
    ) -> std::result::Result<Self, UpdaterError> {
        let entry = stage.try_get(target)?;
        let placement = entry.placement();
        validate_detached(&value, placement)?;
        let paint = if preserve_paint {
            stage
                .family(target)
                .into_iter()
                .find(|&member| {
                    stage
                        .get(member)
                        .is_some_and(|entry| !entry.buffer.is_empty())
                })
                .map(|donor| Style {
                    fill_color: stage
                        .get_fill_color(donor)
                        .unwrap_or(Style::default().fill_color),
                    fill_opacity: stage.get_fill_opacity(donor).unwrap_or(0.0),
                    stroke_color: stage
                        .get_stroke_color(donor)
                        .unwrap_or(Style::default().stroke_color),
                    stroke_opacity: stage.get_stroke_opacity(donor).unwrap_or(0.0),
                    stroke_width: stage.get_stroke_width(donor).unwrap_or(0.0),
                    ..Style::default()
                })
        } else {
            None
        };
        Ok(Self {
            target,
            value,
            placement,
            uniforms: *entry.uniforms(),
            z_index: stage.z_index(target),
            paint,
        })
    }

    fn install(self, stage: &mut Arena) -> std::result::Result<(), UpdaterError> {
        stage.install(self.target, self.value)?;
        for member in stage.family(self.target) {
            stage.set_placement(member, self.placement)?;
            let entry = stage.get_mut(member).ok_or(StageError::StaleHandle)?;
            *entry.uniforms_mut() = self.uniforms;
            if let Some(paint) = self.paint {
                paint.write(&mut entry.buffer);
            }
        }
        stage.set_z_index(self.target, self.z_index, false);
        Ok(())
    }
}

#[derive(Clone)]
struct ButtonState {
    background: Mob,
    content: Mob,
    pending: bool,
}

fn remap_button(state: &mut ButtonState, copied: &CopyMap) {
    state.background = copied.get(state.background).unwrap_or(state.background);
    state.content = copied.get(state.content).unwrap_or(state.content);
}

fn button_state(
    stage: &Arena,
    container: Mob,
    updater: UpdaterId,
) -> std::result::Result<ButtonState, UpdaterError> {
    stage
        .updater_state::<ButtonState>(container, updater)
        .cloned()
        .ok_or(StageError::UpdaterBindingMismatch.into())
}

fn validate_button_family(
    stage: &Arena,
    container: Mob,
    state: &ButtonState,
) -> std::result::Result<(), UpdaterError> {
    let children = stage.try_get(container)?.submobjects();
    if !children.contains(&state.background) || !children.contains(&state.content) {
        return Err(StageError::FamilyShapeMismatch.into());
    }
    stage.try_get(state.background)?;
    stage.try_get(state.content)?;
    Ok(())
}

fn button_listeners(
    scene: &mut Scene,
    container: Mob,
    updater: UpdaterId,
    background: Mob,
) -> Result<Vec<(EventListenerId, EventTarget)>> {
    let target = EventTarget::Mobject(background);
    let id = scene
        .event_dispatcher_mut()
        .add_listener(EventListener::new(
            EventType::MousePress,
            target,
            move |event, _, _, stage| {
                if !matches!(
                    event.payload,
                    EventPayload::MousePress {
                        button: MouseButton::Left,
                        ..
                    }
                ) || !accepts_input(stage, container, updater)
                {
                    return EventPropagation::Continue;
                }
                let Some(state) = stage.updater_state_mut::<ButtonState>(container, updater) else {
                    return EventPropagation::Continue;
                };
                state.pending = true;
                stage.update_mobject_with_recurse(container, 0.0, false);
                EventPropagation::Stop
            },
        ))
        .map_err(SceneError::from)?;
    Ok(vec![(id, target)])
}

fn accepts_input(stage: &Arena, container: Mob, updater: UpdaterId) -> bool {
    stage.updater_ids(container).contains(&updater)
        && !stage.is_updating_suspended(container)
        && stage
            .roots()
            .iter()
            .any(|&root| stage.family(root).contains(&container))
}

fn check_pending(stage: &Arena) -> Result<()> {
    if let Some(failure) = stage.updater_error() {
        return Err(failure.clone().into());
    }
    Ok(())
}

fn control_error(error: impl std::fmt::Display) -> UpdaterError {
    UpdaterError::callback("native control update", error)
}

fn staged_axis(stage: &Arena, axis: Mob) -> std::result::Result<(Vec3, Vec3), UpdaterError> {
    let points = stage.get_points(axis).ok_or(StageError::StaleHandle)?;
    let start = *points
        .first()
        .ok_or_else(|| control_error(SliderError::InvalidAxis))?;
    let end = *points
        .last()
        .ok_or_else(|| control_error(SliderError::InvalidAxis))?;
    if !start.into_iter().chain(end).all(f64::is_finite) {
        return Err(control_error(SliderError::InvalidAxis));
    }
    Ok((start, end))
}

fn axis_fraction(start: Vec3, end: Vec3, point: Vec3) -> std::result::Result<f64, UpdaterError> {
    let direction = subtract(end, start);
    let offset = subtract(point, start);
    let length = direction
        .into_iter()
        .map(|value| value * value)
        .sum::<f64>();
    let dot = direction
        .into_iter()
        .zip(offset)
        .map(|(a, b)| a * b)
        .sum::<f64>();
    if !length.is_finite() || !dot.is_finite() {
        return Err(control_error(SliderError::InvalidAxis));
    }
    Ok(if length == 0.0 {
        0.0
    } else {
        (dot / length).clamp(0.0, 1.0)
    })
}

fn subtract(a: Vec3, b: Vec3) -> Vec3 {
    std::array::from_fn(|axis| a[axis] - b[axis])
}
fn interpolate(a: Vec3, b: Vec3, fraction: f64) -> Vec3 {
    std::array::from_fn(|axis| a[axis] + (b[axis] - a[axis]) * fraction)
}

fn validate_detached(
    value: &VMobject,
    placement: Placement,
) -> std::result::Result<(), UpdaterError> {
    if !placement.coefficients().into_iter().all(f64::is_finite)
        || !value.points().iter().all(|&point| {
            // Geometry enters f32 records; retained affine placement is f64.
            point
                .into_iter()
                .all(|component| (component as f32).is_finite())
                && placement
                    .apply_point(point.map(|component| f64::from(component as f32)))
                    .into_iter()
                    .all(f64::is_finite)
        })
    {
        return Err(control_error("control geometry or placement is not finite"));
    }
    for child in value.children() {
        validate_detached(child, placement)?;
    }
    Ok(())
}

// Only used before a newly built family has been published or passed to user
// code: every member is private and unpinned, with no external parents.
fn discard_new_family(stage: &mut Arena, root: Mob) {
    for member in stage.family(root).into_iter().rev() {
        let _ = stage.delete(member);
    }
}
