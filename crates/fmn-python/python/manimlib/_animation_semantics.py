"""Install the pinned shared animation protocol onto the native module."""


def install(native):
    g = vars(native)
    np = g["_np"]
    interpolate_value = g["_interpolate"]
    copy_module = g["_copy"]
    root_module = g["_FMN_ROOT"]
    smooth_rate = g["_smooth_rate"]
    refuse_unrouted = g["_refuse_unrouted"]
    Mobject = g["Mobject"]
    Animation = g["Animation"]
    NativeAnimation = g["_NativeAnimation"]
    Transform = g["Transform"]
    ReplacementTransform = g["ReplacementTransform"]
    TransformFromCopy = g["TransformFromCopy"]

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
            return tuple(float(component) for component in value)
        if isinstance(start, list) or isinstance(end, list):
            value = interpolate_value(
                np.asarray(start, dtype=float),
                np.asarray(end, dtype=float),
                alpha,
            )
            return [float(component) for component in value]
        return interpolate_value(start, end, alpha)

    def mobject_interpolate(self, mobject1, mobject2, alpha, path_func=None):
        if path_func is None:
            path_func = getattr(root_module, "straight_path")
        alpha = float(alpha)
        locked_data = getattr(self, "locked_data_keys", ())
        constant_data = getattr(self, "const_data_keys", ())
        data_keys = [
            key for key in self.data.dtype.names if key not in locked_data
        ]
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
            else getattr(root_module, "smooth", smooth_rate)
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
            self.rate_func = getattr(root_module, "smooth", smooth_rate)
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
            self.path_func = getattr(root_module, "straight_path")
        else:
            self.path_func = getattr(root_module, "path_along_arc")(
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
        if not self.mobject.has_updaters():
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

    def transform_from_copy_init(self, mobject, target_mobject, **kwargs):
        Transform.__init__(self, mobject.copy(), target_mobject, **kwargs)

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
    Transform.interpolate_submobject = transform_submobject
    ReplacementTransform.replace_mobject_with_target_in_scene = True
    TransformFromCopy.replace_mobject_with_target_in_scene = True
    TransformFromCopy.__init__ = transform_from_copy_init
