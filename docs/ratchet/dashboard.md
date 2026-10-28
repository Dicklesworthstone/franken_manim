# The fmd-math coverage ratchet

The headline metric of the no-LaTeX pivot (§11.5): what fraction of the
real 3b1b formula corpus typesets natively. The denominator is **frozen**
(G0-4: `9269` distinct strings, `17711` occurrences, corpus hash
`a8325e49e0ce78fcc735533952740e9adeaaa5cb10f9c13d73aaa3ba4bf883fc`, rules_version 1); the numbers may only rise.

**Computed against franken_markdown `04d811db8dbf`.**

| Plane | Occurrence-weighted | Unique-string |
|---|---|---|
| **Parse** | 99.994 % | 99.989 % |
| **Parse + typeset returned Ok** | 99.994 % | 99.989 % |
| **Layout within 5% of real TeX (oracle)** | 94.597 % | 94.908 % |
| **Noto math or Plex Sans face usage** | 0.006 % | 0.011 % |

"Typeset returned Ok" means fmd-math produced a layout without an error. It does not show that the layout is right. The corpus TeX box oracle (fm-tex-layout-oracle-bkbc) lays out every math-mode corpus string TeX can typeset alone (4175 strings, 11183 occurrences) and compares each box with real TeX's: 4149 are within 5% and 4167 within 10%, covering 11142 occurrences within 5%. The text-mode census (fm-texttext-box-oracle-r11-hkdv) lays out every text-mode corpus string the Reference sets on one line (3747 strings, 4689 occurrences) and compares each box with real TeX's: 3733 are within 5% and 3743 within 10%, covering 4666 occurrences within 5%. The paragraph census (fm-texttext-line-breaking-r11-bfdx) lays out every multi-line text-mode corpus string, and every one wider than the Reference's 345 pt line, as its centred paragraph (925 strings, 956 occurrences) and compares the widest line, the first line's height and the depth to the last line with real TeX's: 915 are within 5% and 916 within 10%, covering 946 occurrences within 5%. The oracle row counts only those verified strings against the whole corpus; fragments TeX cannot typeset alone count as unverified.

## Noto math or Plex Sans face usage

1 strings (1 occurrences) lay out with at least one glyph from Noto math or Plex Sans. Standard relations among those glyphs: 0. This includes explicitly requested sans styling and excludes bundled AMS faces. It does not measure ink fidelity. The standard relation set never falls back.

| Character | Code point | Occurrences |
|---|---|---|
| ∯ | U+222F | 1 |

## Pending constructs (parse plane)

| Construct | Occurrences blocked | Tracked at |
|---|---|---|
| `\dx` | 1 | franken_manim fm-j5t |

## Pending at layout (parse succeeds)

| Construct | Occurrences blocked | Tracked at |
|---|---|---|

## Trend (by franken_markdown rev)

