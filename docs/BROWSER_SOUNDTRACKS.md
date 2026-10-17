# Self-contained browser soundtracks

`FmnPlayer.from_bundle_with_audio(bytes)` accepts sound-bearing FMTL/1 minor-2
artifacts through the production decoder: engine identity, nested pictures,
camera captures, normalized PCM and input limits remain authoritative. It does
not execute scene code or reopen source audio.

The ordinary `FmnPlayer.from_bundle(bytes)` remains picture-only and refuses
sound at load. The audio-aware constructor still blocks rendering while sound
is attached. `player.take_soundtrack()` transfers it once and authorizes picture
reconstruction. Reading `has_soundtrack` alone does not authorize silent playback.

The returned `FmnSoundtrack` exposes `sample_rate`, `channels`, `sample_frames`
and `duration_seconds`. `copy_channel_into(channel, start_frame, destination)`
copies a sample-frame window into a Float32Array without mixing, gain,
resampling or quantization. Mono is channel zero; stereo is left then right.
Invalid channels or ranges leave the destination unchanged. Call `free()` after
copying to release Wasm PCM. The host then owns scheduling and storage. The
bridge marshals the supplied slice; this is bounded copying, not a zero-copy
memory view. Chunked transfers bound the bridge's scratch buffer.

Generate a moving-geometry recording with a stereo tone:

```sh
cargo run -p fmn-wasm --example export_audio_bundle -- /tmp/audible.fmtl
```

The example refuses an existing destination. Its 1.75-second audio tail extends
past the 1.5-second picture grid; an audio host should hold the final picture
instead of dropping the tail. No source-media dependency is stored in the file.

The CPU renderer and recorded frame indices are unchanged. Browser/device audio
resampling and scheduling are presentation behavior, not certified PCM or
encoded-output determinism. The Rust tests compare exact channel samples,
original camera/planar pixels and invalid-copy destination preservation.
