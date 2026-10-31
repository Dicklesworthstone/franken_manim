<!-- fmn-measurement {"inputs_digest": "sha256:c758365cab1ad92e over 2814 scene ids", "max_age_commits": 200, "measured_at": "2026-10-30", "measured_commit": "6ab48696475625c68fe09b2322e1df45bfc103f0", "producer": "python3 scripts/corpus_scene_sweep.py --report /run/fmn-execute-20261030/release/sweep/records.ndjson --dashboard docs/ratchet/scene_dashboard.md --tsv docs/ratchet/scene_outcomes.tsv --title 3b1b corpus scene sweep (fmn-python) --note /data/tmp/fmn-execute-20261030-scene-note.md --measured-commit 6ab48696475625c68fe09b2322e1df45bfc103f0"} -->
# 3b1b corpus scene sweep (fmn-python)

Measured with the release wheel built from `6ab48696475625c68fe09b2322e1df45bfc103f0`, wheel SHA-256 `317fe607f8dd8ba63037147cc8648c29bc89a9b6e76ce1d9baefcc76dc6c8b4f`. The wheel build and local checkout have identical hashes for all 4,395 tracked non-tracker files (aggregate `cafaa76a5daa6d7e2efc50ea90c20150cbaaf6508f39e61fcc338d0fa5b0df2b`); this is repository byte binding, not a full RCH source-closure receipt. Every scene record carries native payload digest `c050344deb5b27216b8c7d6662538af8126ebf81df89812fa2c4037b15752ba9`.

All 2,814 original scene ids from pinned `3b1b/videos@e317d6c5eaa8370a2deb4d148c246b0d0e9fbe6f` were freshly attempted: final PNG at 320x180, 180 s timeout, 16 concurrent scenes. Attribution and construct-only checks ran for failures; every outcome remains in the denominator. This is a compatibility census on the shared 64-logical-CPU host, **calibration-unqualified (ADR-0024)**. Concurrent measurements and host load affect timeout and timing results. No qualified PG, cross-platform certification, or human Look Gallery verdict is established.

The host uses CPython 3.13.1, NumPy 2.5.2 and IPython 9.14.0, with the historical optional scene package set: scipy 1.18.1, matplotlib 3.11.2, sympy 1.14.0, mpmath 1.4.1, pandas 3.0.6, Pillow 12.3.0, colour 0.1.5 and tqdm 4.70.0. Sympy is installed without dependency resolution because it declares `mpmath<1.4`. Deliberately absent: gensim, transformers, torch, requests, mido and opencv. No private corpus image/SVG/texture assets are supplied. The corpus configuration has only `directories.base` emptied (config SHA-256 `9d79278259343e35d49d6b3d636a5a2d1d40db0af2aed8c5300b9a0153a15ca3`); missing packages and assets remain visible outcomes.

Compared with the previous published sweep at `ea5d83f7` (1,073 ok), this run has 1,068 ok: 4 previously unsuccessful scenes now succeed and 9 previous successes do not. This is an observational comparison across source and host load, not causal credit for the changes in this session. Success ids gained/lost are retained in the raw comparison beside the source-bound records.

Per-scene peak RSS is measured by `os.wait4`: median 158.2 MiB, 95th percentile 233.2 MiB, maximum 1,909.0 MiB. All 1,746 failed render attempts received construct-only probes: 691 `missing_asset`, 522 `runtime_error`, 428 `missing_dependency`, 68 `capability_refusal`, 32 `timeout`, 4 `tex_unsupported` and 1 `ok`. Construct outcomes are a separate diagnostic classification. Their 522 runtime errors do not establish 522 new portal faults. Render outcomes retain in-process attribution (34 `portal_exception` and 488 `scene_code_error`); where an exception surfaced does not establish fault.

2814 scene classes; **1068 ok (38.0%)**. Module-weighted: 13 of 136 modules render every scene (9.6%).

| outcome | scenes | share |
|---|---:|---:|
| ok | 1068 | 38.0% |
| capability_refusal | 68 | 2.4% |
| portal_unbound | 0 | 0.0% |
| tex_unsupported | 4 | 0.1% |
| missing_dependency | 428 | 15.2% |
| missing_asset | 691 | 24.6% |
| scene_code_error | 488 | 17.3% |
| portal_exception | 34 | 1.2% |
| runtime_error | 0 | 0.0% |
| timeout | 33 | 1.2% |
| crash | 0 | 0.0% |

| year | scenes | ok | ok % |
|---|---:|---:|---:|
| _2020 | 461 | 78 | 16.9% |
| _2021 | 395 | 136 | 34.4% |
| _2022 | 350 | 131 | 37.4% |
| _2023 | 481 | 251 | 52.2% |
| _2024 | 461 | 118 | 25.6% |
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
