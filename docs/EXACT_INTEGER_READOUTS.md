# Exact Integer readouts

The Python portal's `Integer` keeps Python and NumPy integral inputs as Python
integers through construction, `set_value`, `increment_value`, `get_value`, and
zero-decimal formatting. For example, `Integer(2**53 + 1)` displays
`9,007,199,254,740,993`, and incrementing `Integer(10**30)` by one changes its
last digit. Values larger than the finite f64 range are supported too, subject
to the existing 4096-character limit (including separators, signs and suffixes).

Values through the exact binary64 integer range retain the existing native
number-shelf layout. Larger values use the existing authored-string route:
native `Text` glyphs, existing placement/style rules, and the same atomic family
publication owner. No Python font engine, second renderer, or numeric native
record format is introduced. Subclass formatting/glyph hooks still dispatch.
Excessive integer bit lengths are refused before decimal expansion, and the
complete formatted string is admitted before glyph allocation.

`DecimalNumber` keeps an integral input the same way, as the pinned Reference does
(it stores the number as given): `DecimalNumber(10**18 + 99).get_value()` is the
exact int, and with `num_decimal_places=0` it displays `1,000,000,000,000,000,099`.
With decimal places, Python's `format()` goes through float, so the default
displays `1,000,000,000,000,000,128.00` in both engines. Float inputs stay floats.
Passing an already-rounded float cannot recover its original integer digits.
Custom format specifications retain their own formatting semantics.

## Animated counters

`ChangeDecimalToValue` retains exact integral start/target values when animating
an `Integer`. Alpha zero and one return those exact endpoints, without multiplying
them by floats. Small integer tracks preserve their ordinary interior samples;
float/complex `DecimalNumber` tracks and mixed nonintegral endpoints are unchanged.

For wide integral endpoints, intermediate values use the exact rational value
of the existing binary64 animation phase in the affine combination, truncated
toward zero to select the displayed integer. Thus a one-unit change at `10**30`
cannot produce unrelated neighboring digits, and a track beyond f64's finite
range need not overflow. Negative values, reverse tracks and overshooting phases
use the same rule. Rate functions, time windows, lifecycle/cleanup, and the scene
clock remain owned by the existing animation path. No endpoint is forced after
an authored easing function chooses a different final phase.

## Validation surfaces

`test_integer_readouts_protocol.py` and `test_integer_animation_protocol.py`
execute the production installer with real NumPy and explicit glyph/storage,
dispatch and animation-constructor doubles. They test the exact strings and
values crossing the native boundary, refusals, subclass behavior and an
independent Fraction interpolation oracle. They do not establish native render
or typography acceptance.

`FMN_TEST_NATIVE=1 python3 crates/fmn-python/tests/test_integer_readouts_native.py`
requires a matching installed native wheel. It checks actual glyph geometry,
bound updates, copies/pickle, native Scene playback, atomic failure, and actual
PNG sequences at one and four threads. The installed-wheel gate invokes it in a
clean process; source-only unittest discovery follows the existing native-suite
skip policy. This acceptance must run before claiming bead fm-yuin complete.
