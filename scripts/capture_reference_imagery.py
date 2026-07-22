#!/usr/bin/env python3
"""One-time Reference imagery capture for the Look Gallery (fm-xb3, §16.3).

Renders the G0-2 calibration set with the pinned Reference engine
(3b1b/manim @ 6199a00d4c1b1127ebe45cb629c3f22538b10e13, at scripts/manim_ref)
and stores stills + a provenance manifest under gallery/reference_captures/
(gitignored — private fixtures per the §15.3 policy; the committed record is
docs/look_gallery/CAPTURE_INVENTORY.md).

THE CAPTURE DOCTRINE (D-16, §16.3): capture once, on one recorded
environment, and never maintain that environment again. There is no
certified Pango/llvmpipe stack to keep alive — the environment used is
*recorded* in PROVENANCE.json, and the captures are kept forever. The
Reference is imagery, never a pixel warden: these files feed the
human-judged Look Gallery and G0-2's calibration study (fm-k77), not any
bit-comparison gate.

Environment (only needed once, on the capture machine): the full Reference
import closure INCLUDING the GL stack — numpy scipy fonttools rich
mapbox_earcut pyyaml colour screeninfo appdirs tqdm validators addict
moderngl moderngl_window PyOpenGL pillow manimpango — plus a working OpenGL
context (a real GPU or EGL/OSMesa headless). Missing pieces fail with a
named capability error below, never a partial capture.

Usage:  python scripts/capture_reference_imagery.py [--out DIR]
"""

import argparse
import hashlib
import json
import os
import platform
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REF = os.path.join(HERE, "manim_ref")
DEFAULT_OUT = os.path.join(HERE, "..", "gallery", "reference_captures")

# The calibration set (§20.1 spike 2). Keep ids in lockstep with
# docs/look_gallery/CAPTURE_INVENTORY.md.
CAPTURE_IDS = [
    "gradient_fills",
    "self_intersections",
    "joints_and_caps",
    "glow",
    "lighting_3d",
    "text_sample",
]


def fail(name: str, detail: str) -> "NoReturn":  # noqa: F821
    print(f"CAPABILITY ERROR [{name}]: {detail}", file=sys.stderr)
    print("No captures were written (capture is all-or-nothing).", file=sys.stderr)
    raise SystemExit(2)


