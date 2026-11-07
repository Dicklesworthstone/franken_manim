# Native OBJ models with materials and textures

`ThreeDModel(obj_file, height=3)` loads OBJ geometry and its declared MTL
libraries into native, lit mesh children. The class remains a heterogeneous
`Group`; an unmaterialed model still has one plain `Surface` child. Named
material runs become colored `Surface` or textured `TexturedGeometry` children.

```python
from manimlib import ThreeDModel

model = ThreeDModel("assets/model.obj", height=3)
scene.add(model)
scene.play(model.animate.rotate(0.5))
for part in model:
    print(part.material_name)
```

OBJ parsing and triangle construction use Atlas's existing geometry parser.
The added native MTL parser reads `newmtl`, RGB `Kd`, `d`, `Tr`, `map_Kd` and
`map_Ka`. A diffuse `map_Kd` takes precedence over the ambient-map fallback,
regardless of their order within a material. Across libraries, the first
material definition in OBJ library order wins. Texture paths are relative to
the MTL file which defines them, not the working directory or OBJ directory.
Multiple material bindings to the same resolved image reuse its decoded native
resource. Unused material images are not opened.

Face order is retained as contiguous material runs. An A/B/A sequence remains
three children instead of moving the final A faces before B. UVs and normals
remain per corner, so a shared vertex may have different UVs on either side of
a seam. OBJ V coordinates are converted once to the native top-row-first image
convention. All parts use one whole-model center and height normalization;
each part is **not** independently centered or stretched.

Colored parts use `Kd` and dissolve alpha. Textured parts use their image and
dissolve alpha with the existing native lighting; `Kd` is not multiplied into
the image. This is the existing manim-facing lighting model, not a new MTL/PBR
shader. Texture sampling uses the existing native image sampler.

## Convex and concave polygon faces

OBJ faces may be planar simple polygons, including concave outlines and
collinear boundary corners with distinct UV or normal indices. The importer
triangulates these faces in their original 3D winding. A U-shaped face keeps
its cutout; triangulation does not fill the polygon's convex hull. Every
accepted boundary corner participates in a nondegenerate triangle, so a UV
value on a straight boundary segment is retained.

The geometry reader projects a normalized copy onto the plane perpendicular
to the largest component of the face normal. It keeps the original vertex
coordinates and per-corner indices in the resulting triangles. Exact valid
convex fans retain their established triangle order. Concave faces and convex
faces whose fan would discard a collinear corner use Chisel's deterministic,
boundary-preserving ear clipper.

A source face can repeat its first corner at the end only with the same
vertex, UV and normal indices. That closing repetition adds no triangle.
Coincident corners carrying different indices are refused rather than merged.
Material runs use the geometry parser's actual emitted face intervals, so a
closing repetition never shifts the material attached to the following face.

