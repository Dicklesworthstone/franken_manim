<!-- fmn-measurement {"inputs_digest": "sha256:c758365cab1ad92e over 2814 scene ids", "max_age_commits": 200, "measured_at": "2026-11-04", "measured_commit": "a215e9a2fe8e4f65cdbebc851bb14073f3b4be35", "producer": "python3 scripts/corpus_scene_sweep.py --report /data/tmp/fmn-execute-20261104-census-whole-local-v1/scenes/records.ndjson --dashboard docs/ratchet/scene_dashboard.md --tsv docs/ratchet/scene_outcomes.tsv --title 3b1b corpus scene sweep (fmn-python) --note /data/tmp/fmn-execute-20261104-corpus68-scene-note.md --measured-commit a215e9a2fe8e4f65cdbebc851bb14073f3b4be35"} -->
# 3b1b corpus scene sweep (fmn-python)

Measured on 2026-11-04 with the release-profile wheel built from `a215e9a2fe8e4f65cdbebc851bb14073f3b4be35`, wheel SHA-256 `6800cf6d5767ff49ea822534c15764e690f5087c4b6e9d5919dceb11580b7b2c`, native extension `2292fedd9814b3d7d8676b286d49ef8667d7926979e8bab358675e74a2cdbd6f`, and matching standalone CLI `ad363569a1f7279927f2861b25c99b4449654df8132536a51caf65b6c4158d70`. All 4,408 tracked non-tracker source entries matched before and after the build and census (aggregate `256c9d54e342279520ab95d77503a7a94617898bfbb4cd5c1ff981a96b035194`); all 145 installed portal payload entries also remained unchanged. This is exact byte binding, not a full RCH source-closure receipt. Every scene record carries installed payload digest `a2c5b47c4d99861b0ecc07698404df93835182d0330d811dc23f3cdf1ffa89c0`.

All 2,814 original scene IDs from pinned `3b1b/videos@e317d6c5eaa8370a2deb4d148c246b0d0e9fbe6f` were freshly attempted: final PNG at 320x180, 180 s timeout, 16 concurrent scenes, years 2020–2026. Every one of the 521 runtime errors received in-process attribution, and all 1,742 failed renders received construct-only checks. All 1,072 successful records map exactly to 1,072 physical PNG files; every file was fully decoded at 320x180 and rehashed. Raw records: `/data/tmp/fmn-execute-20261104-census-whole-local-v1/scenes/records.ndjson`, SHA-256 `4b44442955d26d0e7c57da7bc1bdf8bca0802e55ff168bebc33c131d7a1b1b8c`. The producer log, guarded inputs, complete outcome comparison and PNG inspection are alongside that run. No exclusions or failed-only retries were combined into this census.

This run restores the original 64-logical-CPU workstation, CPython 3.13.1 and the same 35 pinned scene-package versions as the previous `fe90a340` census: NumPy 2.5.2, IPython 9.14.0, requirements SHA-256 `8c5ab6727f43d055a8547aa3bc36b47ce4d29e8427cc3870f41725c47c9d8f89`; `pip check` passes. The Python SDK, scene dependencies and video assets were hashed before and after execution. The asset closure includes all 468 pinned tracked video files plus the existing untracked Wordle `data/pattern_matrix.npy`. Gensim, transformers, torch, requests, mido, opencv and private corpus assets remain absent. The configuration still changes only `directories.base` to empty (SHA-256 `9d79278259343e35d49d6b3d636a5a2d1d40db0af2aed8c5300b9a0153a15ca3`). Missing dependencies and assets remain visible outcomes.

**The run fails the unchanged 1,073-success floor: 1,072 scenes succeeded.** DSR `20261104T021534-387035` records exit 1. Against the previous complete `fe90a340` run (1,074 successes), exactly two outcomes change from `ok` to `timeout`: `_2023/clt/dice_sims.py:SimulationWithExpDistribution5Dice` and `_2023/clt_proof/main.py:DirectMGFInterpretation`. Their previous render times were 149.29 s and 162.59 s; this run reached the original 180 s deadline for both rendering and construction. There are no new successful scenes, and every other outcome is unchanged. All 30 timeouts, both losses and the previous raw records remain retained. The floor, timeout, scope and denominator were not changed. The failed floor remains open work; publishing the measured lower count does not pass it.

The 1,742 construct-only outcomes are 691 `missing_asset`, 521 `runtime_error`, 428 `missing_dependency`, 68 `capability_refusal`, 30 `timeout`, and four `tex_unsupported`. Render attribution is 33 `portal_exception` and 488 `scene_code_error`; exception location alone does not establish fault. Peak RSS median is 139.5 MiB and maximum 2,566.7 MiB across all 2,814 attempts, including failures. These figures do not establish a memory improvement.

This shared host is **calibration-unqualified (ADR-0024)**. The full old/current census was not measured in the same invocation, so the two timeout losses do not establish a code-induced performance regression. A separate offloaded attempt on another host stopped with exit 143 during construct checks; its 1,044 PNGs and partial logs are retained, but it has no complete records or final guards and is not combined with this census. No qualified PG speed win, cross-platform certification or human Look Gallery verdict is established. Differential and full-frame/offload measurements remain separately bound to their earlier `6ab48696` artifact and denominators. Whole source-policy, complete installed-policy and release qualification remain separate gates.

2814 scene classes; **1072 ok (38.1%)**. Module-weighted: 14 of 136 modules render every scene (10.3%).

| outcome | scenes | share |
|---|---:|---:|
| ok | 1072 | 38.1% |
| capability_refusal | 68 | 2.4% |
| portal_unbound | 0 | 0.0% |
| tex_unsupported | 4 | 0.1% |
| missing_dependency | 428 | 15.2% |
| missing_asset | 691 | 24.6% |
| scene_code_error | 488 | 17.3% |
| portal_exception | 33 | 1.2% |
| runtime_error | 0 | 0.0% |
| timeout | 30 | 1.1% |
| crash | 0 | 0.0% |

| year | scenes | ok | ok % |
|---|---:|---:|---:|
| _2020 | 461 | 78 | 16.9% |
| _2021 | 395 | 136 | 34.4% |
| _2022 | 350 | 131 | 37.4% |
| _2023 | 481 | 253 | 52.6% |
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
