"""Install the pinned shared animation protocol onto the native module."""


def install(native):
    g = vars(native)
    if g.get("_FMN_ANIMATION_SEMANTICS_INSTALLED", False):
        return
    np = g["_np"]
    interpolate_value = g["_interpolate"]
    copy_module = g["_copy"]
    smooth_rate = g["_smooth_rate"]
    refuse_unrouted = g["_refuse_unrouted"]
    Mobject = g["Mobject"]
    Animation = g["Animation"]
    NativeAnimation = g["_NativeAnimation"]
    Transform = g["Transform"]
    ReplacementTransform = g["ReplacementTransform"]
    TransformFromCopy = g["TransformFromCopy"]
    DrawBorderThenFill = g["DrawBorderThenFill"]
    FadeTransform = g["FadeTransform"]
    FadeTransformPieces = g["FadeTransformPieces"]
    original_transform_init = Transform.__init__
    original_requires_python = g["_requires_python_animation"]

    def uses_python_path(animation):
        path = getattr(animation, "path_func", None)
        return path is not None and getattr(path, "_fmn_path_arc", None) is None

    def transform_init(
        self,
        mobject,
        target_mobject=None,
        path_arc=0.0,
        path_arc_axis=g["_OUT"],
        path_func=None,
        **kwargs,
    ):
        if path_func is not None and not callable(path_func):
            raise TypeError("Transform path_func must be callable")
        python_path = (
            path_func is not None
            and getattr(path_func, "_fmn_path_arc", None) is None
        )
        # The callback segment now releases the Stage around every Python
        # hook. Keep scalar arc factories on Choreo's native path, but do not
        # reject an authored point-array map that this boundary can execute.
        # Target-less Swap/CyclicReplace still require their native lowering.
        if python_path and self._target_attr is None:
            refuse_unrouted(type(self).__name__ + "()", [("path_func", True)])
        if python_path and isinstance(mobject, g["CameraFrame"]):
            raise NotImplementedError(
                "Python path_func animations of the camera frame await the "
                "camera track's per-frame callback seam"
            )
        original_transform_init(
            self,
            mobject,
            target_mobject,
            path_arc=path_arc,
            path_arc_axis=path_arc_axis,
            path_func=None if python_path else path_func,
            **kwargs,
        )
        self.path_func = path_func

    def requires_python_animation(animation):
        # Consult the live path, not a constructor-time flag: scene authors
        # may replace it between plays or install it on an animation instance.
        if (
            isinstance(animation, Transform)
            and animation._target_attr is not None
        ):
            if uses_python_path(animation):
                return True
            # Compare against the nearest shipped class, not Transform alone:
            # Grow/Indicate/etc. already have native implementations of their
            # own hooks. Only authored changes require callback dispatch.
            for cls in type(animation).__mro__:
                baseline = transform_protocols.get(cls)
                if baseline is not None:
                    for name, expected in baseline.items():
                        method = getattr(animation, name)
                        if getattr(method, "__func__", method) is not expected:
                            return True
                    break
        return original_requires_python(animation)

    def mobject_str(self):
        return type(self).__name__

    def interpolate_uniform(start, end, alpha):
        if isinstance(start, np.ndarray) or isinstance(end, np.ndarray):
            return interpolate_value(np.asarray(start), np.asarray(end), alpha)
        if isinstance(start, tuple) or isinstance(end, tuple):
            value = interpolate_value(
                np.asarray(start, dtype=float),
                np.asarray(end, dtype=float),
                alpha,
            )
            return tuple(value.tolist())
        if isinstance(start, list) or isinstance(end, list):
            value = interpolate_value(
                np.asarray(start, dtype=float),
                np.asarray(end, dtype=float),
                alpha,
            )
            return value.tolist()
        return interpolate_value(start, end, alpha)

    def mobject_interpolate(self, mobject1, mobject2, alpha, path_func=None):
        if path_func is None:
            path_func = g["straight_path"]
        alpha = float(alpha)
        locked_data = getattr(self, "locked_data_keys", ())
        constant_data = getattr(self, "const_data_keys", ())
        data_keys = [
            key for key in self.data.dtype.names if key not in locked_data
        ]
        # Point-free family roots can still carry animated uniforms. They
        # have no first record from which to broadcast a constant data field.
        if len(self.data) == 0:
            data_keys = []
        if data_keys:
            self.note_changed_data()
        for key in data_keys:
            start = mobject1.data[key]
            end = mobject2.data[key]
            if key in constant_data:
                start = start[0]
                end = end[0]
            value = (
                path_func(start, end, alpha)
                if key in self.pointlike_data_keys
                else interpolate_value(start, end, alpha)
            )
            self.data[key][:] = value
        locked_uniforms = getattr(self, "locked_uniform_keys", ())
        for key in tuple(self.uniforms):
            if key in locked_uniforms:
                continue
            # Match Choreo's interpolate_fields: typed flags and joint style
            # stay the live object's own; only numeric uniforms interpolate.
            if key == "joint_type" or isinstance(self.uniforms[key], (bool, np.bool_)):
                continue
            if key not in mobject1.uniforms or key not in mobject2.uniforms:
                continue
            self.uniforms[key] = interpolate_uniform(
                mobject1.uniforms[key],
                mobject2.uniforms[key],
                alpha,
            )
        return self

    def validate_input_type(self, mobject):
        del self
        if not isinstance(mobject, Mobject):
            raise TypeError("Animation only works for Mobjects.")

    def animation_init(
        self,
        mobject,
        run_time=1.0,
        time_span=None,
        lag_ratio=0.0,
        rate_func=None,
        name="",
        remover=False,
        final_alpha_value=1.0,
        suspend_mobject_updating=False,
        **kwargs,
    ):
        self._validate_input_type(mobject)
        self.mobject = mobject
        self.run_time = float(run_time)
        self.time_span = time_span
        self.lag_ratio = float(lag_ratio)
        self.rate_func = (
            rate_func
            if rate_func is not None
            else g.get("smooth", smooth_rate)
        )
        self.name = name or type(self).__name__ + str(mobject)
        self.remover = bool(remover)
        self.final_alpha_value = float(final_alpha_value)
        self.suspend_mobject_updating = bool(suspend_mobject_updating)
        self.__dict__.update(kwargs)

    def ensure_runtime_defaults(self):
        if self.run_time is None:
            self.run_time = 1.0
        if self.rate_func is None:
            self.rate_func = g.get("smooth", smooth_rate)
        if self.lag_ratio is None:
            self.lag_ratio = 0.0

    def animation_str(self):
        return getattr(self, "name", type(self).__name__)

    def create_starting_mobject(self):
        return self.mobject.copy()

    def get_all_mobjects(self):
        return self.mobject, self.starting_mobject

    def get_all_families_zipped(self):
        return zip(*[
            mobject.get_family()
            for mobject in self.get_all_mobjects()
        ])

    def get_all_mobjects_to_update(self):
        result = []
        seen = set()
        for mobject in self.get_all_mobjects():
            identity = id(mobject)
            if mobject is self.mobject or identity in seen:
                continue
            seen.add(identity)
            result.append(mobject)
        return result

    def update_mobjects(self, dt):
        for mobject in self.get_all_mobjects_to_update():
            mobject.update(dt)

    def animation_begin(self):
        self._ensure_runtime_defaults()
        if self.time_span is not None:
            self.run_time = max(float(self.time_span[1]), self.run_time)
        self.mobject.set_animating_status(True)
        self.starting_mobject = self.create_starting_mobject()
        self.mobject_was_updating = False
        if self.suspend_mobject_updating:
            self.mobject_was_updating = not self.mobject._is_updating_suspended()
            self.mobject.suspend_updating()
        self.families = list(self.get_all_families_zipped())
        self.interpolate(0.0)

    def animation_finish(self):
        self.interpolate(self.final_alpha_value)
        self.mobject.set_animating_status(False)
        if self.suspend_mobject_updating and self.mobject_was_updating:
            self.mobject.resume_updating()

    def animation_copy(self):
        return copy_module.deepcopy(self)

    def update_rate_info(self, run_time=None, rate_func=None, lag_ratio=None):
        self.run_time = run_time or self.run_time
        self.rate_func = rate_func or self.rate_func
        self.lag_ratio = lag_ratio or self.lag_ratio
        return self

    def animation_interpolate(self, alpha):
        self.interpolate_mobject(float(alpha))

    def animation_update(self, alpha):
        self.interpolate(alpha)

    def time_spanned_alpha(self, alpha):
        if self.time_span is None:
            return float(alpha)
        start, end = self.time_span
        return np.clip(
            float(alpha) * self.run_time - start,
            0.0,
            end - start,
        ) / (end - start)

    def interpolate_mobject(self, alpha):
        spanned_alpha = self.time_spanned_alpha(alpha)
        for index, mobjects in enumerate(self.families):
            sub_alpha = self.get_sub_alpha(
                spanned_alpha,
                index,
                len(self.families),
            )
            self.interpolate_submobject(*mobjects, sub_alpha)

    def interpolate_submobject(self, submobject, starting_submobject, alpha):
        del self, submobject, starting_submobject, alpha

    def get_sub_alpha(self, alpha, index, num_submobjects):
        full_length = (num_submobjects - 1) * self.lag_ratio + 1
        value = float(alpha) * full_length
        lower = index * self.lag_ratio
        return self.rate_func(min(max(value - lower, 0.0), 1.0))

    def set_run_time(self, run_time):
        self.run_time = float(run_time)
        return self

    def get_run_time(self):
        if self.time_span:
            return max(self.run_time, float(self.time_span[1]))
        return self.run_time

    def set_rate_func(self, rate_func):
        self.rate_func = rate_func
        return self

    def get_rate_func(self):
        return self.rate_func

    def set_name(self, name):
        self.name = name
        return self

    def is_remover(self):
        return bool(getattr(self, "remover", False))

    def clean_up_from_scene(self, scene):
        if self.is_remover():
            scene.remove(self.mobject)

    def native_animation_init(
        self,
        mobject,
        run_time=None,
        rate_func=None,
        lag_ratio=None,
        time_span=None,
        final_alpha_value=1.0,
        suspend_mobject_updating=False,
        name="",
        remover=False,
        **kwargs,
    ):
        # Native compositions derive their root from their members in Choreo.
        # None is an internal sentinel for that class family, not a generally
        # valid Animation target. Do not trust a spoofable _native_kind string.
        if mobject is not None or not isinstance(self, g["AnimationGroup"]):
            self._validate_input_type(mobject)
        refuse_unrouted(
            type(self).__name__ + "()",
            [(key, True) for key in sorted(kwargs)],
        )
        self.mobject = mobject
        self.run_time = run_time
        self.rate_func = rate_func
        self.lag_ratio = lag_ratio
        self.time_span = (
            None
            if time_span is None
            else (float(time_span[0]), float(time_span[1]))
        )
        self.final_alpha_value = float(final_alpha_value)
        self.suspend_mobject_updating = bool(suspend_mobject_updating)
        suffix = "" if mobject is None else str(mobject)
        self.name = name or type(self).__name__ + suffix
        self.remover = bool(remover)

    def init_path_func(self):
        if getattr(self, "path_func", None) is not None:
            return
        if float(self.path_arc) == 0.0:
            self.path_func = g["straight_path"]
        else:
            self.path_func = g["path_along_arc"](
                float(self.path_arc),
                self.path_arc_axis,
            )

    def create_target(self):
        target = getattr(self, "target_mobject", None)
        return self.mobject.copy() if target is None else target

    def check_target_mobject_validity(self):
        if not isinstance(self.target_mobject, Mobject):
            raise TypeError("Transform target must be a Mobject")

    def native_target(self):
        # Scene.play handles camera transforms before callback dispatch. Do
        # not silently lower a later-assigned path or authored hook to its
        # native endpoint-only camera track.
        if (
            isinstance(self.mobject, g["CameraFrame"])
            and requires_python_animation(self)
        ):
            raise NotImplementedError(
                "Python-callback animations of the camera frame await the "
                "camera track's per-frame callback seam"
            )
        # CyclicReplace/Swap carry multiple source mobjects rather than a
        # Transform target. Their native constructor owns those destinations.
        if self._target_attr is None:
            return None
        self.target_mobject = self.create_target()
        self.check_target_mobject_validity()
        return self.target_mobject

    def transform_begin(self):
        self._ensure_runtime_defaults()
        self.init_path_func()
        self.target_mobject = self.create_target()
        self.check_target_mobject_validity()
        if self.mobject.is_aligned_with(self.target_mobject):
            self.target_copy = self.target_mobject
        else:
            self.target_copy = self.target_mobject.copy()
        self.mobject.align_data_and_family(self.target_copy)
        Animation.begin(self)
        # Equal endpoint records do not imply a constant authored path: a
        # closed excursion may leave and return to the very same points.
        # Explicit user locks remain respected by Mobject.interpolate.
        if not self.mobject.has_updaters() and not uses_python_path(self):
            self.mobject.lock_matching_data(
                self.starting_mobject,
                self.target_copy,
            )

    def transform_finish(self):
        Animation.finish(self)
        self.mobject.unlock_data()

    def transform_cleanup(self, scene):
        Animation.clean_up_from_scene(self, scene)
        if self.replace_mobject_with_target_in_scene:
            scene.remove(self.mobject)
            scene.add(self.target_mobject)

    def transform_all_mobjects(self):
        return (
            self.mobject,
            self.starting_mobject,
            self.target_mobject,
            self.target_copy,
        )

    def transform_families(self):
        return zip(*[
            mobject.get_family()
            for mobject in (
                self.mobject,
                self.starting_mobject,
                self.target_copy,
            )
        ])

    def transform_submobject(
        self,
        submobject,
        starting_submobject,
        target_copy,
        alpha,
    ):
        submobject.interpolate(
            starting_submobject,
            target_copy,
            alpha,
            self.path_func,
        )
        return self

    def transform_interpolate_mobject(self, alpha):
        # Replace Transform's bootstrap-local straight-only implementation;
        # otherwise it shadows Animation's family/lag dispatch and none of
        # the installed path, style, uniform, or subclass hooks can run.
        Animation.interpolate_mobject(self, alpha)

    def transform_from_copy_init(self, mobject, target_mobject, **kwargs):
        Transform.__init__(self, mobject.copy(), target_mobject, **kwargs)
        # The public constructor already froze the source copy. The native
        # replacement transform must consume that exact object; invoking the
        # native copy constructor again would leave this first copy rooted.
        self._native_kind = "replacement_transform"

    Mobject.__str__ = mobject_str
    Mobject.interpolate = mobject_interpolate
    Animation.__init__ = animation_init
    Animation._validate_input_type = validate_input_type
    Animation._ensure_runtime_defaults = ensure_runtime_defaults
    Animation.__str__ = animation_str
    Animation.begin = animation_begin
    Animation.finish = animation_finish
    Animation.create_starting_mobject = create_starting_mobject
    Animation.get_all_mobjects = get_all_mobjects
    Animation.get_all_families_zipped = get_all_families_zipped
    Animation.get_all_mobjects_to_update = get_all_mobjects_to_update
    Animation.update_mobjects = update_mobjects
    Animation.copy = animation_copy
    Animation.update_rate_info = update_rate_info
    Animation.interpolate = animation_interpolate
    Animation.update = animation_update
    Animation.time_spanned_alpha = time_spanned_alpha
    Animation.interpolate_mobject = interpolate_mobject
    Animation.interpolate_submobject = interpolate_submobject
    Animation.get_sub_alpha = get_sub_alpha
    Animation.set_run_time = set_run_time
    Animation.get_run_time = get_run_time
    Animation.set_rate_func = set_rate_func
    Animation.get_rate_func = get_rate_func
    Animation.set_name = set_name
    Animation.is_remover = is_remover
    Animation.clean_up_from_scene = clean_up_from_scene
    NativeAnimation.__init__ = native_animation_init
    Transform.__init__ = transform_init
    Transform.replace_mobject_with_target_in_scene = False
    Transform.init_path_func = init_path_func
    Transform.create_target = create_target
    Transform.check_target_mobject_validity = check_target_mobject_validity
    Transform._native_target = native_target
    Transform.begin = transform_begin
    Transform.finish = transform_finish
    Transform.clean_up_from_scene = transform_cleanup
    Transform.get_all_mobjects = transform_all_mobjects
    Transform.get_all_families_zipped = transform_families
    Transform.interpolate_mobject = transform_interpolate_mobject
    Transform.interpolate_submobject = transform_submobject
    ReplacementTransform.replace_mobject_with_target_in_scene = True
    TransformFromCopy.replace_mobject_with_target_in_scene = True
    TransformFromCopy.__init__ = transform_from_copy_init

    def draw_border_get_outline(self):
        """Expose a native-backed outline copy without changing the source."""
        outline = self.mobject.copy()
        outline.set_fill(opacity=0)
        for member in outline.family_members_with_points():
            member.set_stroke(
                color=self.stroke_color or member.get_stroke_color(),
                width=self.stroke_width,
                behind=self.mobject.stroke_behind,
            )
        return outline

    def fade_transform_ghost_to(self, source, target):
        source.replace(target, stretch=self.stretch, dim_to_match=self.dim_to_match)
        source.set_uniform(**target.get_uniforms())
        source.set_opacity(0)

    def fade_transform_pieces_ghost_to(self, source, target):
        for source_member, target_member in zip(source.get_family(), target.get_family()):
            FadeTransform.ghost_to(self, source_member, target_member)

    DrawBorderThenFill.get_outline = draw_border_get_outline
    FadeTransform.ghost_to = fade_transform_ghost_to
    FadeTransformPieces.ghost_to = fade_transform_pieces_ghost_to

    # Both the embedded extension and installed wheel expose these same class
    # objects. Give their methods the public identities before either route
    # applies schema provenance or resolves qualified compatibility imports.
    semantic_methods = {
        Mobject: ("__str__", "interpolate"),
        Animation: (
            "__init__", "_validate_input_type", "_ensure_runtime_defaults",
            "__str__", "begin", "finish", "create_starting_mobject",
            "get_all_mobjects", "get_all_families_zipped",
            "get_all_mobjects_to_update", "update_mobjects", "copy",
            "update_rate_info", "interpolate", "update", "time_spanned_alpha",
            "interpolate_mobject", "interpolate_submobject", "get_sub_alpha",
            "set_run_time", "get_run_time", "set_rate_func", "get_rate_func",
            "set_name", "is_remover", "clean_up_from_scene",
        ),
        NativeAnimation: ("__init__",),
        Transform: (
            "__init__", "init_path_func", "create_target", "check_target_mobject_validity",
            "_native_target", "begin", "finish", "clean_up_from_scene",
            "get_all_mobjects", "get_all_families_zipped",
            "interpolate_mobject", "interpolate_submobject",
        ),
        TransformFromCopy: ("__init__",),
        DrawBorderThenFill: ("get_outline",),
        FadeTransform: ("ghost_to",),
        FadeTransformPieces: ("ghost_to",),
    }
    for cls, names in semantic_methods.items():
        for name in names:
            function = vars(cls)[name]
            function.__name__ = name
            function.__qualname__ = f"{cls.__qualname__}.{name}"
            function.__module__ = cls.__module__
    # Capture after shared installation so inherited methods and qualified
    # exports have their final identities. Retain function objects, not just
    # class names, so later class/instance monkeypatches are visible too.
    transform_hooks = (
        "create_target", "create_starting_mobject", "init_path_func",
        "check_target_mobject_validity", "get_all_mobjects",
        "get_all_families_zipped", "get_all_mobjects_to_update",
        "get_sub_alpha", "time_spanned_alpha",
    )
    transform_protocols = {
        cls: {name: getattr(cls, name) for name in transform_hooks}
        for cls in tuple(g.values())
        if isinstance(cls, type) and issubclass(cls, Transform)
        and getattr(cls, "_target_attr", None) is not None
    }
    g["_requires_python_animation"] = requires_python_animation
    g["_FMN_ANIMATION_SEMANTICS_INSTALLED"] = True
