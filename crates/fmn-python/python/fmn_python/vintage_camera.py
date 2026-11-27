"""The historical moving-camera namespace over the portal's live Scene and CameraFrame."""

import sys
import types


def install_vintage_camera(native):
    """Publish the legacy import paths for this initialized native class table."""
    def _mobject_method(value):
        return (isinstance(value, (types.MethodType, types.BuiltinMethodType))
                and isinstance(value.__self__, native.Mobject))

    def _play_arguments(args):
        if not any(_mobject_method(arg) for arg in args):
            return args
        # Python evaluated explicit .animate arguments before entering play. Bind
        # those targets before a vintage call can generate another one on the same mob.
        args = tuple(native.prepare_animation(arg) if isinstance(arg, native._AnimationBuilder)
                     else arg for arg in args)
        animations = []
        index = 0
        while index < len(args):
            method = args[index]
            if not _mobject_method(method):
                animations.append(method)
                index += 1
                continue

            # One generated target per consecutive group of calls. Build before
            # the next group can replace this receiver's mobject.target.
            receiver = method.__self__
            builder = receiver.animate
            while True:
                index += 1
                start = index
                while index < len(args):
                    item = args[index]
                    if _mobject_method(item) or isinstance(item, native.Animation):
                        break
                    index += 1
                params = args[start:index]
                options = {}
                if params and isinstance(params[-1], dict):
                    params, options = params[:-1], params[-1]
                getattr(builder, method.__name__)(*params, **options)
                if index == len(args):
                    break
                method = args[index]
                if not _mobject_method(method) or method.__self__ is not receiver:
                    break
            animations.append(builder.build())

        return animations

    class MovingCameraScene(native.Scene):
        """Expose the live camera frame under its historical name and play syntax."""

        def setup(self):
            native.Scene.setup(self)
            self.camera_frame = self.camera.frame
            return self

        def play(self, *args, **kwargs):
            """Accept the historical play(frame.shift, RIGHT, frame.scale, .5) form."""
            return super().play(*_play_arguments(args), **kwargs)

    qualified_name = "manimlib.scene.moving_camera_scene"
    qualified = types.ModuleType(qualified_name)
    qualified.__package__ = "manimlib.scene"
    MovingCameraScene.__module__ = qualified_name
    MovingCameraScene.__qualname__ = "MovingCameraScene"
    qualified.MovingCameraScene = MovingCameraScene

    vintage = types.ModuleType("manimlib.imports", "Vintage Manim scene imports.")
    vintage.__package__ = "manimlib"
    for name, value in vars(native).items():
        if not name.startswith("_"):
            setattr(vintage, name, value)
    vintage.MovingCameraScene = MovingCameraScene

    # Both the extension and the wheel wrapper support qualified attribute
    # access. Keeping the module handle out of vars(native) preserves the
    # pinned root wildcard even after importing the historical namespace.
    previous_getattr = vars(native).get("__getattr__")

    def module_attribute(name):
        if name == "imports":
            return vintage
        if previous_getattr is not None:
            return previous_getattr(name)
        raise AttributeError(f"module {native.__name__!r} has no attribute {name!r}")

    scene_package = sys.modules["manimlib.scene"]
    scene_package.moving_camera_scene = qualified
    sys.modules[qualified_name] = qualified
    sys.modules[vintage.__name__] = vintage
    native.__getattr__ = module_attribute
