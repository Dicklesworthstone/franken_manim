"""Arrow authoring through the VMobject lifecycle and Atlas's outline builder.

Python owns the recipe and public hook dispatch. Atlas still constructs the
filled/curved outline and tip; Marionette owns point publication, schemas and
live views. Rebuilding an arrow must not replace its record schema or family.
"""
from __future__ import annotations


def install_arrow_geometry(native):
    g = vars(native)
    if g.get("_FMN_ARROW_GEOMETRY_INSTALLED", False):
        return
    Arrow, np = g["Arrow"], g["_np"]

    def arrow_init(
        self, start=g["_LEFT"], end=g["_LEFT"], buff=0.25, path_arc=0.0,
        fill_color=g["_DEFAULT_LIGHT_COLOR"], fill_opacity=1.0, stroke_width=0.0,
        thickness=3.0, tip_width_ratio=5, tip_angle=g["_math"].pi / 3,
        max_tip_length_to_length_ratio=0.5, max_width_to_length_ratio=0.1,
        **kwargs,
    ):
        self.thickness, self.tip_width_ratio = float(thickness), float(tip_width_ratio)
        self.tip_angle = float(tip_angle)
        self.max_tip_length_to_length_ratio = float(max_tip_length_to_length_ratio)
        self.max_width_to_length_ratio = float(max_width_to_length_ratio)
        # Line installs the endpoint recipe, then the native base dispatches
        # init_data/init_points/init_uniforms and VMobject runs init_colors.
        # Preserve cooperative constructor dispatch instead of running a
        # second hook pass over an already constructed native arrow.
        super(Arrow, self).__init__(
            start, end, buff=buff, path_arc=path_arc, fill_color=fill_color,
            fill_opacity=fill_opacity, stroke_width=stroke_width, **kwargs,
        )

    def init_points(self):
        # Unlike inherited Line.init_points, this cannot become a plain line.
        # Keep the public endpoint hook virtual for authored arrow subclasses.
        return self.set_points_by_ends(self.start, self.end, self.buff, self.path_arc)

    def set_points_by_ends(self, start, end, buff=0, path_arc=0):
        start, end = g["_vec3"](start), g["_vec3"](end)
        # Stage the entire native outline before touching live records. The
        # original detached builder installed a new tree, dropping custom
        # dtype lanes and init_data-owned children on every scale/rebuild.
        candidate = g["_native_shell_factory"]()
        specs, tip_index = candidate._build_arrow(
            g["_native_shell_factory"], start, end, float(buff), float(path_arc),
            self.thickness, self.tip_width_ratio, self.tip_angle,
            self.max_tip_length_to_length_ratio, self.max_width_to_length_ratio,
        )
        if specs:
            raise RuntimeError("a point-only native Arrow builder returned children")
        self.set_points(candidate.get_points())
        self.tip_index = tip_index
        self.start, self.end = np.array(start), np.array(end)
        return self

    for name, function in (("__init__", arrow_init), ("init_points", init_points),
                           ("set_points_by_ends", set_points_by_ends)):
        function.__name__ = name
        function.__qualname__ = Arrow.__qualname__ + "." + name
        function.__module__ = Arrow.__module__
        setattr(Arrow, name, function)
    g["_FMN_ARROW_GEOMETRY_INSTALLED"] = True
