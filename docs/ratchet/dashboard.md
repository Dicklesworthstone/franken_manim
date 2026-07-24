# The fmd-math coverage ratchet

The headline metric of the no-LaTeX pivot (§11.5): what fraction of the
real 3b1b formula corpus typesets natively. The denominator is **frozen**
(G0-4: `9269` distinct strings, `17711` occurrences, corpus hash
`a8325e49e0ce78fcc735533952740e9adeaaa5cb10f9c13d73aaa3ba4bf883fc`, rules_version 1); the numbers may only rise.

**Computed against franken_markdown `4e5066c62818`.**

| Plane | Occurrence-weighted | Unique-string |
|---|---|---|
| **Parse** | 99.577 % | 99.266 % |
| **Parse + layout** | 99.379 % | 98.921 % |

## Pending constructs (parse plane)

| Construct | Occurrences blocked | Tracked at |
|---|---|---|
| `\centering` | 13 | franken_manim fm-j5t |
| `env:flushleft` | 8 | franken_manim fm-kg9 |
| `\female` | 6 | franken_manim fm-j5t |
| `\small` | 6 | franken_manim fm-j5t |
| `\substack` | 5 | franken_manim fm-j5t |
| `\Large` | 4 | franken_manim fm-j5t |
| `\male` | 4 | franken_manim fm-j5t |
| `\i` | 3 | franken_manim fm-j5t |
| `\'` | 2 | franken_manim fm-j5t |
| `\circlearrowright` | 2 | franken_manim fm-j5t |
| `\dddot` | 2 | franken_manim fm-j5t |
| `\ding` | 2 | franken_manim fm-j5t |
| `\j` | 2 | franken_manim fm-j5t |
| `\nmid` | 2 | franken_manim fm-j5t |
| `\"` | 1 | franken_manim fm-j5t |
| `\circlearrowleft` | 1 | franken_manim fm-j5t |
| `\copyright` | 1 | franken_manim fm-j5t |
| `\ddddot` | 1 | franken_manim fm-j5t |
| `\doublespacing` | 1 | franken_manim fm-j5t |
| `\dx` | 1 | franken_manim fm-j5t |
| `\earth` | 1 | franken_manim fm-j5t |
| `\footnotesize` | 1 | franken_manim fm-j5t |
| `\huge` | 1 | franken_manim fm-j5t |
| `\large` | 1 | franken_manim fm-j5t |
| `\oiint` | 1 | franken_manim fm-j5t |
| `\tiny` | 1 | franken_manim fm-j5t |
| `\xmapsto` | 1 | franken_manim fm-j5t |
| `\xrightarrow` | 1 | franken_manim fm-j5t |

## Pending at layout (parse succeeds)

| Construct | Occurrences blocked | Tracked at |
|---|---|---|
| `char:U+1D53C` | 16 | franken_markdown br-…-4vjj (Noto math-alphanumeric subset) |
| `char:U+1D4AA` | 14 | franken_markdown br-…-4vjj (Noto math-alphanumeric subset) |
| `char:U+1D4A9` | 3 | franken_markdown br-…-4vjj (Noto math-alphanumeric subset) |
| `char:U+1D49E` | 1 | franken_markdown br-…-4vjj (Noto math-alphanumeric subset) |
| `char:U+1D4AE` | 1 | franken_markdown br-…-4vjj (Noto math-alphanumeric subset) |

## Trend (by franken_markdown rev)

| Rev | Parse occ. % | Parse uniq. % | Layout occ. % | Layout uniq. % |
|---|---|---|---|---|
| `5310d87a9db3` | 99.577 | 99.266 | 98.916 | 98.188 |
| `4e5066c62818` | 99.577 | 99.266 | 99.379 | 98.921 |

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
