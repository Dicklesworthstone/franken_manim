# Scene input and native-backed controls

The wheel installs `fmn_python.interaction` and `fmn_python.control_events`
on the existing public classes. Direct extension embeddings may install those
same adapters after the native module has initialized. Geometry, hit testing,
scalar values, text construction and camera projection remain native-backed.
No window, network endpoint, browser event transport or renderer is added.

```python
from manimlib import Scene, Square, Button, RIGHT

scene = Scene()
shape = Square().shift(2 * RIGHT)
button = Button(shape, lambda mob: mob.shift(RIGHT))
scene.add(button)
scene.on_mouse_press(shape.get_center(), 1, 0)
```

Button and MotionMobject register handlers on their original wrapped objects.
Checkbox and EnableDisableButton register their toggle handlers. A
LinearNumberSlider registers its handle for captured dragging; Textbox binds
box activation and key input; ControlPanel binds opener dragging and panel
scrolling. Registrations use the public misspelled `*_listner` APIs retained
from the Reference, including authored handler/registrar overrides. A failed
later registration attempts to remove only the registrations it just added.

## Scene input semantics

Scene mouse callbacks consume world-space coordinates. Each event's point is
authoritative, so pressing or scrolling does not require a preceding hover.
Fixed-frame listeners receive coordinates projected by the existing
CameraFrame methods; world-space listeners retain world-space coordinates.
Pointer, key and drag-capture state are kept separately for each scene.
Ordinary registered objects must remain in that scene's drawable family;
arena allocation alone does not make a removed control interactive.

Listeners execute once, before the Scene/InteractiveScene default action.
An exact `False` return stops propagation and the default camera/selection
action. Existing user overrides still run normally; code an override executes
before calling `super()` is not undone. Listener order is snapshotted at input
entry, removals take effect before subsequent delivery, and additions begin
with the next event. Drag capture survives movement outside the object bounds
and ends on release or an input callback failure. Nested input restores the
outer scene context even when a callback raises.

**Compatibility boundary:** direct EventDispatcher calls keep their existing
standalone hover/capture and pointer-identity behavior. An explicit detached
EventListener added only to the global dispatcher remains a global interceptor,
including in Scene input. This is distinct from Mobject-local registrations
used by the installed controls. Bound listener targets are scene-scoped.

## Live control changes and limits

Checkbox marks are placed against the current box bounds. Enable/disable and
Textbox activation change live style rather than replacing positioned boxes.
New text is reseated at the live box. Active textboxes accept the existing
alphanumeric/space/tab/backspace key surface; navigation and modifier keysyms
are not inserted as Unicode letters, and unsupported shortcuts do not execute
scene actions. This is not clipboard, text-selection or IME support.

The subsequently installed color-bank adapter supplies independently
interactive ColorSliders channels and grouped panel controls; see
[Live color controls and composite panels](COLOR_CONTROL_PANELS.md).
These adapters do not change the existing copying/closure semantics, make
every camera or control mutation transactional, or complete the Python/native
Studio worker gateway. The existing Studio capability refusals remain in place.

`test_interaction_protocol.py` and `test_control_events_protocol.py` test
adapter logic against explicit storage fixtures. The installed-wheel gate
also runs `control_interaction.py` with actual native objects and a decoded
input-driven Y4M render, including a one/four-thread byte comparison. Protocol
passes and syntax checks alone do not establish that native acceptance passed.
