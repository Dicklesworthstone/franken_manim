# HDR video

The standalone CPU renderer can export opaque BT.2020/PQ video directly from
its linear RGBA16F raster. `--hdr-pq` selects limited-range, ten-bit P010 input
for the encoder, a 203 cd/m² reference white, and a 1,000 cd/m² content limit.
It also selects video output and durable publication.

```sh
fmn --hdr-pq --vcodec auto --crf 18 --preset medium \
  --resolution 1920x1080 --fps 30 --video_dir media \
  @builtin circle_shift.v1
```

`--vcodec auto` selects the software `libx265` encoder for this HDR profile.
An explicit encoder from the command line or configuration remains selected.
The inherited configuration's `libx264` setting also remains selected; use
`--vcodec auto` when you want the HDR software default. Every selected encoder
must advertise a compatible ten-bit input format. A missing encoder or an
eight-bit-only encoder produces a capability error before scene execution.

The native CLI publishes MP4. The Rust `render()` API also supports opaque MOV
with `RenderFormat::Mov` and `RenderOptions::hdr_pq`.

## Python scenes

The Python portal uses the same native HDR converter for ordinary captures,
offloaded animation frames, moving cameras, and live recordings:

```sh
fmn-python scene.py MyScene --format mp4 --hdr-pq --vcodec auto \
  --resolution 1920x1080 --fps 30 --output media/scene.mp4
```

For the Python API, select the profile in the scene's file-writer configuration:

```python
from fmn_python import render_scene

report = render_scene(
    MyScene,
    "media/scene.mp4",
    scene_kwargs={"file_writer_config": {"hdr_pq": True, "video_codec": "auto"}},
)
print(report.hdr_pq)
```

An existing scene can set `scene.file_writer.hdr_pq = True` before rendering or
starting a recording. The Python profile uses the same 203 cd/m² white and
1,000 cd/m² peak as `--hdr-pq`. The completed `RenderResult.hdr_pq` and robot
report record the profile actually used, even if the writer configuration is
changed later. Ordinary ten-bit SDR output leaves this field unset.

## Recorded scenes and sound

Compiled FMTL recordings use the same HDR conversion as live native scenes:

```sh
fmn --hdr-pq --vcodec auto --video_dir media recording.fmtl
```

The same `--hdr-pq --vcodec auto` options apply to `fmn batch`. Each scene
records its own HDR profile in its completed-video report and manifest.

The recording keeps its authored frame schedule. Camera, vector, compiled,
and pure-animation jobs all pass their linear raster through the selected
color conversion, including frames whose raw pixels were reused from a cache.
Native scene soundtracks are mixed as usual; the final audio mux copies the
already-encoded video stream, preserving its HDR color information.

## Luminance and color

Working-space RGB uses linear sRGB/BT.709 primaries. The HDR converter transforms
those primaries into BT.2020 in linear light, assigns absolute luminance, applies
the SMPTE ST 2084 PQ transfer, and produces BT.2020 nonconstant-luminance Y′CbCr.
The encoder receives the same color description on both input and output.

Linear RGB `1.0` maps to 203 cd/m². Values above one preserve highlight detail
up to the 1,000 cd/m² clipping ceiling. That ceiling is a hard content limit;
the converter does not perform tone mapping. It does not invent mastering-display
or measured-content-light metadata. The Rust API accepts a validated
`HdrPqConfig::try_new(reference_white_nits, peak_nits)` for other luminance
settings. The CLI uses the default profile.

The numerical definitions follow [ITU-R BT.2100-3](https://www.itu.int/rec/R-REC-BT.2100-3-202502-I/en).
The D65 primary transformation uses the rational matrices in
[CSS Color 4](https://www.w3.org/TR/css-color-4/#color-conversion-code).

Without `--hdr-pq`, existing SDR behavior is unchanged, including explicit
P010 output. Ordinary P010 remains a ten-bit SDR encoding.

## Supported combinations and receipts

HDR requires an opaque background, CPU rendering, standard determinism, and
identity movie gamma and saturation. PNG, GIF, Y4M, WAV, SVG, transparency,
certified output, accelerator rendering, and Studio previews reject the flag.
An explicitly conflicting `--pix_fmt` is refused; `auto` and the P010 aliases
are accepted. The Python console validates requested formats, transparency,
and explicit pixel formats before importing the scene source. Writer gamma,
saturation, and native encoder capabilities are checked before an encoder is
opened. API callers receive the same native validation.

With `--robot`, a completed video report includes `hdr_pq` with the primaries,
transfer, matrix, range, reference-white luminance, and peak luminance. The
native CLI's published manifest includes `native-hdr-pq.json` in the configuration input
closure. FFmpeg invocation receipts contain the actual wire format, encoder,
and color flags. These records distinguish native luminance conversion from
encoder settings and remain present when the video contains a soundtrack.
