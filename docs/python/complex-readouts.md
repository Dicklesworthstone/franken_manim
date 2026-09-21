# Complex-valued live readouts

The Python portal's `DecimalNumber` accepts finite real and complex values,
including NumPy complex scalars. Each real/imaginary component is formatted and
typeset by the existing Rust number shelf; the portal composes native mobjects.
No additional font, TeX process, renderer, or animation clock is involved.

```python
from manimlib import *

class ComplexReadout(Scene):
    def construct(self):
        value = DecimalNumber(1 + 2j, color=BLUE, unit="V")
        self.add(value)
        self.play(ChangeDecimalToValue(value, -3j))
        self.play(ChangeDecimalToValue(value, 4))
```

`hide_zero_components_on_complex=True` hides exactly-zero components. Set it to
`False` to retain the full `0.00+2.00i` form. The imaginary component following a
real component always includes its sign; `include_sign` controls the first
component. Precision, comma grouping, zero padding and negative-zero suppression
follow the real-number contract independently for each component. Zero decimal
places truncate each displayed component toward zero, as real readouts do.

The `i` glyph is part of the numeric string. An optional ellipsis and user unit
follow it. Units remain native plain text: a leading `^` requests raised
alignment, not TeX interpretation. Use `unit="^°"`, not `unit=r"^\circ"`.
Background rectangles remain root records, with numeric glyphs as children.

`set_value`, `increment_value`, `ChangingDecimal`, `ChangeDecimalToValue`, and
`CountInFrom` can cross between real, imaginary and full-complex displays. Updates
retain the object identity, fixed edge, scaled font size and glyph style, and
replace the exact glyph list when a number becomes shorter. Nested animated
readouts remain descendants rather than acquiring duplicate scene draw roots.

Replacement geometry and style are prepared before updating the live receiver.
Non-finite components, invalid geometry settings, missing glyphs and exhausted
layout budgets therefore do not publish a new value alongside old glyphs. The
4,096-character budget covers the **combined** numeric string, ellipsis and unit,
not 4,096 characters independently for each component. Authored Python override
side effects are not rolled back.

This extends the portal, not the Rust scalar type: Rust's `DecimalNumber` still
accepts `f64`; Rust callers compose scalar readouts for complex values. The native
real-number builder remains unchanged.

Acceptance coverage: `crates/fmn-python/tests/decimal_authoring.py`.
