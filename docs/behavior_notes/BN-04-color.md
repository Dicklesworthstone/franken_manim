# BN-04 — Color: correct compositing, familiar gradients

**Status:** Draft (W1, fm-0dg). Finalized when Lumen's compositor lands (W5).

## What changed

Classic manim has no single color model. Colors pass through the `colour`
library (which stores HSL internally and wobbles rgb values on round-trip),
composite in whatever space the GPU pipeline happens to be in, and encode
however the driver and ffmpeg agree to. Two installs can and do disagree.

FrankenManim has exactly one pipeline (§6.3):

1. **Decode:** sRGB-encoded user colors decode to linear light
   (IEC 61966-2-1) at the render boundary.
2. **Composite:** all blending is Porter–Duff source-over on premultiplied
   linear-light RGBA.
3. **Encode:** the output transfer function is applied once, at the frame
   boundary, per the negotiated output format.

## What deliberately did NOT change

Manim's gradient *aesthetic* is part of the look, and the look is a product
requirement. Two Reference formulas are therefore kept bit-for-bit, applied
to sRGB-encoded components exactly as `manimlib/utils/color.py` applies
them:

- `interpolate_color(c1, c2, α) = sqrt(lerp(c1², c2², α))` per channel —
  the root-mean-square blend that keeps manim's gradients bright in the
  middle instead of muddy.
- `average_color(...)` — the RMS mean, same property.

These operate on *user-space colors* (styles, gradients, colormaps), before
the decode step. Compositing never uses them.

## Migration guidance

- Scenes that read back composited pixel values will see different (more
  physically correct) results than classic manim, most visibly where
  translucent mobjects overlap: linear-light blending does not darken
  midtones the way gamma-space blending does.
- Gradients, `set_color_by_gradient`, colormaps, and `average_color` match
  classic manim to floating-point tolerance; no visual change.
- An **Oklab interpolation option** (`interpolate_color_oklab`) exists for
  users who want perceptually uniform ramps. It is opt-in, never a silent
  replacement; the default remains the Reference formula.

## Evidence

- `crates/fmn-core/tests/parity.rs::color_operations_match_the_reference` —
  418 fixture rows generated from the pinned Reference
  (`3b1b/manim @ 6199a00d4c1b1127ebe45cb629c3f22538b10e13`).
- `crates/fmn-core/tests/color_oracles.rs` — decode/encode identity on all
  8-bit code points, premultiply round-trip, source-over algebra
  (opaque-replaces, transparent-identity, associativity).
