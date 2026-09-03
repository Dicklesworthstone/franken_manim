"""Machine-readable authority map for Rust library constructor aliases.

The Rust library deliberately exposes ergonomic snake_case constructor helpers,
while the pinned ``manimlib`` compatibility surface exposes CamelCase classes.
This module records how the seven helpers introduced by ``fm-5wq.4.141`` reach
their Python authority without pretending that every valid binding has the
same shape.
"""

from __future__ import annotations

from types import MappingProxyType
from typing import Final, NamedTuple


class ConstructorAuthority(NamedTuple):
    rust_helper: str
    reference_module: str
    reference_class: str
    binding_kind: str
    python_base: str
    python_authority_class: str
    native_builder: str | None
    rust_source: str
    rust_function: str
    rust_authority_token: str
    bridge_function: str | None
    bridge_authority_token: str | None
    rationale: str


_BINDING_KINDS: Final = frozenset(
    {
        "inherited_native_builder",
        "direct_native_builder",
        "python_identity_container",
    }
)

_REFERENCE_CLASS_BY_RUST_HELPER: Final = {
    "group": "Group",
    "v_group": "VGroup",
    "vectorized_point": "VectorizedPoint",
    "small_dot": "SmallDot",
    "sector": "Sector",
    "vector": "Vector",
    "polyline": "Polyline",
    "triangle": "Triangle",
    "rounded_rectangle": "RoundedRectangle",
    "svg_mobject": "SVGMobject",
    "tangent_line": "TangentLine",
    "curved_arrow": "CurvedArrow",
    "curved_double_arrow": "CurvedDoubleArrow",
    "curves_as_submobjects": "CurvesAsSubmobjects",
    "dashed_vmobject": "DashedVMobject",
    "v_highlight": "VHighlight",
}
REFERENCE_CLASS_BY_RUST_HELPER: Final = MappingProxyType(
    _REFERENCE_CLASS_BY_RUST_HELPER
)