| Rev | Parse occ. % | Parse uniq. % | Layout occ. % | Layout uniq. % |
|---|---|---|---|---|
| `5310d87a9db3` | 99.577 | 99.266 | 98.916 | 98.188 |
| `4e5066c62818` | 99.577 | 99.266 | 99.379 | 98.921 |
| `76fcbd264d67` | 99.881 | 99.795 | 99.684 | 99.450 |
| `5db49f54f0cf` | 99.944 | 99.892 | 99.746 | 99.547 |
| `4743f78d3e57` | 99.960 | 99.924 | 99.763 | 99.579 |
| `b30516ea9522` | 99.977 | 99.957 | 99.780 | 99.612 |
| `0e727c336281` | 99.989 | 99.978 | 99.791 | 99.633 |
| `82588865c453` | 99.994 | 99.989 | 99.797 | 99.644 |
| `e911be2ad4ff` | 99.994 | 99.989 | 99.994 | 99.989 |
| `68fe29b8af0d` | 99.994 | 99.989 | 99.994 | 99.989 |
| `4c8caa256e30` | 99.994 | 99.989 | 99.994 | 99.989 |
| `f059cac6b861` | 99.994 | 99.989 | 99.994 | 99.989 |
| `bfb3221c53df` | 99.994 | 99.989 | 99.994 | 99.989 |
| `0a0281278edc` | 99.994 | 99.989 | 99.994 | 99.989 |
| `a3578a0f1bd7` | 99.994 | 99.989 | 99.994 | 99.989 |
| `a8aab0db646f` | 99.994 | 99.989 | 99.994 | 99.989 |
| `d664c138440f` | 99.994 | 99.989 | 99.994 | 99.989 |
| `e4e8e2ee1b8e` | 99.994 | 99.989 | 99.994 | 99.989 |
| `798de11bb51c` | 99.994 | 99.989 | 99.994 | 99.989 |
| `bd00a2471566` | 99.994 | 99.989 | 99.994 | 99.989 |
| `3ed1d3320069` | 99.994 | 99.989 | 99.994 | 99.989 |
| `126a5304e8d9` | 99.994 | 99.989 | 99.994 | 99.989 |
| `e0dd66e7c5e7` | 99.994 | 99.989 | 99.994 | 99.989 |
| `80cbadcd0d37` | 99.994 | 99.989 | 99.994 | 99.989 |
| `23c4d0c546a9` | 99.994 | 99.989 | 99.994 | 99.989 |
| `206bb6396c3b` | 99.994 | 99.989 | 99.994 | 99.989 |
| `216c7c3b0d99` | 99.994 | 99.989 | 99.994 | 99.989 |
| `49466bc2dc4e` | 99.994 | 99.989 | 99.994 | 99.989 |
| `4db6f439f5ed` | 99.994 | 99.989 | 99.994 | 99.989 |
| `abbb957d1b48` | 99.994 | 99.989 | 99.994 | 99.989 |
| `83d363c5d499` | 99.994 | 99.989 | 99.994 | 99.989 |
| `2f1e00c08eb4` | 99.994 | 99.989 | 99.994 | 99.989 |
| `439a910ce7c1` | 99.994 | 99.989 | 99.994 | 99.989 |
| `9b457a3895d7` | 99.994 | 99.989 | 99.994 | 99.989 |
| `c81208f19a81` | 99.994 | 99.989 | 99.994 | 99.989 |
| `e6da6ee1a73e` | 99.994 | 99.989 | 99.994 | 99.989 |
| `2d820c52b718` | 99.994 | 99.989 | 99.994 | 99.989 |
| `683e4cccb66b` | 99.994 | 99.989 | 99.994 | 99.989 |
| `37632d2dc0ab` | 99.994 | 99.989 | 99.994 | 99.989 |
| `4b705d147c10` | 99.994 | 99.989 | 99.994 | 99.989 |
| `1ffcadc92e9f` | 99.994 | 99.989 | 99.994 | 99.989 |
| `8667819a0a5a` | 99.994 | 99.989 | 99.994 | 99.989 |
| `d99fee964a47` | 99.994 | 99.989 | 99.994 | 99.989 |
| `cc470d1a0eaa` | 99.994 | 99.989 | 99.994 | 99.989 |
| `450111a4199a` | 99.994 | 99.989 | 99.994 | 99.989 |
| `04d811db8dbf` | 99.994 | 99.989 | 99.994 | 99.989 |

## How this is enforced

- Coverage is a pure function of (frozen corpus, fmd-math pin), so the
numbers can only move when `SUITE.lock`'s `franken_markdown` row moves.
An always-on CI test requires `baseline.tsv` to name the current pin:
**a pin bump without a ratchet re-run fails CI.**
- On corpus-bearing machines the ratchet test recomputes all four
counts and fails on any decrease; `RATCHET_UPDATE=1` blesses a
deliberate advance (with this dashboard regenerated in the same
commit).
- Every non-tier-1 construct must fail with its precise, named,
tier-tagged error — audited construct-by-construct against the G0-4
table in always-on CI. Nothing ever fails silently.

## The escalation path (R1, the G2 checkpoint)

If coverage misses a gate's criteria, the response is a **public
amendment with a construct-sprint plan** — never a silent slip: the
gap is named construct-by-construct above, each with its tracked
bead, and the gate review adjudicates the sprint scope in the open.

## Licensing (§15.3)

The corpus strings are 3b1b-authored course material and stay in the
private fixture; this dashboard publishes **numbers, construct names,
and hashes only**. Anyone with the pinned trees can reproduce the
denominator byte-for-byte via `scripts/harvest_tex_corpus.py`.

## A second corpus: ManimCommunity/manim

The corpus above is 3b1b's own videos. This one is the TeX of ManimCommunity/manim's library, example scenes and documentation examples: `113` distinct strings, `142` occurrences (MIT; commit and counts in `crates/fmn-conformance/fixtures/second_corpus/PROVENANCE.json`). It is public, every checkout measures it, and its numbers may only rise.

| Plane | Occurrence-weighted | Unique-string |
|---|---|---|
| **Parse** | 98.592 % | 99.115 % |
| **Parse + typeset returned Ok** | 97.887 % | 98.230 % |

Blocked, by occurrences: `\vv` (2), `char:U+4F60` (1).

