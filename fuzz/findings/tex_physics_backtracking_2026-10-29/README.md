# Resolved: exponential re-parsing of unclosed physics arguments

- Parser: fmd-math (franken_markdown), reached through `fmn_library::Tex`
  under the default preamble pack, whose physics option gives `\ln`, `\sin`,
  `\exp` and the rest delimited arguments
- Found by: the `tex_layout` campaign of 2026-10-29 (fm-fmd-math-fuzz-budgets-k8fe),
  about 8 minutes into its first hour, on a 564-byte inline `Tex` that took
  477 ms twice, past the target's 100 ms per-input bound
- Reproducer here: `slow-ln-chain-24.bin` (98 bytes). The selector byte 0x01
  (inline `Tex`) is followed by twenty-four `\ln(` and an `x`. It was minimized
  by hand: the campaign's own input mutated strings from the G0-4 TeX corpus,
  which is CC BY-NC-SA and stays out of the repo
- Resolution: franken_markdown `db22126c`, pinned at `e7b4309d`

## Root cause

`physics_delimited` parses a physics argument speculatively. When the argument
does not close, it rewinds, and the ordinary list re-reads the same tokens as
plain material. Each unclosed `\ln(` nested inside another was therefore
re-parsed by every enclosing argument, so parse time doubled per level:

| `\ln(` openers | 10 | 12 | 14 | 16 |
|---|---|---|---|---|
| time (release) | 0.31 ms | 1.3 ms | 5.3 ms | 19 ms |

Extrapolating, twenty-four would take about 5 s and thirty, a 121-byte
string, about 5 minutes. No budget bounded it: the input is tiny and nests 24
levels deep, well inside the 64-level bound.

## Resolution

- **Failed openers are remembered.** The parser records, per token, an argument
  that opened there and did not close, and skips the retry, which would fail the
  same way.
- **A nesting-budget error inside an attempt is propagated.** It is the named
  `MathError::Budget`, not a fallback. That makes the outcome depend only on the
  tokens, so remembering it is exact.
- **Closing delimiters come from one table pass.** Previously each opener scanned
  to the end of input. A unit test pins the table to the old scan.

After the fix (release):
- the campaign's input takes 0.3 ms;
- 1 000 nested openers take 13.5 ms;
- a 65th nested opener is refused by name.

## Regressions

- fmd-math `tests/chaos.rs`
  `nested_unclosed_physics_arguments_parse_in_bounded_time` covers `\ln(`,
  `\cos[2](`, `\cos[` and `\exp[` chains, the pair counts, and the 65-opener
  refusal.
- franken_manim `crates/fmn-tex/tests/fmd_math_surface.rs`
  `the_resource_budgets_hold_through_the_pin` runs twenty-four `\ln(` through
  `TexEngine` within 5 s, alongside the input and nesting budgets.
- The re-run `tex_layout` campaign at `e7b4309d` loaded the campaign's
  reproducer in its seed set (`fuzz/campaigns/2026-10-29.md`).