LIBRARY_CONSTRUCTOR_AUTHORITIES: Final = (
    ConstructorAuthority(
        rust_helper="group",
        reference_module="manimlib.mobject.mobject",
        reference_class="Group",
        binding_kind="python_identity_container",
        python_base="Mobject",
        python_authority_class="Group",
        native_builder=None,
        rust_source="crates/fmn-library/src/vmobject.rs",
        rust_function="group",
        rust_authority_token="Mobject::group(children.into_iter().collect())",
        bridge_function=None,
        bridge_authority_token=None,
        rationale=(
            "Group must preserve the exact heterogeneous Python proxy identities; "
            "a detached Rust reconstruction would change observable object identity."
        ),
    ),
    ConstructorAuthority(
        rust_helper="polyline",
        reference_module="manimlib.mobject.geometry",
        reference_class="Polyline",
        binding_kind="direct_native_builder",
        python_base="VMobject",
        python_authority_class="Polyline",
        native_builder="_build_polyline",
        rust_source="crates/fmn-library/src/poly.rs",
        rust_function="polyline",
        rust_authority_token="Polygon::polyline(vertices)",
        bridge_function="_build_polyline",
        bridge_authority_token="fmn_library::Polygon::polyline(vertices)",
        rationale=(
            "Polyline owns a direct native point-run builder while retaining the "
            "Reference class and mutation surface."
        ),
    ),
    ConstructorAuthority(
        rust_helper="rounded_rectangle",
        reference_module="manimlib.mobject.geometry",
        reference_class="RoundedRectangle",
        binding_kind="direct_native_builder",
        python_base="Rectangle",
        python_authority_class="RoundedRectangle",
        native_builder="_build_rounded_rectangle",
        rust_source="crates/fmn-library/src/poly.rs",
        rust_function="rounded_rectangle",
        rust_authority_token="Rectangle::new()",
        bridge_function="_build_rounded_rectangle",
        bridge_authority_token="fmn_library::Rectangle::new()",
        rationale=(
            "RoundedRectangle directly installs the Atlas-built rounded point run "
            "and then exposes ordinary Reference mutation semantics."
        ),
    ),
    ConstructorAuthority(
        rust_helper="sector",
        reference_module="manimlib.mobject.geometry",
        reference_class="Sector",
        binding_kind="inherited_native_builder",
        python_base="AnnularSector",
        python_authority_class="AnnularSector",
        native_builder="_build_annular_sector",
        rust_source="crates/fmn-library/src/arc.rs",
        rust_function="sector",
        rust_authority_token="AnnularSector::sector",
        bridge_function="_build_annular_sector",
        bridge_authority_token="fmn_library::AnnularSector::new()",
        rationale=(
            "Sector is the Reference's constrained AnnularSector facade, so the "
            "parent class remains the single native-builder authority."
        ),
    ),
    ConstructorAuthority(
        rust_helper="small_dot",
        reference_module="manimlib.mobject.geometry",
        reference_class="SmallDot",
        binding_kind="inherited_native_builder",
        python_base="Dot",
        python_authority_class="Dot",
        native_builder="_build_dot",
        rust_source="crates/fmn-library/src/arc.rs",
        rust_function="small_dot",
        rust_authority_token="Dot::small().point(point)",
        bridge_function="_build_dot",
        bridge_authority_token="fmn_library::Dot::new()",
        rationale=(
            "SmallDot is a parameter-specialized Dot and must reuse Dot's native "
            "builder rather than fork its live proxy semantics."
        ),
    ),
    ConstructorAuthority(
        rust_helper="triangle",
        reference_module="manimlib.mobject.geometry",
        reference_class="Triangle",
        binding_kind="inherited_native_builder",
        python_base="RegularPolygon",
        python_authority_class="RegularPolygon",
        native_builder="_build_regular_polygon",
        rust_source="crates/fmn-library/src/poly.rs",
        rust_function="triangle",
        rust_authority_token="RegularPolygon::triangle()",
        bridge_function="_build_regular_polygon",
        bridge_authority_token="fmn_library::RegularPolygon::new(n)",
        rationale=(
            "Triangle is exactly RegularPolygon(n=3); inheritance preserves the "
            "Reference constructor and keeps one bounded compass kernel."
        ),
    ),
    ConstructorAuthority(
        rust_helper="vector",
        reference_module="manimlib.mobject.geometry",
        reference_class="Vector",
        binding_kind="inherited_native_builder",
        python_base="Arrow",
        python_authority_class="Arrow",
        native_builder="_build_arrow",
        rust_source="crates/fmn-library/src/line.rs",
        rust_function="vector",
        rust_authority_token="Arrow::vector(direction)",
        bridge_function="_build_arrow",
        bridge_authority_token="fmn_library::line::Arrow::new",
        rationale=(
            "Vector is the Reference's origin-anchored Arrow facade and therefore "
            "inherits Arrow's native geometry and tip-installation authority."
        ),
    ),
)

LIBRARY_CONSTRUCTOR_AUTHORITY_BY_HELPER: Final = MappingProxyType(
    {item.rust_helper: item for item in LIBRARY_CONSTRUCTOR_AUTHORITIES}
)


def _validate() -> None:
    helpers = [item.rust_helper for item in LIBRARY_CONSTRUCTOR_AUTHORITIES]
    if helpers != sorted(helpers):
        raise RuntimeError("library constructor authority records must be sorted")
    if len(helpers) != len(set(helpers)):
        raise RuntimeError("library constructor authority helpers must be unique")
    for item in LIBRARY_CONSTRUCTOR_AUTHORITIES:
        if item.binding_kind not in _BINDING_KINDS:
            raise RuntimeError(
                f"unknown constructor binding kind {item.binding_kind!r}"
            )
        expected_class = REFERENCE_CLASS_BY_RUST_HELPER.get(item.rust_helper)
        if expected_class != item.reference_class:
            raise RuntimeError(
                f"{item.rust_helper!r} maps to {expected_class!r}, "
                f"not {item.reference_class!r}"
            )
        has_builder = item.native_builder is not None
        has_bridge = (
            item.bridge_function is not None
            and item.bridge_authority_token is not None
        )
        if item.binding_kind == "python_identity_container":
            if has_builder or has_bridge:
                raise RuntimeError(
                    f"{item.rust_helper!r} identity container cannot name a native bridge"
                )
        elif not has_builder or not has_bridge:
            raise RuntimeError(
                f"{item.rust_helper!r} native binding requires a complete bridge"
            )


_validate()
del _validate
