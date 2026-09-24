# 3b1b corpus scene sweep (fmn-python)

Every `Scene` subclass that a static AST pass finds in `scripts/videos_ref/_2020`–`_2026` (3b1b/videos@e317d6c5) is rendered to one final PNG at 320×180. `ok` means the scene ran to completion and the portal wrote that PNG (all 936 are present). The sweep does not judge the image; that is the Look Gallery's job. Each scene runs through `fmn-python` in its own process, with a 180 s timeout and 16 scenes in parallel. The portal is a release build of commit `6b8315b6`, run on CPython 3.13.1 with NumPy 2.5.2. Later portal fixes are not reflected here: AnimatedStreamLines over plain line groups (4cd950b4), and the Reference's isolate labelling rules (86f2896d; re-running the 53 affected scenes, 12 now render).

Third-party packages that scenes import themselves are installed beside the portal, as they would be for the Reference: scipy 1.18.1, matplotlib 3.11.2, sympy 1.14.0, mpmath 1.4.1, pandas 3.0.6, Pillow 12.3.0 and colour 0.1.5. The portal neither vendors nor shims them; where the Reference re-exports one (colour.Color, PIL.Image), the portal forwards the installed package, except matplotlib.pyplot, which it never imports. gensim, transformers, torch, requests, mido and opencv were deliberately not installed; scenes importing them report `missing_dependency`. So do the scenes that import `_2022.newton_fractal`, `_2025.blocks_and_grover` or `_2025.hairy_ball`, because those modules are absent from the pinned corpus tree itself.

Scenes run under the corpus's own `custom_config.yml`, with `directories.base` pointed at an empty local folder. The corpus's image, SVG and texture assets are not public, so scenes that need them report `missing_asset`. `capability_refusal` includes 42 scenes that request fonts FrankenManim does not bundle (mostly Consolas); the refusal names the family, and a user can load the TTF by path.

Failures that are not otherwise classified are re-run in-process, and `raised_in` records whether the exception surfaced in portal or scene code. That is where the exception surfaced, not proof of fault: several `scene_code_error` scenes are written for pre-2022 manim (`CONFIG` dictionaries, removed keywords), which the pinned Reference also rejects.

`timeout` depends on host load. This sweep ran on a shared 64-core host whose load average stayed between about 40 and 60 during the run. The 17 `clt/dice_sims.py` timeouts are about 3,000-iteration scenes that need roughly 7 minutes at this build (tracked as fm-5wq.21).

Regenerate with `scripts/corpus_scene_sweep.py` (see its docstring). The records carry scene identifiers and outcome codes only, because the corpus is CC BY-NC-SA.

Earlier measurements over the same 2,814 scenes, all on 2026-09-24:
- 508 ok (18.1%) at the start of the day: bare interpreter, corpus config;
- 760 ok (27.0%) after `manimlib.utils.directories` was bound, with the dependencies above except `colour`;
- 890 ok (31.6%) at `95dfac40`, before the Fade `path_arc`, leaked-import and TexText-alignment fixes;
- 924 ok (32.8%) at `543b999a`, before `Scene.time` became the Reference's float attribute (6b8315b6).

2814 scene classes; **936 ok (33.3%)**. Module-weighted: 11 of 136 modules render every scene (8.1%).

| outcome | scenes | share |
|---|---:|---:|
| ok | 936 | 33.3% |
| capability_refusal | 76 | 2.7% |
| portal_unbound | 4 | 0.1% |
| tex_unsupported | 73 | 2.6% |
| missing_dependency | 428 | 15.2% |
| missing_asset | 684 | 24.3% |
| scene_code_error | 533 | 18.9% |
| portal_exception | 35 | 1.2% |
| runtime_error | 0 | 0.0% |
| timeout | 45 | 1.6% |
| crash | 0 | 0.0% |

| year | scenes | ok | ok % |
|---|---:|---:|---:|
| _2020 | 461 | 75 | 16.3% |
| _2021 | 395 | 99 | 25.1% |
| _2022 | 350 | 118 | 33.7% |
| _2023 | 481 | 230 | 47.8% |
| _2024 | 461 | 104 | 22.6% |
| _2025 | 505 | 225 | 44.6% |
| _2026 | 161 | 85 | 52.8% |

## portal_exception: top clusters

| scenes | cluster |
|---:|---|
| 7 | ValueError: surface construction exceeds its 65536-point aggregate UV-grid budget |
| 5 | TypeError: '…' not supported between instances of 'function' and 'int' |
| 4 | TypeError: TransformMatchingTex expects two StringMobject instances |
| 2 | StaleHandleError: mobject is detached; add it to a Scene before using a Scene-only operation |
| 2 | TypeError: AnimatedStreamLines requires a StreamLines instance |
| 2 | TypeError: Scene.play unexpected keyword(s): path_arc |
| 2 | TypeError: only 0-dimensional arrays can be converted to Python scalars |
| 1 | RuntimeError: a composition needs at least one animation |
| 1 | RuntimeError: become between families of different shapes (family alignment lands with fm-cye) |
| 1 | RuntimeError: scene animation failed: stage refused the operation: geometry operation refused during alignment: curve operation requests 106 |

