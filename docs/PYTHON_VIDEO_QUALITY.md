# Python video quality through the native encoder

The Python portal reads four optional attributes from `scene.file_writer`
before it starts an MP4/MOV generation: `video_crf`, `video_preset`,
`video_tune`, and `video_bitrate`. They become the same typed `VideoQuality`
used by the Rust facade. There is no process-runner decorator, raw encoder
argument string, or separate Python encoder implementation.

```python
from manimlib import Scene, Square
from fmn_python import render_scene

class Picture(Scene):
    def construct(self):
        self.add(Square())
        self.wait(1)

scene = Picture()
scene.file_writer.video_crf = 16
scene.file_writer.video_preset = "slow"
scene.file_writer.video_tune = "animation"
render_scene(scene, "picture.mp4", resolution=(1920, 1080), fps=60)
```

Install these settings before rendering begins. Missing attributes and `None`
request the previous encoder defaults. CRF zero is a real request, not a false
value to discard. `video_bitrate` is an integer **bits-per-second target**, not
a strict file-size ceiling, and cannot be combined with CRF. The shared
[quality policy](VIDEO_ENCODING_QUALITY.md) validates the selected encoder,
container, preset and tune; software-only controls on hardware encoders fail
by name. Strings are exact catalog names, never argv fragments. Booleans and
lossy float-to-integer conversions are refused.

The adapter covers ordinary portal generations and recorded/subdivided video
exports, which both use `PortalVideoConfig`. It preserves the ownership checks
around Python getters and propagates authored getter/conversion failures.
Every successful video receipt's `ffmpeg_invocations` contains the actual
resolved encoder and complete arguments. Video remains outside certification.

The permanent `portal_video_quality_acceptance_suite` runs the actual extension,
exercises admission before executable discovery, checks descriptor errors and
existing-output preservation, and renders MP4/MOV at two CRFs. Its independent
bitstream check demuxes H.264 with stream copy and reads x264 user-data SEI;
it does not infer settings from our requested-option receipt alone. Real encode
cases need ffmpeg and become mandatory under `FMN_REQUIRE_FFMPEG=1` or
`FMN_REQUIRE_FULL_INPUTS=1`. Syntax/isolated parser checks are not extension proof.

This does not yet configure AAC bitrate or expose these controls in standalone
Rust `fmn` CLI/config files. No encoder default or visual-quality claim changes.
