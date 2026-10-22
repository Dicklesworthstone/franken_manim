<!-- fmn-measurement {"inputs_digest": "sha256:c758365cab1ad92e over 2814 scene ids", "max_age_commits": 200, "measured_at": "2026-10-22", "measured_commit": "ea5d83f78bfc", "producer": "python3 scripts/corpus_scene_sweep.py --report /data/tmp/claude-1000/-data-projects-franken-manim/78504355-0f8a-4ebd-9b46-b2bbc764b3b0/scratchpad/w11/dew9/out/records.ndjson --dashboard docs/ratchet/scene_dashboard.md --tsv docs/ratchet/scene_outcomes.tsv --title 3b1b corpus scene sweep (fmn-python) --note /data/tmp/claude-1000/-data-projects-franken-manim/78504355-0f8a-4ebd-9b46-b2bbc764b3b0/scratchpad/w11/dew9/note.md --measured-commit ea5d83f78bfc"} -->
# 3b1b corpus scene sweep (fmn-python)

Every `Scene` subclass that a static AST pass finds in `scripts/videos_ref/_2020`–`_2026` (3b1b/videos@e317d6c5) is rendered to one final PNG at 320×180.
- `ok` means the scene ran to completion and the portal wrote that PNG. The sweep does not judge the image; that is the Look Gallery's job.
- Each scene runs through `fmn-python` in its own process, with a 180 s timeout and 16 scenes in parallel.
- The portal is a release wheel of the tree at `ea5d83f7` (2026-10-22). Later commits up to the measurement touch only scripts, the README and the tracker.
- It ran on CPython 3.13.1 with NumPy 2.5.2. Every record carries the same `portal_digest`, `eecea106…`.

Each record also carries its era and the scene process's own peak RSS from `os.wait4`: median 156 MiB, 95th percentile 226 MiB, maximum 3 GiB. Every scene that did not render was re-run with `--construct-only`, which runs its whole lifecycle without rasterizing. `construct_outcome` records the result.

Third-party packages that scenes import themselves are installed beside the portal, as they would be for the Reference. They are the previous sweep's set plus tqdm:
- scipy 1.18.1, matplotlib 3.11.2, sympy 1.14.0, mpmath 1.4.1, pandas 3.0.6, Pillow 12.3.0, colour 0.1.5 and tqdm 4.70.0.
- sympy is installed without dependency resolution, because 1.14.0 declares `mpmath<1.4`.
- The corpus's own `manim_imports_ext` imports tqdm. Without it every scene reports `missing_dependency`, which is what a first run today did.

The portal neither vendors nor shims these packages. gensim, transformers, torch, requests, mido and opencv are deliberately not installed; scenes importing them report `missing_dependency`.

Scenes run under the corpus's own `custom_config.yml`, with `directories.base` pointed at an empty local folder. The corpus's image, SVG and texture assets are not public, so scenes that need them report `missing_asset`. `capability_refusal` includes scenes that request fonts FrankenManim does not bundle (mostly Consolas); the refusal names the family.

Failures not otherwise classified are re-run in-process, and `raised_in` records whether the exception surfaced in portal or scene code. That is where the exception surfaced, not proof of fault. Many `scene_code_error` scenes are written for pre-2022 manim, which the pinned Reference also rejects (`CONFIG` dictionaries, removed keywords such as `x_min` and `gloss`).

`timeout` depends on host load. This sweep ran on the shared 64-core host, with the load average at about 18–27 at the start and end of the 36-minute run.

Against the previous sweep (`15c65f1c`, 2026-09-26, 1,052 ok) this one gains 24 scenes and loses 3.
- **Gained:** 14 former timeouts, 6 former `scene_code_error` and 4 former `portal_exception` now run to completion.
- **Lost:** all three also fail in the pinned Reference (corpus differential, both engines).
  - `covid` ShowVaryingExpCurve passes the removed keyword `x_min`. The portal now raises the Reference's TypeError instead of ignoring it.
  - `ctracing` ContactTracingMisnomer and `newton_fractal` CyclicAttractor stop earlier in the Reference, on a missing SVG asset. The portal now refuses what the Reference also refuses: a non-VMobject in a `VGroup`, and the removed `gloss` keyword.

Regenerate with `scripts/corpus_scene_sweep.py` (see its docstring), passing `--measured-commit`. The first line of this file is the staleness header that `scripts/check_measurement_staleness.py` reads. The records carry scene identifiers and outcome codes only, because the corpus is CC BY-NC-SA.

Earlier measurements over the same 2,814 scenes:
- 508 ok (18.1%) on 2026-09-24 at the start of the day: bare interpreter, corpus config;
- 760 ok (27.0%) after `manimlib.utils.directories` was bound;
- 890 ok (31.6%) at `95dfac40`;
- 924 ok (32.8%) at `543b999a`;
- 936 ok (33.3%) at `6b8315b6`;
- 981 ok (34.9%) at `04599cc2`;
- 990 ok (35.2%) at `67c87a03`;
- 1,039 ok (36.9%) at `328b52f0`;
- 1,052 ok (37.4%) at `15c65f1c` on 2026-09-26.

