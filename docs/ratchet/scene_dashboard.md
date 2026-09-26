# 3b1b corpus scene sweep (fmn-python)

Every `Scene` subclass that a static AST pass finds in `scripts/videos_ref/_2020`–`_2026` (3b1b/videos@e317d6c5) is rendered to one final PNG at 320×180. `ok` means the scene ran to completion and the portal wrote that PNG; all 1,039 are present, and no other frames exist. The sweep does not judge the image; that is the Look Gallery's job. Each scene runs through `fmn-python` in its own process, with a 180 s timeout and 16 scenes in parallel. The portal is a release build of commit `328b52f0`, run on CPython 3.13.1 with NumPy 2.5.2. Every record carries the same `portal_digest`, `12f79ad8…`: a sha256 over the imported `manimlib` and `fmn_python` trees.

Each record also carries its era and the scene process's own peak RSS from `os.wait4`: median 132 MiB, 95th percentile 228 MiB, maximum 2.4 GiB. Every scene that did not render was re-run with `--construct-only`, which runs its whole lifecycle without rasterizing. `construct_outcome` records the result. Of the timeouts, 7 construct within the limit, so their time goes to rendering.

Third-party packages that scenes import themselves are installed beside the portal, as they would be for the Reference: scipy 1.18.1, matplotlib 3.11.2, sympy 1.14.0, mpmath 1.4.1, pandas 3.0.6, Pillow 12.3.0 and colour 0.1.5. The portal neither vendors nor shims them. Where the Reference re-exports one (colour.Color, PIL.Image), the portal forwards the installed package, except matplotlib.pyplot, which it never imports. gensim, transformers, torch, requests, mido and opencv were deliberately not installed; scenes importing them report `missing_dependency`. So do the scenes that import `_2022.newton_fractal`, `_2025.blocks_and_grover` or `_2025.hairy_ball`, because those modules are absent from the pinned corpus tree itself.

Scenes run under the corpus's own `custom_config.yml`, with `directories.base` pointed at an empty local folder. The corpus's image, SVG and texture assets are not public, so scenes that need them report `missing_asset`. `capability_refusal` includes 42 scenes that request fonts FrankenManim does not bundle (mostly Consolas); the refusal names the family, and a user can load the TTF by path.

Failures that are not otherwise classified are re-run in-process, and `raised_in` records whether the exception surfaced in portal or scene code. That is where the exception surfaced, not proof of fault. Many `scene_code_error` scenes are written for pre-2022 manim, which the pinned Reference also rejects:
- `CONFIG` dictionaries;
- removed keywords such as `x_min`, `gloss` in `set_style`, and `element_to_mobject_config`;
- `LaggedStartMap`'s old third positional argument;
- Mobject methods passed to `play`.