def ref_commit() -> str:
    return subprocess.run(
        ["git", "-C", REF, "rev-parse", "HEAD"],
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


def import_reference():
    """Import the full Reference (GL stack included) with named failures."""
    if not os.path.isdir(os.path.join(REF, "manimlib")):
        fail(
            "reference-checkout",
            f"pinned Reference not found at {REF}; clone 3b1b/manim and "
            "check out 6199a00d4c1b1127ebe45cb629c3f22538b10e13",
        )
    sys.path.insert(0, REF)
    try:
        import manimlib  # noqa: F401  (full import: config, GL, Pango)
    except ImportError as e:
        fail("reference-import-closure", f"missing Reference dependency: {e}")
    except Exception as e:  # GL context creation can raise non-ImportErrors
        fail("opengl-context", f"Reference import failed (GL stack?): {e}")
    return sys.modules["manimlib"]


def build_scenes(m):
    """The calibration scenes, defined against the Reference API.

    Each entry: (capture_id, callable(scene) -> None) adding static content;
    the harness captures a single still per scene.
    """

    def gradient_fills(s):
        sq = m.Square(side_length=3.0)
        sq.set_fill(color=[m.BLUE_E, m.YELLOW], opacity=0.8)
        sq.set_stroke(color=[m.RED, m.GREEN], width=6.0)
        ci = m.Circle(radius=1.5)
        ci.set_fill(color=[m.PURPLE, m.TEAL], opacity=0.5)
        ci.next_to(sq, m.RIGHT, buff=0.5)
        s.add(m.VGroup(sq, ci).center())

    def self_intersections(s):
        import numpy as np

        # A five-point star drawn edge-to-edge (self-intersecting outline),
        # filled — the nonzero-winding stress case.
        angles = [np.pi / 2 + k * 4 * np.pi / 5 for k in range(5)]
        pts = [3.0 * np.array([np.cos(a), np.sin(a), 0.0]) for a in angles]
        star = m.Polygon(*pts)
        star.set_fill(m.BLUE_D, opacity=0.7)
        star.set_stroke(m.WHITE, width=4.0)
        s.add(star.center())

    def joints_and_caps(s):
        import numpy as np

        rows = []
        for jt in ["auto", "bevel", "miter", "no_joint"]:
            zig = m.VMobject()
            zig.set_points_as_corners(
                [
                    np.array([-3.0, 0.0, 0.0]),
                    np.array([-1.0, 1.2, 0.0]),
                    np.array([1.0, -1.2, 0.0]),
                    np.array([3.0, 0.0, 0.0]),
                ]
            )
            zig.set_stroke(m.YELLOW, width=20.0)
            zig.set_joint_type(jt)
            label = m.Text(jt, font_size=24)
            label.next_to(zig, m.LEFT, buff=0.3)
            rows.append(m.VGroup(zig, label))
        s.add(m.VGroup(*rows).arrange(m.DOWN, buff=0.6).center())

    def glow(s):
        dots = m.VGroup()  # GlowDots are not VMobjects; use Group if needed
        try:
            g1 = m.GlowDot(m.LEFT * 2, radius=1.0, color=m.BLUE)
            g2 = m.GlowDot(m.ORIGIN, radius=1.5, color=m.YELLOW)
            g3 = m.GlowDot(m.RIGHT * 2, radius=0.75, color=m.RED)
            s.add(g1, g2, g3)
        except Exception:
            s.add(dots)
            raise

    def lighting_3d(s):
        sphere = m.Sphere(radius=2.0)
        sphere.set_color(m.BLUE_E)
        s.frame.reorient(20, 70)
        s.add(sphere)

    def text_sample(s):
        title = m.Text("FrankenManim look study", font_size=60)
        body = m.Text(
            "the same feel, cleaner — measured, then kept",
            font_size=32,
            slant=m.ITALIC,
        )
        body.next_to(title, m.DOWN, buff=0.5)
        s.add(m.VGroup(title, body).center())

    return [
        ("gradient_fills", gradient_fills),
        ("self_intersections", self_intersections),
        ("joints_and_caps", joints_and_caps),
        ("glow", glow),
        ("lighting_3d", lighting_3d),
        ("text_sample", text_sample),
    ]


def gl_identity():
    """Vendor/renderer/version strings from a standalone moderngl context."""
    try:
        import moderngl

        ctx = moderngl.create_standalone_context()
        info = {
            "vendor": ctx.info.get("GL_VENDOR", "unknown"),
            "renderer": ctx.info.get("GL_RENDERER", "unknown"),
            "version": ctx.info.get("GL_VERSION", "unknown"),
        }
        ctx.release()
        return info
    except Exception as e:
        fail("opengl-context", f"cannot create a standalone GL context: {e}")


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=DEFAULT_OUT)
    args = ap.parse_args()

    m = import_reference()
    gl = gl_identity()
    scenes = build_scenes(m)
    ids = [sid for sid, _ in scenes]
    if ids != CAPTURE_IDS:
        fail("inventory-drift", f"scene ids {ids} != inventory {CAPTURE_IDS}")

    os.makedirs(args.out, exist_ok=True)
    captures = {}
    for sid, populate in scenes:
        scene = m.Scene()
        populate(scene)
        image = scene.camera.get_image()
        path = os.path.join(args.out, f"{sid}.png")
        image.save(path)
        with open(path, "rb") as f:
            captures[sid] = hashlib.sha256(f.read()).hexdigest()
        print(f"  captured {sid}.png  {captures[sid][:12]}…")

    def pkg_version(name):
        try:
            import importlib.metadata as md

            return md.version(name)
        except Exception:
            return "absent"

    provenance = {
        "manifest": "fmn-reference-capture v1",
        "reference": {"repo": "3b1b/manim", "commit": ref_commit()},
        "environment": {
            "python": sys.version,
            "platform": platform.platform(),
            "gl": gl,
            "packages": {
                p: pkg_version(p)
                for p in [
                    "numpy",
                    "scipy",
                    "moderngl",
                    "PyOpenGL",
                    "pillow",
                    "manimpango",
                    "fonttools",
                ]
            },
        },
        "policy": (
            "§15.3 fixture policy: private gallery fixtures; captures are "
            "kept forever; the environment above is recorded, not maintained"
        ),
        "captures": captures,
    }
    manifest_path = os.path.join(args.out, "PROVENANCE.json")
    with open(manifest_path, "w") as f:
        json.dump(provenance, f, indent=2, sort_keys=True)
        f.write("\n")
    print(f"wrote {manifest_path}")
    print(
        "Update docs/look_gallery/CAPTURE_INVENTORY.md statuses to 'captured' "
        "and record the capture machine there."
    )


if __name__ == "__main__":
    main()
