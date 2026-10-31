<!-- fmn-measurement {"inputs_digest": "sha256:c758365cab1ad92e over 2814 scene ids", "max_age_commits": 200, "measured_at": "2026-10-31", "measured_commit": "7fba660635e931da6e7de2b08ea97f63d3141a85", "producer": "python3 scripts/corpus_scene_sweep.py --report /data/tmp/fmn-execute-20261031-whole-census/records.ndjson --dashboard docs/ratchet/scene_dashboard.md --tsv docs/ratchet/scene_outcomes.tsv --title 3b1b corpus scene sweep (fmn-python) --note /data/tmp/fmn-execute-20261031-scene-note.md --measured-commit 7fba660635e931da6e7de2b08ea97f63d3141a85"} -->
# 3b1b corpus scene sweep (fmn-python)

Measured with the release wheel built from `7fba660635e931da6e7de2b08ea97f63d3141a85`, wheel SHA-256 `ffbf9c22f3128a0b037484b59018ea883eaf7f55ea3e81c4a089cebf744b4f9b`. Its 4,405 tracked non-tracker source entries matched both build workers (aggregate `e0c05295120aa997e4f82399a0855b941cd2918800c52829be04666f92698693`). This is repository byte binding, not a full RCH source-closure receipt. Every scene record carries installed payload digest `208605130e6ba56e00a0c03ce24f5f12db06be29529662e0be7f5db24f7efb1f`. Later uncommitted oracle, source-path and optional-shell changes are excluded from this measurement.

All 2,814 original scene ids from pinned `3b1b/videos@e317d6c5eaa8370a2deb4d148c246b0d0e9fbe6f` were freshly attempted: final PNG at 320x180, 180 s timeout, 16 concurrent scenes. All 522 runtime errors received in-process attribution, and all 1,741 failed renders received construct-only checks. This is the complete census, not failed-only retries; every outcome remains in the denominator. Raw records: `/data/tmp/fmn-execute-20261031-whole-census/records.ndjson`, SHA-256 `208f84e50af781b3d5bd203c4c1869e3f39a4c1401fbb777019eb14b85ffeffc`. Original run log: `/data/tmp/fmn-execute-20261031-whole-census-writable.log`.

The host uses CPython 3.13.1, NumPy 2.5.2 and IPython 9.14.0 with the same frozen 35-package optional scene environment as the earlier sweep (requirements SHA-256 `c249191900f77fc76347e9edee19fb3e2d7a81c0357d4d0fa4e71e05f941cd03`). Sympy 1.14.0 remains installed without dependency resolution alongside mpmath 1.4.1 despite its declared upper bound. Gensim, transformers, torch, requests, mido, opencv and private corpus assets remain absent. The corpus configuration changes only `directories.base` to empty (SHA-256 `9d79278259343e35d49d6b3d636a5a2d1d40db0af2aed8c5300b9a0153a15ca3`). Missing dependencies and assets remain visible outcomes.

This whole run has 1,073 successful scenes, meeting the unchanged 1,073 ratchet. Against the preceding `6ab48696` whole run's 1,068, exactly five outcomes change, all timeout -> ok: `SimulationWithExpDistribution2Dice`, `SimulationWithExpDistribution5Dice`, `DirectMGFInterpretation`, `PrimeRace` and `SphereStrips`. Every other outcome is unchanged. This is an observation across source and host conditions, not a claim that code caused those five gains. The earlier 1,068 result, its 33 timeouts and its four gains/nine losses against `ea5d83f7` remain recorded in Git and the retained raw archive. Current timeouts are 28; `SpinMeasurements` still refuses a nonplanar depth-tested fill. No floor, timeout, exclusion or denominator was changed.

All 1,741 failed renders received construct-only probes: 691 `missing_asset`, 522 `runtime_error`, 428 `missing_dependency`, 68 `capability_refusal`, 28 `timeout`, and four `tex_unsupported`. These are separate diagnostic outcomes, not 522 newly established portal defects. Render attribution remains 34 `portal_exception` and 488 `scene_code_error`; exception location alone does not establish fault. Peak RSS median is 157.6 MiB and maximum 2,705.7 MiB across all 2,814 attempts, including failures.

This shared 64-logical-CPU host is **calibration-unqualified (ADR-0024)**. No qualified PG speed win, cross-platform certification or human Look Gallery verdict is established. Differential and full-frame/offload measurements remain separately bound to their earlier `6ab48696` artifact and denominators; this census does not refresh them.

2814 scene classes; **1073 ok (38.1%)**. Module-weighted: 14 of 136 modules render every scene (10.3%).

| outcome | scenes | share |
|---|---:|---:|
| ok | 1073 | 38.1% |
| capability_refusal | 68 | 2.4% |
| portal_unbound | 0 | 0.0% |
| tex_unsupported | 4 | 0.1% |
| missing_dependency | 428 | 15.2% |
| missing_asset | 691 | 24.6% |
| scene_code_error | 488 | 17.3% |
| portal_exception | 34 | 1.2% |
| runtime_error | 0 | 0.0% |
| timeout | 28 | 1.0% |
| crash | 0 | 0.0% |

| year | scenes | ok | ok % |
|---|---:|---:|---:|
| _2020 | 461 | 78 | 16.9% |
| _2021 | 395 | 136 | 34.4% |
| _2022 | 350 | 131 | 37.4% |
| _2023 | 481 | 255 | 53.0% |
| _2024 | 461 | 119 | 25.8% |
| _2025 | 505 | 262 | 51.9% |
| _2026 | 161 | 92 | 57.1% |

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
| 2 | NotImplementedError: Lightbulb needs the SVG asset 'lightbulb', which FrankenManim does not bundle: the Reference's drawings assets are CC B |
| 2 | ValueError: font family 'Kalam' is not available; bundled families: Computer Modern, CM Typewriter, IBM Plex Sans (load a TTF by path to add |
| 1 | CapabilityError: find_file cannot download '…'; network assets enter through the host-provided AssetFetcher |
| 1 | NotImplementedError: FadeIn() keyword(s) not yet routed to the native builder: suspend_updating |
| 1 | NotImplementedError: FadeIn() keyword(s) not yet routed to the native builder: time_range |
| 1 | NotImplementedError: NumberLine.add_numbers() keyword(s) not yet routed to the native builder: height |
| 1 | NotImplementedError: OldThoughtBubble.get_body needs the SVG asset 'Bubbles_thought.svg', which FrankenManim does not bundle: the Reference' |

## tex_unsupported: top clusters

| scenes | cluster |
|---:|---|
| 3 | TexError: `\ding{55}` is not supported; untiered (not observed in the G0-4 corpus), report at https:/…  |
| 1 | TexError: `\pii` is not supported; untiered (not observed in the G0-4 corpus), report at https:/…  |

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
| 484 | OSError: … not Found |
| 17 | OSError: EarthTextureMap not Found |
| 11 | OSError: SunTexture not Found |
| 9 | OSError: woman_icon not Found |
| 7 | FileNotFoundError: [Errno 2] No such file or directory: '…' |
| 7 | OSError: videos/2026/print_gallery/textures/PrintGalleryLog.png not Found |
| 5 | OSError: monster not Found |
| 4 | OSError: PixelArtCat not Found |
| 4 | OSError: example_loop1 not Found |
| 4 | OSError: person not Found |