Faces with more than three corners must be planar within `1e-6` of their
largest 3D bounding-box extent. This tolerance admits coordinate roundoff; it
does not flatten or repair the model. Self-intersecting, touching, backtracking,
zero-area or numerically ambiguous polygon boundaries refuse with the source
face's line. Explicit three-corner faces retain their existing behavior,
including defined zero-area, unlit triangles. Nonplanar surfaces should be
triangulated in the authoring tool with the intended diagonal choices.
See [BN-22](../behavior_notes/BN-22-surface-construction-admission.md#planar-obj-polygon-admission)
for the admission contract.

## Texture placement and addressing

`map_Kd` and the `map_Ka` fallback accept these options before the filename:

| Option | Native import behavior |
|---|---|
| `-o u [v [w]]` | UV offset; omitted components default to zero. |
| `-s u [v [w]]` | UV scale; omitted components default to one. Negative and zero scales are supported. |
| `-clamp on` | Clamp both sampling axes to edge texels. |
| `-clamp off` | Repeat the image on both sampling axes. |

For example, `map_Kd -s 2 .5 1 -o -.25 .125 0 -clamp off maps/tiles.png`
samples the map at `(2*u - .25, .5*v + .125)` in OBJ UV space. The native V
conversion happens after this transform, exactly once. Coordinates outside
the unit square stay in the records: wrapping is evaluated after perspective
interpolation, and bilinear filtering crosses repeat seams correctly. Options
can appear in any order; the last occurrence of a repeated option wins. Each
new map declaration starts with fresh defaults, so an ignored `map_Ka` does
not change a selected `map_Kd`'s placement or sampler.

The third component describes 3D texture depth or displacement-map behavior in
the original format. These diffuse maps are 2D rasters, so this importer admits
only neutral third components: `0` for offset and `1` for scale. Other values
raise a named import error. Nonfinite components and transformed coordinates
that cannot be represented by native records also refuse.

The explicit clamp contract is the native image sampler's **clamp to edge**.
It does not implement Advanced Visualizer's material-preserving decal behavior
outside the unit UV square. This distinction is recorded in
[BN-17](../behavior_notes/BN-17-image-input-and-sampling.md). When no `-clamp`
option is present, the supplied image's sampler is preserved. Host-decoded
images use Repeat by default. Material bindings share decoded pixel bytes even
when their sampler policies differ; copy, pickle and scene restoration preserve
the selected policy without reopening the image.

## Failure and ownership

The host reads local files, while all geometry, material parsing, image decoding
and rendering remain native. No trimesh, pywavefront, Pillow, external image
tool, downloader, or temporary conversion file is used. Geometry/source errors,
missing libraries, undefined selected materials, missing or invalid required
images, and textured corners lacking UVs are errors: the loader does not
silently return a gray substitute.

All required inputs and mesh children are prepared before initializing the
receiving Group. An input or native preparation failure does not replace an
existing detached model's family. Bound reinitialization and same-object
constructor reentry refuse before input conversion. Authored subclass hooks,
path conversion side effects and host filesystem races are not rolled back;
this is an ordinary trusted-host authoring API, not a sandbox.

Images are immutable native resources. Copies, pickle restoration, captures
and scene snapshots keep their resources and do not reopen the original files.
The existing pixel/material setters work on textured children; ordinary group
placement animation keeps their mesh topology and images. Triangle meshes are
not sent through UV-grid resampling merely because their class derives from
`Surface`. A mesh-to-UV-grid alignment still refuses rather than inventing grid
topology.

`model.asset_paths` lists the absolute inputs actually read. It can be passed
as explicit asset paths to a `SceneProject` watch; this importer does not install
a second watcher or claim certified closure for mutable host files.

## Admission limits and format boundary

The portal bounds each input to 64 MiB, total distinct encoded/source bytes to
128 MiB, and material/input counts to 4096. Native image decoding has the
existing 16,777,216-pixel per-raster limit. Total decoded texture bindings are
limited to 256 MiB, conservatively counting each material binding even when
multiple bindings share one allocation. OBJ material import allows at most
262,144 triangles, 64 KiB per source line and 4096 bytes per name/path; vertex,
UV and normal counts also obey the native geometry limits. Nonregular files
are refused without blocking on FIFOs. Local relative/absolute paths and file
symlinks are supported; paths are not confined to the OBJ's directory.

Sources must be UTF-8. `mtllib` accepts whitespace-separated library paths;
quoted library filenames and continued lines are not supported. Texture-map
filenames may contain spaces, and their internal whitespace is preserved.
Options precede the path; prefix a numeric or option-like filename with `./`
to distinguish it from an optional component or a flag. Map options other
than `-s`, `-o`, and `-clamp`, non-RGB `Kd`, and out-of-range RGB/dissolve values
explicitly refuse.
Other MTL reflectance properties are ignored, not emulated. A face may name at
most 4096 source corners, including an optional repeated closing corner.
The boundary-preserving triangulator also enforces a deterministic per-face
predicate-scan budget of 67,108,864 units. Simplicity checks and unsuccessful
ear searches both consume that budget; exhausting it is a named refusal.
Partition larger or pathological polygons before import. Free-form OBJ
curves/surfaces and faces containing separate hole contours remain unsupported.

## Native Rust composition

`ThreeDModel::from_obj` and `ObjMesh::parse` use the same polygon triangulation
as the material-aware importer, without opening any files.
For material-aware construction, use `fmn_library::obj_materials::ObjDocument`
and `parse_mtl`, then supply material-name-to-`ImageResource` bindings to
`ObjDocument::to_mobject`. That library layer performs no filesystem access;
asset authority stays with the host.

`ObjMaterial::texture_options` is an `ObjTextureOptions` value with `offset`,
`scale` and optional `clamp` fields. Programmatic materials follow the same
finite-coordinate checks as parsed MTL input. `ImageResource::with_sampler`
can replace a native resource's policy while sharing its immutable pixel
storage; install the resulting resource through `Stage` to update retained
render state.

The independent Rust tests are `fmn-library/tests/obj_materials.rs` and
`fmn-library/tests/obj_polygons.rs`. They verify exact corner records, signed
area, independent cutout coverage, material ranges, and named refusals. Native
portal acceptance is `fmn-python/tests/obj_materials.py`; host input policy is
`test_obj_inputs.py`, whose parser doubles are explicitly not geometry evidence.