2814 scene classes; **1073 ok (38.1%)**. Module-weighted: 14 of 136 modules render every scene (10.3%).

| outcome | scenes | share |
|---|---:|---:|
| ok | 1073 | 38.1% |
| capability_refusal | 68 | 2.4% |
| portal_unbound | 0 | 0.0% |
| tex_unsupported | 5 | 0.2% |
| missing_dependency | 428 | 15.2% |
| missing_asset | 690 | 24.5% |
| scene_code_error | 493 | 17.5% |
| portal_exception | 31 | 1.1% |
| runtime_error | 0 | 0.0% |
| timeout | 26 | 0.9% |
| crash | 0 | 0.0% |

| year | scenes | ok | ok % |
|---|---:|---:|---:|
| _2020 | 461 | 79 | 17.1% |
| _2021 | 395 | 137 | 34.7% |
| _2022 | 350 | 129 | 36.9% |
| _2023 | 481 | 256 | 53.2% |
| _2024 | 461 | 119 | 25.8% |
| _2025 | 505 | 262 | 51.9% |
| _2026 | 161 | 91 | 56.5% |

## portal_exception: top clusters

| scenes | cluster |
|---:|---|
| 5 | TypeError: '…' not supported between instances of 'function' and 'int' |
| 5 | TypeError: TransformMatchingTex expects two StringMobject instances |
| 4 | ValueError: surface uv_func must return finite f32-representable coordinates |
| 3 | TypeError: Scene.play unexpected keyword(s): path_arc |
| 2 | TypeError: only 0-dimensional arrays can be converted to Python scalars |
| 2 | ValueError: surface construction exceeds its 262144-point aggregate UV-grid budget |
| 1 | Exception: Invalid argument to Group of type <class 'NotImplementedType'> |
| 1 | Exception: Only VMobjects can be passed into VGroup |
| 1 | RuntimeError: scene animation failed: stage refused the operation: geometry operation refused during alignment: curve operation requests 716 |
| 1 | TypeError: Scene.play unexpected keyword(s): remover |

## scene_code_error: top clusters

| scenes | cluster |
|---:|---|
| 70 | TypeError: VectorField.__init__() missing 1 required positional argument: 'coordinate_system' |
| 36 | NameError: name 'MovingCameraScene' is not defined |
| 31 | TypeError: got an unexpected keyword argument 'gloss' |
| 16 | TypeError: Mobject.__init__() got an unexpected keyword argument 'scale_factor' |
| 15 | AttributeError: 'NumberLine' object has no attribute 'unit_size' |
| 14 | Exception: Don't actually run this class. |
| 13 | TypeError: Mobject.__init__() got an unexpected keyword argument 'x_min' |
| 11 | AttributeError: 'Chessboard' object has no attribute 'shape' |
| 11 | TypeError: Mobject.__init__() got an unexpected keyword argument 'shape' |
| 11 | TypeError: TimeVaryingVectorField.__init__() missing 1 required positional argument: 'coordinate_system' |

## capability_refusal: top clusters

| scenes | cluster |
|---:|---|
| 34 | ValueError: font family 'Consolas' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to  |
| 16 | NotImplementedError: manimlib.camera.camera.moderngl (moderngl) is excluded (OOT-MODERNGL-SHADER-SURFACE): FrankenManim renders through Lume |
| 5 | ValueError: font family '…' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to add one |
| 2 | NotImplementedError: Lightbulb requires bundled SVG 'lightbulb'; the drawings SVG shelf is fm-3kr |
| 2 | ValueError: font family 'Kalam' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to add |
| 1 | CapabilityError: ImageMobject URL fetching is unavailable; provide a local file or supply bytes through the host AssetFetcher boundary |
| 1 | NotImplementedError: Bubble.get_body requires bundled SVG 'Bubbles_thought.svg'; SpeechBubble supplies a native body |
| 1 | NotImplementedError: FadeIn() keyword(s) not yet routed to the native builder: suspend_updating |
| 1 | NotImplementedError: FadeIn() keyword(s) not yet routed to the native builder: time_range |
| 1 | NotImplementedError: NumberLine.add_numbers() keyword(s) not yet routed to the native builder: height |

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
| 542 | OSError: SVGMobject cannot read "…": No such file or directory (os error 2) |
| 20 | OSError: … not Found |
| 17 | OSError: EarthTextureMap not Found |
| 11 | OSError: SunTexture not Found |
| 7 | FileNotFoundError: [Errno 2] No such file or directory: '…' |
| 4 | OSError: PixelArtCat not Found |
| 3 | OSError: MarioSmall not Found |
| 3 | OSError: Newton not Found |
| 3 | OSError: Richard_Hamming not Found |
| 2 | FileNotFoundError: … Dropbox/3Blue1Brown/videos/2025/cosmic_distance/Data/galactic_data.csv not found. |
