# Exact windows over portable soundtracks

`BundleSoundtrack::sample_window(start, frames, max_pcm_bytes)` copies one
half-open interval of the original sample-frame grid. The window keeps the
stored sample rate, channel order and exact normalized Float32 bits. Source
samples beyond the end become silence. Only the requested window is allocated;
its elapsed prefix is not. Integer overflow and a destination above the smaller
of the explicit PCM limit and the 256 MiB production ceiling are refused before
allocation. A zero-length window needs no storage.

Picture coordinates are not sample coordinates. Consumers convert the absolute
start and end independently through `fmn_output::frames_to_samples`, then request
`sample_window(start_sample, end_sample - start_sample, budget)`. Rounding each
clip duration separately can create gaps when the frame rate does not divide the
sample rate. For example, boundaries 0, 1, 2, 3 at 3 fps become 0, 4, 7, 11 at
11 Hz: the correct window lengths are 4, 3, 4, not 4, 4, 4.

`BundleSoundtrack::pad_to(minimum_frames, max_pcm_bytes)` consumes an owned track,
retains any longer tail, and extends a shorter track with silence. When no growth
is needed, the original sample buffer moves unchanged. The complete result must
fit the PCM limit even when it was already long enough.

Native `render_bundle` now sends these already-mixed samples directly to the
existing video sink. It no longer converts the entire soundtrack to f64 and back
through a second mixer. The sink still owns its declared WAV quantization and
AAC encoding. Replay counters describe one stored track and zero DSP workers;
they do not reconstruct the original export's mixing provenance. Container
bytes, source clocks and picture/camera reconstruction are unchanged.

Analytic tests are in `fmn-scene/tests/soundtrack_windows.rs`. Existing native
soundtrack-bundle integration tests exercise real MP4/MOV publication and compare
the mux-input waveform with the original native mix.
