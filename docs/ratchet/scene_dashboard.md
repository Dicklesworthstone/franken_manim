<!-- fmn-measurement {"inputs_digest": "sha256:c758365cab1ad92e over 2814 scene ids", "max_age_commits": 200, "measured_at": "2026-11-02", "measured_commit": "fe90a340c8fd88ad78836ff1cd07c96cb508a658", "producer": "python3 scripts/corpus_scene_sweep.py --report /data/tmp/fmn-execute-20261102-corpus68-whole-census-v1/scenes/records.ndjson --dashboard docs/ratchet/scene_dashboard.md --tsv docs/ratchet/scene_outcomes.tsv --title 3b1b corpus scene sweep (fmn-python) --note /data/tmp/fmn-execute-20261102-corpus68-scene-note.md --measured-commit fe90a340c8fd88ad78836ff1cd07c96cb508a658"} -->
# 3b1b corpus scene sweep (fmn-python)

Measured on 2026-11-02 with the clean release wheel built from `fe90a340c8fd88ad78836ff1cd07c96cb508a658`, wheel SHA-256 `00efb89cc68378809b96480194ec8fac908d5183be0a8747a736885097eeba9e`, native extension `6a9a2f458b0e62bebbb3bc29b62b8c1eef98fc20b302ce0da6fa2bf5af2bfcfa`, and matching standalone CLI `44cf2f92053a7b7ac67068886228c302f0137254c34a478fca07fa419c250b09`. All 4,406 tracked non-tracker source entries matched before and after the build and census (aggregate `5fde4187a527dbc8394e1edeaf172b0b95ab202cb3967db914d899ceb025eafe`); all 145 installed portal payload entries also remained unchanged. This is exact byte binding, not a full RCH source-closure receipt. Every scene record carries installed payload digest `3bd5eb9dc7429451a952ede9b4e05328a3e3846038edfb60cf6e2f2916c441f6`.

All 2,814 original scene IDs from pinned `3b1b/videos@e317d6c5eaa8370a2deb4d148c246b0d0e9fbe6f` were freshly attempted: final PNG at 320x180, 180 s timeout, 16 concurrent scenes, years 2020–2026. Every one of the 521 runtime errors received in-process attribution, and all 1,740 failed renders received construct-only checks. Raw records: `/data/tmp/fmn-execute-20261102-corpus68-whole-census-v1/scenes/records.ndjson`, SHA-256 `067584f5fa982f8b92f8069514f83367c68cddc2ce89d7dfc4fb140120defed1`. Original producer log and guarded inputs are alongside that run. This is the complete census, with no exclusions or failed-only retries.

The host uses CPython 3.13.1, NumPy 2.5.2 and IPython 9.14.0 with 35 pinned scene packages (requirements SHA-256 `8c5ab6727f43d055a8547aa3bc36b47ce4d29e8427cc3870f41725c47c9d8f89`); `pip check` passes. **The host cohort changed:** the previous `c249191900f77fc76347e9edee19fb3e2d7a81c0357d4d0fa4e71e05f941cd03` environment retained a declared Sympy/mpmath incompatibility. This comparison changes both source and host dependencies, so outcome differences cannot be attributed solely to code. Gensim, transformers, torch, requests, mido, opencv and private corpus assets remain absent. The corpus configuration still changes only `directories.base` to empty (SHA-256 `9d79278259343e35d49d6b3d636a5a2d1d40db0af2aed8c5300b9a0153a15ca3`). Missing dependencies and assets remain visible outcomes.

This run has **1,074 successful scenes**, meeting the unchanged 1,073 ratchet. Against the previous complete `7fba6606` run, exactly one outcome changes: `_2025/laplace/integration.py:IntegrateRealExponential`, from `portal_exception` to `ok`. There are no successful-to-failed changes, and every other outcome is unchanged. The earlier 1,073 result and its raw records remain retained. Current timeouts are still 28. No floor, timeout, exclusion or denominator was changed.

All 1,740 failed renders received construct-only probes: 691 `missing_asset`, 521 `runtime_error`, 428 `missing_dependency`, 68 `capability_refusal`, 28 `timeout`, and four `tex_unsupported`. Render attribution is 33 `portal_exception` and 488 `scene_code_error`; exception location alone does not establish fault. Peak RSS median is 137.0 MiB and maximum 2,681.2 MiB across all 2,814 attempts, including failures. These observations do not establish a memory improvement across the changed cohorts.

This shared 64-logical-CPU host is **calibration-unqualified (ADR-0024)**. No qualified PG speed win, cross-platform certification or human Look Gallery verdict is established. Differential and full-frame/offload measurements remain separately bound to their earlier `6ab48696` artifact and denominators; this census does not refresh them. Whole source-policy, complete installed-policy and release qualification remain separate gates.

2814 scene classes; **1074 ok (38.2%)**. Module-weighted: 14 of 136 modules render every scene (10.3%).

| outcome | scenes | share |
|---|---:|---:|
| ok | 1074 | 38.2% |
| capability_refusal | 68 | 2.4% |
| portal_unbound | 0 | 0.0% |
| tex_unsupported | 4 | 0.1% |
| missing_dependency | 428 | 15.2% |
| missing_asset | 691 | 24.6% |
| scene_code_error | 488 | 17.3% |
| portal_exception | 33 | 1.2% |
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
| _2025 | 505 | 263 | 52.1% |
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
