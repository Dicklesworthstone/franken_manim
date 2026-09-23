# Labels on live coordinate systems

`NumberLine.add_numbers()` places labels through the live `number_to_point`
and `get_number_mobject` protocols. Rotation, reflection, shear, three-dimensional
placement, and native point edits no longer cause labels to be laid out on a
reconstructed horizontal bounding box.

```python
line = NumberLine((-2, 2, 1)).rotate(PI / 2)
line.add_numbers([-1, 0, 1], direction=RIGHT, buff=0.2)
```

Labels remain upright at construction. `direction` and `buff` are world-space
placement arguments, just as in `get_number_mobject`; transforming the labeled
family afterward transforms the labels with the line. Ordinary `.animate`
placement uses the existing native record interpolation and scene clock.

The method returns a `VGroup` containing the actual `DecimalNumber` objects (or
an authored `get_number_mobject` override's VMobjects) and stores it as `numbers`.
Existing ticks, tips, annotations and previous label groups are not rebuilt or
removed. Repeated calls append a group, matching the Reference. Numeric values,
precision, signs, units and supported decimal text/style configuration are passed
to the existing native number/text implementation; no glyph layout is duplicated.
Default ticks, exclusions and subclass overrides retain their public dispatch.

All requested labels are prepared before attaching the group. An exception,
cancellation or detected mutation of the source axis geometry, family, range or
scene ownership leaves the candidate unattached. Authored side effects are not
rolled back. Reentrant labeling of the same line refuses; independent operations
and copies made during callbacks remain valid. Arbitrary custom `add` overrides
are outside this preparation guarantee.

Inputs are bounded to 4,096 values/exclusions and one label batch to 1,048,576
native point records. Default tick ranges are admitted before `arange`. These
bounds do not preempt an authored callback that never returns or bound arbitrary
allocation performed by such a callback. Labels do not automatically track later
edits of the line's own points; transform their common family, or explicitly
create a new group and remove the old one in the scene's authored lifecycle.

Acceptance: `crates/fmn-python/tests/coordinate_labels.py`, including native
records, live views, callbacks, copied state and independent Y4M rendering at
one and four threads. The installed-wheel gate runs the same suite.
