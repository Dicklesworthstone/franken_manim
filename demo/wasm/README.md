# W5 wasm tier-1 browser demo (fm-l97, §10.7)

The `fmn-wasm` crate compiled to `wasm32-unknown-unknown`, rendering the
fixed primitive-corpus scenes (`circle_shift`, `parametric_wave`,
`orbit_duet`) to a `<canvas>` via `ImageData`. Single-threaded; wasm is not
in the certified matrix, but standard-mode determinism holds (same scene,
same seed, same build ⇒ byte-identical RGBA8; every transcendental comes
from `fmn-dmath`, ADR-0014).

This directory is the demo's one home (`demo/wasm/`); there is no parallel
`examples/wasm/`.

## Build

`wasm-pack` 0.13.1 is installed on this machine and is the documented path
(it vendors its own pinned `wasm-bindgen` CLI + `wasm-opt`):

```sh
wasm-pack build --target web --out-dir ../../demo/wasm/pkg crates/fmn-wasm
```

If `wasm-pack` is unavailable, the equivalent two-step path is (requires
`wasm-bindgen-cli` 0.2.126, `cargo install wasm-bindgen-cli --version 0.2.126`
— installing it is a deliberate local action, not part of this repo):

```sh
cargo build --target wasm32-unknown-unknown --release -p fmn-wasm
wasm-bindgen --target web --out-dir demo/wasm/pkg \
    target/wasm32-unknown-unknown/release/fmn_wasm.wasm
```

(`demo/wasm/pkg/` is a build artifact; wasm-pack's generated `.gitignore`
keeps it out of version control.)

## Run

Any static file server works; ES modules require http(s), not `file://`:

```sh
python3 -m http.server 8080 --directory demo/wasm
# open http://localhost:8080/
```

The page constructs a scene, then scrubs/plays captured frames through the
zero-copy `render_into` path into `ImageData`.

## Tier 2: the FMTL/1 timeline player (fm-oee)

`player.html` is the tier-2 demo: it consumes a serialized timeline bundle
(`docs/FMNT1_TIMELINE_BUNDLE.md`) with `FmnPlayer` — no scene code in the
browser — and scrubs/seeks it (slider, play, and one button per authored
label). Pure segments reconstruct each frame from begin/end snapshots
through the contract's record-lerp law; stateful segments restore recorded
per-frame snapshots.

Export the bundle (deterministic; reruns rewrite identical bytes), then
serve as above and open `player.html`:

```sh
cargo run -p fmn-wasm --example export_bundle   # writes demo/wasm/bundle.fmtl
python3 -m http.server 8080 --directory demo/wasm
# open http://localhost:8080/player.html
```

The bundle's size is recorded with headroom in `SIZE_BUDGET.tsv`
(`demo-timeline-bundle` row), enforced by a host test.

## Artifact size (R19)

`crates/fmn-wasm/SIZE_BUDGET.tsv` records the measured sizes and the
enforced budget (measured + 10% headroom); a host test in the crate
(`size_budget_within_headroom`) fails if a rebuilt artifact exceeds it.
