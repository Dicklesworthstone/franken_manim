# Rebuild a scene without leaving its host editor

`SceneProject` owns a file-backed scene recipe: a local class name, constructor
arguments and the project's scoped Python imports. It constructs a fresh native
Scene, executes the ordinary `Scene.run` lifecycle and takes a native camera
snapshot. Only then does the candidate replace the working generation.

```python
from fmn_python import SceneProject

with SceneProject("my_scene.py", "Demo") as project:
    scene = project.scene
    image = project.preview
    # Edit the source or a helper on disk.
    replacement = project.rebuild()
    assert replacement is not scene
    # Check file contents first; unchanged sources keep the current scene.
    project.rebuild(if_changed=True)
```

A forced rebuild resets the scene from its recipe even when source files are
unchanged. The prior scene and image are not mutated; references retained by
the host still refer to the prior generation. Source-only `reload_source()` and
`auto_reload()` remain different operations: they update definitions without
reconstructing scene objects. A later full rebuild uses the updated definitions.

## The interactive workflow

With host IPython and a matching installed portal wheel:

```bash
fmn-python edit demo/python/rebuild_scene.py RebuildScene
```

At the IPython prompt, change `OFFSET` or `construct()` in the file and save it:

```python
reload()
square.get_center()
preview()
```

The editor stays open, but `self`, `scene`, `project.scene`, generated local
names and the default scene shortcuts refer to the new generation. Native
geometry, clocks, scene lifecycle and PNG capture use the same authorities as
ordinary rendering. No output file or encoder is opened by this workflow.

An authored `embed_scene(self, locals())` or `InteractiveScene.embed()` stops
construction at that point and contributes local bindings to the editor. It
uses the existing `EndScene` lifecycle; it is not a suspended coroutine and does
not resume the remainder of `construct()` when the editor closes. Without an
embed call, construction runs to completion before opening the editor.

A host may open the same editor programmatically:

```python
with SceneProject("my_scene.py", "Demo", scene_kwargs={"random_seed": 42}) as project:
    project.edit(clipboard=my_explicit_clipboard_reader)
```

IPython is optional for `SceneProject.rebuild`, and imported only when editing.
The CLI refuses noninteractive stdin and robot editing before executing source;
`fmn-python edit --help --robot` remains a structured help request. Headless
hosts should control `SceneProject` directly, rather than synthesize a terminal.

## Generation and failure rules

Successful rebuilding resets native state, clock and checkpoint history. A new
console/checkpoint manager owns the new scene. Old console objects are closed,
and stale reload callbacks cannot reopen an exited editor. A checkpoint-paste
cell cannot rebuild its own scene mid-execution; finish the cell and issue
`reload()` at the prompt instead. Active renders and play/wait segments also
refuse rebuilding before source execution.

Syntax, import, constructor, lifecycle, native preview and shortcut-preparation
failures leave the last working scene, preview, module graph, shell namespace
and checkpoint history active. Source rollback restores the import graph at the
start of that rebuild; a separately successful definition-only reload is not
undone. Correct the file and retry `reload()`. If release
of the old checkpoint manager fails *after* a new generation has committed, the
error explicitly reports that the new generation is active; it does not claim
that the successful build rolled back.

Shell and module dictionaries retain their identities. Functions defined in
cells therefore see refreshed source helpers in module globals. Interactive
bindings which no longer match a source-owned name are preserved. Explicit
aliases, closure captures and IPython output history may still retain old
objects; rebuilding does not rewrite arbitrary Python object graphs. `self`,
`scene` and `project` are reserved current-generation bindings.

## Boundaries

The host executes trusted Python with its ordinary authority. Import rollback
cannot undo files, sockets, host RNG state, external mutations, published
artifacts or side effects through shared constructor arguments. Source inputs
are bounded by the existing reload budget; authored execution has no forced
in-process timeout. Process-isolated Studio remains the appropriate separate
workflow for crash isolation. This feature is not a sandbox or certification.

An existing `SceneSource` can be supplied instead of a filename; it must already
be active and remains owned by its caller. Declared Studio-worker input tables
must rebuild through the supervisor. Packages cannot change their root or name
inside an existing source context. Optional line-number insertion is not
implemented: place an explicit embed call in the source instead.

Native acceptance lives in `tests/scene_project.py` and
`tests/scene_project_editor.py` under `crates/fmn-python`, both registered in the
installed-wheel gate. The separate `test_scene_project.py` and
`test_project_editor.py` tests exercise real imports, production console code
and real IPython with explicit native lifecycle/capture doubles; they are not
native rendering evidence.
