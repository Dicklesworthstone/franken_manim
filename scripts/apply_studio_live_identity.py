"""Bind the frozen input validator to native live event/clock publication."""
from pathlib import Path


def edit(name, old, new):
    path = Path(name)
    text = path.read_text()
    if new in text:
        return
    if text.count(old) != 1:
        raise RuntimeError(f"refusing drifted live integration: {name}: {old[:90]!r}")
    path.write_text(text.replace(old, new, 1))


live = "crates/fmn-python/src/portal_studio/live.rs"
edit(live, '''/// Snapshot after synchronization, never while a Python callback has an engine''', '''/// The worker freezes this callable at startup, outside authored scene fields.
/// It must run unborrowed, before callbacks and before publishing their pixels.
fn validate_inputs(py: Python<'_>, validator: Option<&Py<PyAny>>) -> PyResult<()> {
    if let Some(validator) = validator {
        if !validator.call0(py)?.bind(py).is_none() {
            return Err(PyTypeError::new_err("Studio input validator must return None"));
        }
    }
    Ok(())
}

/// Snapshot after synchronization, never while a Python callback has an engine''')
edit(live, '''fn refresh(scene: &Bound<'_, PyScene>) -> PyResult<()> {
    let engine''', '''fn refresh(scene: &Bound<'_, PyScene>, validator: Option<&Py<PyAny>>) -> PyResult<()> {
    validate_inputs(scene.py(), validator)?;
    let engine''')
edit(live, '''    with_capture(scene, |capture| {
        capture.live_refresh = true;''', '''    // Includes lazy imports and input edits made by zero-dt updaters and
    // camera descriptors. No frame has replaced the healthy recording yet.
    validate_inputs(scene.py(), validator)?;
    with_capture(scene, |capture| {
        capture.live_refresh = true;''')
edit(live, '''fn advance(scene: &Bound<'_, PyScene>, frames: u32) -> PyResult<()> {
    let engine''', '''fn advance(
    scene: &Bound<'_, PyScene>, frames: u32, validator: Option<&Py<PyAny>>,
) -> PyResult<()> {
    validate_inputs(scene.py(), validator)?;
    let engine''')
edit(live, '''        // Complete the normal updater pass, capturing only the final step.''', '''        // Scans are command-boundary work, not another per-frame watcher.
        // Earlier steps have no publication; validate after the last Python
        // callback before allowing the native final capture to replace it.
        if index + 1 == frames {
            validate_inputs(scene.py(), validator)?;
        }
        // Complete the normal updater pass, capturing only the final step.''')
edit(live, '''struct LiveWorker {
    scene: Py<PyScene>,''', '''struct LiveWorker {
    scene: Py<PyScene>,
    input_validator: Option<Py<PyAny>>,''')
edit(live, '''    fn new(scene: &Bound<'_, PyScene>) -> PyResult<Self> {
        let empty''', '''    fn new(scene: &Bound<'_, PyScene>) -> PyResult<Self> {
        let input_validator = scene
            .getattr("__dict__")?
            .cast::<PyDict>()?
            .get_item("_fmn_studio_validate_inputs")?
            .map(Bound::unbind);
        if input_validator.as_ref().is_some_and(|v| !v.bind(scene.py()).is_callable()) {
            return Err(PyTypeError::new_err("Studio input validator must be callable"));
        }
        let empty''')
# Both startup refreshes share the frozen callable. Do not rewrite the event
# arm until its pre-dispatch validation is installed below.
p = Path(live)
s = p.read_text()
old = '        refresh(scene)?;'
new = '        refresh(scene, input_validator.as_ref())?;'
if old in s:
    if s.count(old) != 2:
        raise RuntimeError("unexpected live startup refresh count")
    p.write_text(s.replace(old, new))
edit(live, '''            scene: scene.clone().unbind(),
            name,''', '''            scene: scene.clone().unbind(),
            input_validator,
            name,''')
edit(live, '''if let Err(error) = advance(scene, request.frames) {''', '''if let Err(error) = advance(scene, request.frames, self.input_validator.as_ref()) {''')
edit(live, '''let dispatched = dispatch(scene, input.event).and_then(|()| refresh(scene));''', '''let dispatched = validate_inputs(py, self.input_validator.as_ref())
                    .and_then(|()| dispatch(scene, input.event))
                    .and_then(|()| refresh(scene, self.input_validator.as_ref()));''')
edit(live, '''#[cfg(test)]
mod tests {''', '''#[cfg(test)]
#[path = "live_input_identity_tests.rs"]
mod input_identity_tests;

#[cfg(test)]
mod tests {''')
portal = "crates/fmn-python/python/fmn_python/studio.py"
edit(portal, '''                scene.__dict__["_fmn_studio_live_input"] = True
                try:''', '''                scene.__dict__["_fmn_studio_live_input"] = True
                scene.__dict__["_fmn_studio_validate_inputs"] = lambda: inputs.verify(loaded)
                try:''')
edit(portal, '''                    scene.__dict__.pop("_fmn_studio_live_input", None)
                return None''', '''                    scene.__dict__.pop("_fmn_studio_live_input", None)
                    scene.__dict__.pop("_fmn_studio_validate_inputs", None)
                return None''')
edit(portal, '''freeze input until reload; they are not rolled back or automatically retried.''', '''freeze input until reload; they are not rolled back or automatically retried.
Live commands also check the declared-input generation before callbacks and
before final capture: a helper/asset edit cannot silently mix two generations.''')
