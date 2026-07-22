# Look Gallery — Reference Capture Inventory

> The committed record of the one-time Reference imagery capture (fm-xb3,
> §16.3, D-16). The captures themselves live in `gallery/reference_captures/`
> (gitignored — private fixtures per the §15.3 policy) together with their
> `PROVENANCE.json`; this file is what the repository remembers about them.

## Doctrine

- **Capture once, keep forever.** The Reference render environment (GL stack,
  Pango, LaTeX-adjacent fonts) is *recorded* in the provenance manifest at
  capture time and never maintained afterward. There is no certified
  Pango/llvmpipe environment in CI, ever.
- **Imagery, never a pixel warden.** These captures feed the human-judged
  Look Gallery (verdicts: at-least-as-good / different-but-fine, Behavior-
  Noted / regression) and G0-2's calibration study (fm-k77). No bit- or
  pixel-comparison gate consumes them.
- **Fixture policy (§15.3).** Gallery fixtures are private (not committed,
  not redistributed); the public corpus is our own permissively-licensed
  primitive scenes. The calibration scenes below are our own definitions,
  rendered *by* the Reference engine for aesthetic comparison; per-scene
  attribution applies to any future capture derived from 3b1b video scenes
  (CC BY-NC-SA), which this set deliberately avoids.

## The calibration set (§20.1 spike 2)

Scene definitions live in `scripts/capture_reference_imagery.py` (ids are
kept in lockstep by the script's inventory check). One still per scene.

| id | exercises | status |
|---|---|---|
| `gradient_fills` | fill + stroke color gradients, opacity compositing | **pending capture** |
| `self_intersections` | nonzero-winding fill of a self-intersecting star | **pending capture** |
| `joints_and_caps` | every joint type (auto/bevel/miter/no_joint) on wide zig-zag strokes, with caps | **pending capture** |
| `glow` | GlowDot falloff at three radii/colors | **pending capture** |
| `lighting_3d` | 3D sphere under the Reference lighting model, oblique camera | **pending capture** |
| `text_sample` | Pango text rendering (regular + italic) for the native-text comparison | **pending capture** |

## Capture record

| field | value |
|---|---|
| Reference pin | `3b1b/manim @ 6199a00d4c1b1127ebe45cb629c3f22538b10e13` |
| capture machine | *unrecorded — capture has not run yet* |
| GL identity | *see `gallery/reference_captures/PROVENANCE.json` after capture* |
| capture date | — |

**Why pending:** the capture requires the Reference's full GL import closure
(moderngl + a real OpenGL context + manimpango), which the current
development environment does not provide. The harness is complete and
validated for import errors; executing it on a GL-capable machine is tracked
as its own bead (blocking fm-k77, the G0-2 look study). After running it,
update the two tables above and commit — the PNG bytes stay out of git.
