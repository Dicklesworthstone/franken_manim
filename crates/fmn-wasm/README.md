# fmn-wasm

`fmn-wasm` is FrankenManim's single-threaded browser package. It exposes:

- `FmnScene`, which constructs one of the fixed primitive-corpus scenes and
  renders captured frames through Lumen to RGBA8 pixels;
- `FmnPlayer`, which loads an FMTL/1 serialized timeline, seeks without replay,
  and renders it through the same path; and
- `engine_version()`, the engine/package version compiled into the module.

The npm package is a bundler-targeted ES module. A typical consumer is:

```js
import { FmnScene, engine_version } from "fmn-wasm";

console.log(`FrankenManim ${engine_version()}`);
const scene = new FmnScene("circle_shift", 640, 360);
const pixels = new Uint8Array(scene.width * scene.height * 4);
scene.render_into(0, pixels);
context.putImageData(
  new ImageData(new Uint8ClampedArray(pixels.buffer), scene.width, scene.height),
  0,
  0,
);
```

`render_into` reuses caller-owned JS storage, avoiding a new returned
`Uint8Array` on every frame. The wasm-bindgen boundary still copies between JS
and WebAssembly memory; this API does not claim a zero-copy JS/WASM transfer.

FMTL/1 bundles carry an engine identity and are rejected before use when that
identity does not match the player. The npm package version is kept in lockstep
with the Rust workspace version, and the release staging metadata records the
exact source commit.

The package is self-contained: it makes no CDN requests and carries the engine
license, the bundled-font manifest, and all corresponding license texts. It is
standard-mode only and is not part of FrankenManim's certified platform matrix.

Only the single-threaded package ships today. A threads build requires a
separate atomics/shared-memory artifact plus cross-origin isolation
(`Cross-Origin-Opener-Policy: same-origin` and
`Cross-Origin-Embedder-Policy: require-corp`); no such variant is claimed by
this release.

Repository build, browser-smoke, size-budget, and npm dry-run instructions live
in [`demo/wasm/README.md`](../../demo/wasm/README.md).
