<!-- fmn-measurement {"inputs_digest": "sha256:c758365cab1ad92e over 2814 scene ids", "max_age_commits": 200, "measured_at": "2026-11-04", "measured_commit": "e8ce37623aea9e07fce883e3e784d6ad12ff7523", "producer": "python3 scripts/corpus_scene_sweep.py --report /data/tmp/fmn-execute-20261104-census-whole-local-v2/scenes/records.ndjson --dashboard docs/ratchet/scene_dashboard.md --tsv docs/ratchet/scene_outcomes.tsv --title 3b1b corpus scene sweep (fmn-python) --note /data/tmp/fmn-execute-20261104-corpus68-scene-note.md --measured-commit e8ce37623aea9e07fce883e3e784d6ad12ff7523"} -->
# 3b1b corpus scene sweep (fmn-python)

Measured on 2026-11-04 with the release-profile wheel built from `e8ce37623aea9e07fce883e3e784d6ad12ff7523`, wheel SHA-256 `91180cdc48f3b187e472e16d2462b207e1f8b2be6aac5000900db0207132f306`, native extension `76092ec3aece9608ec5f645824003526edeb24d2a1a8d1fcfe54c4d961b24c7e`, and matching standalone CLI `902e563fc9204422ff1e394777846d53065048f581cfa9b7cf9a57eeebe4f5f3`. All 4,408 tracked non-tracker source entries matched before and after the build and census (aggregate `aa9ff90c05f6c864943a33f6cca207670dcbbce81244c77e9fbed374628a9398`); all 145 installed portal payload entries also remained unchanged. Every record carries installed payload digest `6d488bd197cb27a74d2abcf1d4bf842b2b6ee8a599602548c1130112ba91956a`. Artifact construction and the census are measurement evidence; full clean-source, installed-policy and release qualification remain separate gates.

All 2,814 original scene IDs from pinned `3b1b/videos@e317d6c5eaa8370a2deb4d148c246b0d0e9fbe6f` were freshly attempted at 320x180, the original 180 s timeout and 16 concurrent scenes, years 2020–2026. Every one of the 521 runtime errors received in-process attribution, and all 1,740 failed renders received construct-only checks. All 1,074 successful records map exactly to 1,074 physical PNG files, each fully decoded at 320x180 and rehashed. Raw records: `/data/tmp/fmn-execute-20261104-census-whole-local-v2/scenes/records.ndjson`, SHA-256 `380a4ea2960d6516a476ba71221fed3e957d4839ce2c37fd4d9ef4b0c5ab4a66`. Guarded inputs, full outcome comparison and PNG inspection are alongside that run. This is one complete census; no failed-only retries were combined into its count.

The original 64-logical-CPU workstation, CPython 3.13.1 and the same 35 pinned scene-package versions are retained: NumPy 2.5.2, IPython 9.14.0, requirements SHA-256 `8c5ab6727f43d055a8547aa3bc36b47ce4d29e8427cc3870f41725c47c9d8f89`; `pip check` and all 14 original native/signature checks pass. The Python SDK, dependencies and video assets were hashed before and after execution. The asset closure includes all 468 tracked video files plus the existing Wordle `data/pattern_matrix.npy`. Gensim, transformers, torch, requests, mido, opencv and private corpus assets remain absent. Configuration still changes only `directories.base` to empty (SHA-256 `9d79278259343e35d49d6b3d636a5a2d1d40db0af2aed8c5300b9a0153a15ca3`). Missing dependencies and assets remain visible outcomes.

**The unchanged 1,073-success floor passes: 1,074 scenes succeeded.** DSR `20261104T075250-4157451` records both checks passing: the original 14 native checks and the complete census. Against the retained failed `a215e9a2` run, exactly two outcomes recover from `timeout` to `ok`: `_2023/clt/dice_sims.py:SimulationWithExpDistribution5Dice` and `_2023/clt_proof/main.py:DirectMGFInterpretation`; every other outcome is unchanged. All 1,072 common successful PNGs are byte-identical after rehashing the old physical files. The failed 1,072 run and its original records SHA-256 `4b44442955d26d0e7c57da7bc1bdf8bca0802e55ff168bebc33c131d7a1b1b8c` remain retained. Against the earlier complete `fe90a340` census, all 2,814 outcomes are identical: this restores its measured coverage. The floor, timeout, scope and denominator were unchanged.

The 1,740 construct-only outcomes are 691 `missing_asset`, 521 `runtime_error`, 428 `missing_dependency`, 68 `capability_refusal`, 28 `timeout`, and four `tex_unsupported`. Render attribution is 33 `portal_exception` and 488 `scene_code_error`; exception location alone does not establish fault. Peak RSS median is 138.4 MiB and maximum 2,688.1 MiB across all 2,814 attempts, including failures. The previous run's maximum was 2,566.7 MiB; these observations do not establish a memory improvement. Remaining unsupported constructs and downstream scene errors remain recorded.

This shared host is **calibration-unqualified (ADR-0024)**. The complete old/current census runs were separate invocations; timing changes cannot be attributed solely to the code or claimed as a qualified PG win. The new native change since the failed `a215e9a2` measurement is the prepared Transform carrier repair; the intervening report-only change is separate. An earlier offloaded attempt stopped with exit 143 during construct checks; its 1,044 PNGs and partial logs remain separate and are excluded from this census. Cross-platform certification, human Look Gallery verdicts, complete installed-policy and release acceptance are not established here. Differential and full-frame/offload measurements remain bound to their earlier `6ab48696` artifact and denominators.

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
