# BN-17 — Image input is bounded and outside sampling always refuses

**Status:** Draft. Landed in W10 (fm-5wq.4.25); becomes Final when the
Python-portal gate and its source-unedited image corpus pass.

## What changed

Classic manim's `ImageMobject.point_to_rgb` intends to reject a point outside
the image. Its guard combines the two axis checks with `and`, however, so a
point outside only one axis reaches Pillow with an invalid or unintended pixel
coordinate. FrankenManim rejects a point outside either axis. Points inside
the image use the familiar axis-aligned family box and top-row-first pixel
orientation.

Image acquisition is also explicit. Local PNG and JPEG paths are decoded by
the bounded native codec into Marionette's durable image resource. A URL does
not trigger an ambient download; a host that wants network assets must provide
the declared `AssetFetcher` capability. The portal additionally accepts GIF,
BMP, WebP and TIFF through Reel's governed image decoder when an explicit
`ffmpeg_bin` capability is supplied. Input bytes select the recognized format;
animated input contributes its first frame. Unknown formats and malformed
native PNG/JPEG inputs refuse without a fallback. This remains a bounded
format catalog rather than full image-format parity.

## OBJ diffuse-map placement and native clamp policy

The material-aware OBJ importer accepts MTL `-o`, `-s`, and `-clamp` options
for `map_Kd` and the `map_Ka` fallback. Offset defaults to zero and scale to
one on omitted axes. The importer applies `uv * scale + offset` in OBJ's
coordinate convention before converting V to the native top-row-first image
convention. Repeat addressing is evaluated by the renderer after interpolation;
the importer never folds vertex coordinates into the unit square.

An explicit `-clamp on` selects Marionette's `ImageWrap::ClampToEdge` for both
axes, and `-clamp off` selects `ImageWrap::Repeat`. This is a documented native
importer contract: the original Alias/Wavefront manual describes leaving the
underlying material unchanged outside a clamped map, a decal-style behavior
which this importer does not implement. Edge clamping follows the common
Assimp import mapping and the existing Lumen texture sampler. No `-clamp`
declaration preserves the host-supplied resource's sampler; the standard image
decoder supplies Repeat. This texture addressing policy is separate from
`ImageMobject.point_to_rgb`'s outside-point refusal above.

These maps are 2D rasters. The optional depth component must be neutral: zero
for `-o`, one for `-s`. Nonneutral depth, unsupported map options, nonfinite
arguments, and transformed UVs outside native record range refuse explicitly.
The selected path, transform and sampler travel together under diffuse-map
precedence. Existing UV records and image sampler fields carry the result
through copies, snapshots and durable restoration without a new wire format.

Source contracts: the [Alias/Wavefront FILE FORMATS v4.2 manual, October 1995](https://www.fileformat.info/format/material/)
defines optional components, defaults and the original clamp behavior;
[Assimp's OBJ material importer](https://github.com/assimp/assimp/blob/HEAD/code/AssetLib/Obj/ObjFileImporter.cpp)
maps explicit clamp to its texture Clamp mode. See
[native OBJ model guidance](../python/obj_models.md#texture-placement-and-addressing)
for supported syntax and migration examples.

## Migration guidance

- Code that accidentally sampled beyond one edge must clamp its point or
  handle the raised `ValueError`; relying on Pillow's downstream indexing was
  never a stable image-coordinate contract.
- Download remote assets through the host application, record them in the
  input closure, and pass a local path to `ImageMobject`.
- For GIF/BMP/WebP/TIFF, pass `ffmpeg_bin` to the image/texture constructor or
  setter, or convert to native PNG/JPEG before construction. There is no
  implicit tool lookup for native PNG/JPEG and no silent format substitution.
- Certified generations refuse ffmpeg-derived live images, including copies,
  interpolated materials and restored checkpoints. Explicit baked RGBA inputs
  and durable embedded pixels have their own recorded pixel identities.
- `set_color`, including its optional `opacity` argument, remains the
  Reference's no-op. Use `set_opacity` to change the live image rows.

## Evidence

- `crates/fmn-library/src/image.rs`: budgeted PNG/JPEG decode, exact image
  record schema, aspect sizing, durable resource construction, and sampling.
- `crates/fmn-render/src/retained.rs`: ImageQuad lowering, resource interning,
  texture sampling, and camera-bound rendering.
- `crates/fmn-python/tests/bridge.py`: exact and extension-less path lookup,
  URL and malformed-input refusals, outside sampling, copy/adoption resource
  survival, and a production Lumen/Reel PNG pixel witness.
- `crates/fmn-output/src/image.rs`: native-first, byte-recognized image decoding
  and exact source, pixel and external-tool receipts.
- `crates/fmn-python/tests/image_inputs.py`: real external/native pixel and
  frame equality, immutable receipt propagation, checkpoint restoration,
  light/dark materials and certified refusal before publication.
- `crates/fmn-library/tests/obj_materials.rs`: exact transformed corner UVs,
  bounded option parsing, independent textured-mesh pixel oracles at 1/4/16
  threads, and durable sampler restoration.
- `crates/fmn-mobject/tests/texture_pairs.rs`: sampler replacement shares both
  pixel allocations, preserves existing copies, and updates image revisions.
- `crates/fmn-python/tests/obj_materials.py`: real MTL/file imports, spaced
  texture paths, independent transformed-UV and clamp pixel witnesses, copy,
  pickle, restoration, and failed-reinitialization ownership.
- Pinned Reference:
  `scripts/manim_ref/manimlib/mobject/types/image_mobject.py::ImageMobject`
  at commit `6199a00d4c1b1127ebe45cb629c3f22538b10e13`.