`timeout` depends on host load. This sweep ran on a shared 64-core host with other work beside it; the load average ranged from about 45 to 85. The 17 `clt/dice_sims.py` timeouts are about 3,000-sample scenes that stay over the 180 s budget on the Reference as well: 97 s for 1,200 samples, measured in the same session, growing superlinearly (fm-5wq.21, closed; the portal's per-sample growth is fm-92gu).

Regenerate with `scripts/corpus_scene_sweep.py` (see its docstring). The records carry scene identifiers and outcome codes only, because the corpus is CC BY-NC-SA.

Earlier measurements over the same 2,814 scenes:
- 508 ok (18.1%) on 2026-09-24 at the start of the day: bare interpreter, corpus config;
- 760 ok (27.0%) after `manimlib.utils.directories` was bound, with the dependencies above except `colour`;
- 890 ok (31.6%) at `95dfac40`, before the Fade `path_arc`, leaked-import and TexText-alignment fixes;
- 924 ok (32.8%) at `543b999a`, before `Scene.time` became the Reference's float attribute;
- 936 ok (33.3%) at `6b8315b6`, before the command-keyword ink, OldTex part-selection, Reference isolate-labelling and point-cloud ShowCreation fixes;
- 981 ok (34.9%) at `04599cc2`;
- 990 ok (35.2%) at `67c87a03`, after the franken_markdown repin (𝔼, 𝒪 and 𝒩 math alphanumerics).

Against `67c87a03` this sweep gains 49 scenes and loses none. The gains come from:
- vector-valued `ValueTracker` (all 18 `colliding_blocks_v2` scenes and clt `RuleOfThumb`);
- the binned 3D stroke search (hairy_ball, convolutions2 and borwein render timeouts);
- `become` on detached groups with Scene-bound members;
- `NumberLine(stroke_color=...)`;
- vectorized `ComplexPlane.n2p` (zeta);
- `GrowArrow` on arced arrows;
- zero-length curved arrows (monster);
- the `use_winding_fill`/`index_labels` bindings.

2814 scene classes; **1039 ok (36.9%)**. Module-weighted: 13 of 136 modules render every scene (9.6%).

| outcome | scenes | share |
|---|---:|---:|
| ok | 1039 | 36.9% |
| capability_refusal | 80 | 2.8% |
| portal_unbound | 0 | 0.0% |
| tex_unsupported | 5 | 0.2% |
| missing_dependency | 428 | 15.2% |
| missing_asset | 696 | 24.7% |
| scene_code_error | 475 | 16.9% |
| portal_exception | 37 | 1.3% |
| runtime_error | 0 | 0.0% |
| timeout | 54 | 1.9% |
| crash | 0 | 0.0% |

| year | scenes | ok | ok % |
|---|---:|---:|---:|
| _2020 | 461 | 79 | 17.1% |
| _2021 | 395 | 133 | 33.7% |
| _2022 | 350 | 126 | 36.0% |
| _2023 | 481 | 244 | 50.7% |
| _2024 | 461 | 113 | 24.5% |
| _2025 | 505 | 255 | 50.5% |
| _2026 | 161 | 89 | 55.3% |

## portal_exception: top clusters

| scenes | cluster |
|---:|---|
| 8 | ValueError: surface construction exceeds its 65536-point aggregate UV-grid budget |
| 5 | TypeError: '…' not supported between instances of 'function' and 'int' |
| 5 | TypeError: TransformMatchingTex expects two StringMobject instances |
| 3 | TypeError: Scene.play unexpected keyword(s): path_arc |
| 2 | TypeError: '…' not supported between instances of 'float' and 'NoneType' |
| 2 | TypeError: only 0-dimensional arrays can be converted to Python scalars |
| 1 | RuntimeError: a composition needs at least one animation |
| 1 | RuntimeError: scene animation failed: stage refused the operation: geometry operation refused during alignment: curve operation requests 106 |
| 1 | TypeError: Scene.play unexpected keyword(s): remover |
| 1 | TypeError: missing a required argument: 'values' |

## scene_code_error: top clusters

| scenes | cluster |
|---:|---|
| 70 | TypeError: VectorField.__init__() missing 1 required positional argument: 'coordinate_system' |
| 36 | NameError: name 'MovingCameraScene' is not defined |
| 31 | TypeError: got an unexpected keyword argument 'gloss' |
| 19 | AttributeError: 'NewtonFractal' object has no attribute 'julia_highlight' |
| 14 | AttributeError: 'NumberLine' object has no attribute 'unit_size' |
| 14 | Exception: Don't actually run this class. |
| 11 | AttributeError: 'Chessboard' object has no attribute 'shape' |
| 11 | TypeError: TimeVaryingVectorField.__init__() missing 1 required positional argument: 'coordinate_system' |
| 9 | AttributeError: '…' object has no attribute 'simulation_config' |
| 9 | TypeError: NumberLine() got an unexpected keyword argument `tick_frequency` |

## capability_refusal: top clusters

| scenes | cluster |
|---:|---|
| 34 | ValueError: font family 'Consolas' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to  |
| 16 | NotImplementedError: manimlib.camera.camera.moderngl (moderngl) is excluded (OOT-MODERNGL-SHADER-SURFACE): FrankenManim renders through Lume |
| 8 | NotImplementedError: Chessboard() keyword(s) not yet routed to the native builder: shape |
| 5 | ValueError: font family '…' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to add one |
| 2 | NotImplementedError: Chessboard() keyword(s) not yet routed to the native builder: height, shape |
| 2 | NotImplementedError: Lightbulb requires bundled SVG 'lightbulb'; the drawings SVG shelf is fm-3kr |
| 2 | NotImplementedError: TwistedRibbon() keyword(s) not yet routed to the native builder: prefered_creation_axis |
| 2 | ValueError: font family 'Kalam' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to add |
| 1 | CapabilityError: ImageMobject URL fetching is unavailable; provide a local file or supply bytes through the host AssetFetcher boundary |
| 1 | NotImplementedError: Bubble.get_body requires bundled SVG 'Bubbles_thought.svg'; SpeechBubble supplies a native body |

## tex_unsupported: top clusters

| scenes | cluster |
|---:|---|
| 3 | TexError: `\ding{55}` is not supported; untiered (not observed in the G0-4 corpus), report at https:/…  |
| 1 | TexError: `\pii` is not supported; untiered (not observed in the G0-4 corpus), report at https:/…  |
| 1 | TexError: `\square` is not supported; untiered (not observed in the G0-4 corpus), report at https:/…  |

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
| 548 | OSError: SVGMobject cannot read "…": No such file or directory (os error 2) |
| 20 | OSError: … not Found |
| 17 | OSError: EarthTextureMap not Found |
| 11 | OSError: SunTexture not Found |
| 7 | FileNotFoundError: [Errno 2] No such file or directory: '…' |
| 4 | OSError: PixelArtCat not Found |
| 3 | OSError: MarioSmall not Found |
| 3 | OSError: Newton not Found |
| 3 | OSError: Richard_Hamming not Found |
| 2 | FileNotFoundError: … Dropbox/3Blue1Brown/videos/2025/cosmic_distance/Data/galactic_data.csv not found. |
