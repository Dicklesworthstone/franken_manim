"""In-memory camera frames from the same native Lumen renderer as file output.

A capture copies native records, not Python objects. Detached and scene-bound
families can be observed without adoption, clocks, callbacks or file creation.
Successful captures replace the previous immutable frame atomically.
"""
from __future__ import annotations

from typing import Any

_MAX_NODES = 16_384
_MAX_EDGES = 131_072


def _freeze_graph(roots, mobject_type):
    nodes, rows, indices, root_indices = [], [], {}, []

    def intern(node):
        if not isinstance(node, mobject_type):
            raise TypeError("Camera.capture accepts Mobject families only")
        marker = id(node)
        if marker not in indices:
            if len(nodes) == _MAX_NODES:
                raise ValueError("camera capture family exceeds its node budget")
            indices[marker] = len(nodes)
            nodes.append(node)
        return indices[marker]

    if len(roots) > _MAX_NODES:
        raise ValueError("camera capture exceeds its root budget")
    for root in roots:
        root_indices.append(intern(root))
    cursor, edges = 0, 0
    while cursor < len(nodes):
        children = nodes[cursor].submobjects
        if not isinstance(children, (list, tuple)):
            raise TypeError("camera capture requires a finite submobjects list")
        edges += len(children)
        if edges > _MAX_EDGES:
            raise ValueError("camera capture family exceeds its edge budget")
        rows.append([intern(child) for child in tuple(children)])
        cursor += 1
    return nodes, rows, root_indices


def install_camera_capture(native: Any) -> None:
    """Patch existing Camera classes, preserving subclass and import identity."""
    g = vars(native)
    if g.get("_FMN_CAMERA_CAPTURE_INSTALLED", False):
        return
    Camera, Mobject, Capture = g["Camera"], g["Mobject"], g["_CameraCapture"]
    np = g["_np"]

    def render(self, roots, refresh):
        if self.__dict__.get("_fmn_capture_busy", False):
            raise RuntimeError("Camera.capture cannot recursively capture the same camera")
        self.__dict__["_fmn_capture_busy"] = True
        try:
            if refresh:
                self.refresh_uniforms()
            nodes, rows, indices = _freeze_graph(roots, Mobject)
            # Every authored getter executes before the native boundary. No
            # engine borrow crosses a descriptor, iterator or uniform hook.
            capture = Capture(self._core, self.frame._core,
                              tuple(self.background_rgba),
                              tuple(self.light_source.get_location()),
                              nodes, rows, indices,
                              getattr(self, "capture_threads", 1))
            self.__dict__["_fmn_camera_capture"] = capture
        finally:
            self.__dict__["_fmn_capture_busy"] = False

    def capture(self, *mobjects):
        render(self, mobjects, True)

    def clear(self):
        render(self, (), False)

    def current(self):
        snapshot = self.__dict__.get("_fmn_camera_capture")
        if snapshot is None or snapshot.size != tuple(self.get_pixel_shape()):
            self.clear()
            snapshot = self.__dict__["_fmn_camera_capture"]
        return snapshot

    def pixels(self):
        snapshot = current(self)
        width, height = snapshot.size
        # The owned byte snapshot cannot alias the native frame or a live
        # scene view. Return an independently writable Reference-shaped array.
        return np.frombuffer(snapshot.pixels(), dtype=np.uint8).reshape(height, width, 4).copy()

    for name, method in {"capture": capture, "clear": clear, "get_pixel_array": pixels}.items():
        method.__name__ = name
        method.__qualname__ = Camera.__qualname__ + "." + name
        method.__module__ = Camera.__module__
        setattr(Camera, name, method)
    g["_FMN_CAMERA_CAPTURE_INSTALLED"] = True