## portal_unbound: top clusters

| scenes | cluster |
|---:|---|
| 3 | NotImplementedError: manimlib.mobject.types.vectorized_mobject.VMobject.use_winding_fill is present in the parity surface but its semantic b |
| 1 | NotImplementedError: manimlib.utils.debug.index_labels is present in the parity surface but its semantic binding has not landed |

## scene_code_error: top clusters

| scenes | cluster |
|---:|---|
| 70 | TypeError: VectorField.__init__() missing 1 required positional argument: 'coordinate_system' |
| 36 | NameError: name 'MovingCameraScene' is not defined |
| 31 | TypeError: got an unexpected keyword argument 'gloss' |
| 23 | TypeError: only 0-dimensional arrays can be converted to Python scalars |
| 16 | IndexError: list index out of range |
| 14 | Exception: Don't actually run this class. |
| 13 | AttributeError: 'NewtonFractal' object has no attribute 'julia_highlight' |
| 13 | AttributeError: 'NumberLine' object has no attribute 'unit_size' |
| 11 | AttributeError: 'Chessboard' object has no attribute 'shape' |
| 11 | TypeError: Axes() got unexpected keyword arguments: x_max, x_min, y_max, y_min |

## capability_refusal: top clusters

| scenes | cluster |
|---:|---|
| 34 | ValueError: font family 'Consolas' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to  |
| 16 | NotImplementedError: manimlib.camera.camera.moderngl (moderngl) is excluded (OOT-MODERNGL-SHADER-SURFACE): FrankenManim renders through Lume |
| 8 | NotImplementedError: Chessboard() keyword(s) not yet routed to the native builder: shape |
| 5 | ValueError: font family '…' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to add one |
| 2 | NotImplementedError: Chessboard() keyword(s) not yet routed to the native builder: height, shape |
| 2 | ValueError: font family 'Kalam' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to add |
| 1 | CapabilityError: ImageMobject URL fetching is unavailable; provide a local file or supply bytes through the host AssetFetcher boundary |
| 1 | NotImplementedError: Bubble.get_body requires bundled SVG 'Bubbles_thought.svg'; SpeechBubble supplies a native body |
| 1 | NotImplementedError: Chessboard() keyword(s) not yet routed to the native builder: shape, square_resolution, top_square_resolution |
| 1 | NotImplementedError: FadeIn() keyword(s) not yet routed to the native builder: suspend_updating |

## tex_unsupported: top clusters

| scenes | cluster |
|---:|---|
| 35 | TexError: isolate '…' is not in the native span map of '…' |
| 12 | TexError: isolate '…' is not in the native span map of "…" |
| 8 | TexError: character '𝔼' (U+1D53C) has no glyph in the bundled math faces  |
| 3 | TexError: Matching key '…' splits a native source-span part |
| 3 | TexError: `\ding{55}` is not supported; untiered (not observed in the G0-4 corpus), report at https:/…  |
| 2 | TexError: character '𝒪' (U+1D4AA) has no glyph in the bundled math faces  |
| 1 | TexError: Matching key 'm_1' splits a native source-span part |
| 1 | TexError: `\pii` is not supported; untiered (not observed in the G0-4 corpus), report at https:/…  |
| 1 | TexError: `\square` is not supported; untiered (not observed in the G0-4 corpus), report at https:/…  |
| 1 | TexError: character '𝒩' (U+1D4A9) has no glyph in the bundled math faces  |

## missing_dependency: top clusters

| scenes | cluster |
|---:|---|
| 141 | gensim |
| 100 | _2025.blocks_and_grover |
| 71 | transformers |
| 52 | _2022.newton_fractal |
| 32 | torch |
| 17 | requests |
| 12 | mido |
| 2 | _2025.hairy_ball |
| 1 | cv2 |

## missing_asset: top clusters

| scenes | cluster |
|---:|---|
| 537 | OSError: SVGMobject cannot read "…": No such file or directory (os error 2) |
| 20 | OSError: … not Found |
| 17 | OSError: EarthTextureMap not Found |
| 11 | OSError: SunTexture not Found |
| 7 | FileNotFoundError: [Errno 2] No such file or directory: '…' |
| 4 | OSError: PixelArtCat not Found |
| 3 | OSError: MarioSmall not Found |
| 3 | OSError: Newton not Found |
| 3 | OSError: Richard_Hamming not Found |
| 2 | FileNotFoundError: … Dropbox/3Blue1Brown/videos/2025/cosmic_distance/Data/galactic_data.csv not found. |
