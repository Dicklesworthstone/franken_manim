"""Configured scene frames over the shared native camera and scene lifecycle.

The capture Camera remains lazy. Its frame is the one identity already owned
by Scene playback, not a default pose substituted after configuration.
"""
from __future__ import annotations


def _bind(cls, name, function):
    function.__name__ = name
    function.__qualname__ = cls.__qualname__ + "." + name
    function.__module__ = cls.__module__
    setattr(cls, name, function)


def install_scene_camera_configuration(native):
    g = vars(native)
    if g.get("_FMN_SCENE_CAMERA_CONFIGURATION_INSTALLED", False):
        return
    Scene = g["Scene"]

    def __init__(self, *args, **kwargs):
        self.args = args
        self.kwargs = kwargs
        # SceneSource installs the resolved project configuration before it
        # imports authored scenes. This is the production initializer (the
        # bootstrap initializer has already been superseded), so consume the
        # live scene and writer layers here, including edits made at import.
        scene_defaults = g["_portal_configuration_section"]("scene")
        # Reference Scene.__init__: deterministic scenes are the default.
        # The compatibility portal seeds the two RNG modules source-unedited
        # scenes actually call; engine-owned randomness remains Marionette's
        # named PCG64DXSM streams.
        self.random_seed = kwargs.get(
            "random_seed", getattr(type(self), "random_seed", 0)
        )
        if self.random_seed is not None:
            getattr(g["_FMN_ROOT"], "random").seed(self.random_seed)
            g["_np"].random.seed(self.random_seed)
        # Reference Scene.__init__ constructs the camera and starts the scene
        # list as `[self.camera.frame]` (scene.py:117); the frame is an
        # ordinary, non-drawing member of that list (fm-6l2i, below).
        # Resolve the frame recipe before publishing the identity used by
        # animations and updaters. The capture Camera stays lazy, but must not
        # discard frame_config by replacing its frame with a default one.
        camera_config = kwargs.get("camera_config", None)
        if camera_config is not None and not isinstance(camera_config, dict):
            raise TypeError("Scene camera_config must be a dict")
        # Reference merge order: manim_config.camera (default_config.yml plus
        # the cwd custom_config.yml: 1920x1080, 30 fps, #333333 by default),
        # then the class default_camera_config, then the constructor's.
        self.camera_config = g["_FMN_ROOT"].merge_dicts_recursively(
            g["_reference_camera_layer"](),
            type(self).default_camera_config,
            {} if camera_config is None else camera_config,
        )
        # Select Choreo's rational clock while the arena is still empty.
        # Bare run()/play()/wait() must use the same configured frame grid as
        # a render generation. Reuse the native pristine-scene boundary; it
        # acquires no output sink, and later renders still select their own
        # clock/seed before construction. Keep the initial native seed at 0.
        native._portal_prepare_scene_clock(self, self.camera_config["fps"])
        frame_config = self.camera_config.get("frame_config", {})
        if not isinstance(frame_config, dict):
            raise TypeError("Camera frame_config must be a dict")
        # Do not retain the caller's nested dictionary as mutable scene state.
        # CameraFrame converts dimensional values into its owned native core.
        if "frame_config" in self.camera_config:
            self.camera_config["frame_config"] = dict(frame_config)
        self.frame = g["CameraFrame"](**frame_config)
        self.frame.reorient(*self.default_frame_orientation)
        self.frame.make_orientation_default()
        # fm-6l2i: the list starts with the frame. Scene.add sorts it by its
        # z_index of -1, clear() and remove() drop it, and only a listed
        # frame's updaters run. The native add leaves id_to_mobject_map
        # alone, which the Reference fills only from add().
        g["_SceneCore"].add(self, self.frame)
        # The listed frame stays listed if `self.frame` is later reassigned,
        # as in the Reference; render generations carry this one across.
        self.__dict__["_fmn_construction_frame"] = self.frame
        self.num_plays = 0
        self.undo_stack = []
        self.redo_stack = []
        self.id_to_mobject_map = {}
        self.pan_sensitivity = float(
            kwargs.get("pan_sensitivity", type(self).pan_sensitivity)
        )
        self.scroll_sensitivity = float(
            kwargs.get("scroll_sensitivity", type(self).scroll_sensitivity)
        )
        self.drag_to_pan = bool(
            kwargs.get("drag_to_pan", type(self).drag_to_pan)
        )
        self.max_num_saved_states = int(
            kwargs.get("max_num_saved_states", type(self).max_num_saved_states)
        )
        # Reference Scene.__init__ keeps two live Point mobjects, distinct
        # from the dispatcher's coordinate arrays. InteractiveScene's label
        # updater reads the scene's hover Point on every frame. A drag moves
        # the other Point without changing that hover position.
        self.mouse_point = g["Point"]()
        self.mouse_drag_point = g["Point"]()
        # Host window is Studio-owned. Pointer/scroll/key chords below stay
        # live without one; pan_3d/pan key-state is skipped until a host
        # adapter is bound.
        self.window = kwargs.get("window")
        self.presenter_mode = bool(kwargs.get("presenter_mode", False))
        self.hold_on_wait = self.presenter_mode
        self.preview_while_skipping = bool(
            kwargs.get("preview_while_skipping", scene_defaults.get("preview_while_skipping", True))
        )
        self.quit_interaction = False
        self.skip_animations = bool(kwargs.get("skip_animations", False))
        self.start_at_animation_number = kwargs.get("start_at_animation_number")
        self.end_at_animation_number = kwargs.get("end_at_animation_number")
        # Reference order: original_skipping_status is captured BEFORE the
        # start-at forcing, so update_skipping_status can stop_skipping back
        # to live rendering once num_plays reaches start_at_animation_number.
        self.original_skipping_status = self.skip_animations
        if self.start_at_animation_number is not None:
            self.skip_animations = True
        self.show_animation_progress = bool(
            kwargs.get("show_animation_progress", scene_defaults.get("show_animation_progress", False))
        )
        self.always_update_mobjects = bool(
            kwargs.get("always_update_mobjects", False)
        )
        default_wait_time = type(self).default_wait_time
        # Preserve an authored class default even when its numeric value
        # happens to equal Scene's built-in default. Do not compare values or
        # ask a descriptor to run twice merely to identify the owner.
        for cls in type(self).__mro__:
            if "default_wait_time" in vars(cls):
                if cls is Scene:
                    default_wait_time = scene_defaults.get("default_wait_time", default_wait_time)
                break
        self.default_wait_time = float(kwargs.get("default_wait_time", default_wait_time))
        self.leave_progress_bars = bool(
            kwargs.get("leave_progress_bars", scene_defaults.get("leave_progress_bars", False))
        )
        # Reference Scene.__init__: project writer defaults, then class
        # defaults, then constructor input; nested mappings merge recursively.
        # SceneFileWriter only records these knobs. Native negotiation remains
        # responsible for validating codec/quality combinations before I/O.
        file_writer_config = kwargs.get("file_writer_config", None)
        if file_writer_config is not None and not isinstance(file_writer_config, dict):
            raise TypeError("Scene file_writer_config must be a dict")
        self.file_writer_config = g["_FMN_ROOT"].merge_dicts_recursively(
            g["_portal_configuration_section"]("file_writer"),
            type(self).default_file_writer_config,
            {} if file_writer_config is None else file_writer_config,
        )
        self.file_writer = g["SceneFileWriter"](self, **self.file_writer_config)

    def camera(self):
        # Scene and Camera share one live CameraFrame identity, matching the
        # Reference while capture configuration is built by Lumen. samples
        # ride the Scene.samples default unless camera_config names it.
        camera = self.__dict__.get("_camera")
        if camera is None:
            config = dict(self.camera_config)
            # Scene already consumed this recipe into its authoritative frame.
            # Re-evaluating it for a temporary CameraFrame repeats authored
            # conversions and can observe caller-mutated arrays. The temporary
            # capture frame is discarded below; only capture options remain.
            config.pop("frame_config", None)
            if "samples" not in config:
                config["samples"] = int(getattr(self, "samples", 0))
            camera = g["Camera"](**config)
            # Retain edits made through scene.frame before capture was needed.
            # Apply Lumen's pixel-aspect policy to that actual frame, not to the
            # temporary constructor frame which is about to be discarded: the
            # frame keeps its height, as a constructed camera's does (BN-25).
            camera.frame = self.frame
            camera.resize_frame_shape(fixed_dimension=True)
            self.__dict__["_camera"] = camera
        return camera

    _bind(Scene, "__init__", __init__)
    camera.__name__ = "camera"
    camera.__qualname__ = Scene.__qualname__ + ".camera"
    camera.__module__ = Scene.__module__
    Scene.camera = property(camera, doc=Scene.camera.__doc__)
    g["_FMN_SCENE_CAMERA_CONFIGURATION_INSTALLED"] = True
