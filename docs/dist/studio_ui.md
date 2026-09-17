# Embedded Studio UI Assets (§13.5)

The Studio's browser UI is **compiled into the binary and versioned with it**.
There is no runtime file serving: no route reads the filesystem, no asset
directory exists at install time, and no request path can name a file.

## The mechanism

The asset set lives in `fmn_studio::ui` as compile-time constants:

- `ui_assets()` — the static asset table (`UiAsset { route, content_type,
  bytes }`). `/studio.js` and `/studio.css` are embedded verbatim with
  `include_str!`. Adding an asset is adding a table entry — an
  asset change is a source change, reviewed and versioned like any other code.
- `studio_index_html(capability_hex)` — the index shell. It is *not* a static
  asset because it carries the per-session capability token; the version meta
  tag in it is baked at compile time via `concat!` + `env!`.

`include_bytes!` is the mechanism of record should a binary asset (a font, an
icon) ever join the table: same compiled-in guarantee, same versioning. The
current assets are text and need no build script or asset pipeline.

## Version coupling

`fmn_studio::STUDIO_UI_VERSION` is `env!("CARGO_PKG_VERSION")` — the
fmn-studio crate version, a compile-time constant. Because the assets are
compiled in, the UI asset set shares the binary's crate version. The coupling
is observable:

- every UI response carries the `X-FMN-Studio-Version` header
  (`STUDIO_UI_VERSION_HEADER`), so a connected browser can detect a stale page
  against a restarted Studio;
- the index shell carries
  `<meta name="fmn-studio-ui-version" content="<version>">`.

## What the host serves

The host (`fmn_studio::host`) is a small loopback-only HTTP/1.1 server with an
exact-route table — not a general web framework. The UI routes:

| Route | Source | Stamping |
|---|---|---|
| `GET /` | `studio_index_html(token)` (per-session) | version header + meta tag |
| `GET /studio.js` | `ui_asset("/studio.js")` table bytes | version header |
| `GET /studio.css` | `ui_asset("/studio.css")` table bytes | version header |

Everything else under the UI surface is refused: unknown clean paths get 404
from the exact-route table; ambiguous paths (`..` components, percent escapes,
backslashes) get 400 at the request parser, before any routing. Dynamic
routes (`/stream`, `/api/scrub`, `/api/restart`, `/api/event`, `/api/inspect`,
`/api/overlays`) serve protocol data, never files.

Every request is authenticated with the per-session 256-bit capability
(header or `cap` query, never both), Host/Origin-validated, rate-limited, and
bounded (§13.5's security model); the socket is loopback-only and the session
expires.

## Browser controls

The timeline supports frame stepping, keyboard navigation, preview playback,
and scrubbing. Releasing a scrub commits the position used by worker replay;
temporary previews do not replace that committed position. Restart reports
the restored frame and replay outcome. Frame count and effective FPS come
from the worker, and the displayed time comes from the captured scene clock.

The keyboard-accessible family tree opens live records, uniforms, placement,
bounds, and UTF-8 source spans. Object IDs belong to one capture, so a frame
change clears the inspector's selection. Debug layers draw tiles, control points,
bounds, winding, and depth over the preview using the renderer's coordinate map.
The browser verifies multipart framing, publication order, PNG hashes and
dimensions before marking the inspector synchronized. Large trees and
overlays have explicit display limits.

Scene input is opt-in and enabled only when the worker advertises a live
adapter and revision. The shipped `@builtin interactive.v1` route now supplies
one over the existing native `InteractiveScene` and replay engine. Immutable
FMTL and other captured preview artifacts still report no live adapter and
return a typed refusal without disabling timeline controls. The capability is
removed from browser history after asset startup and redacted from errors.

## Edit the shipped native canvas

Build the current shipping executable with the pinned toolchain, or use a
binary built from these sources:

```bash
cargo run --locked -p fmn-cli --features cli,batch --bin fmn -- \
  studio --resolution 384x216 --threads 1 @builtin interactive.v1

# With the current executable installed:
fmn studio @builtin interactive.v1
```

Open the private loopback launch URL, enable **Forward scene input**, and focus
the preview canvas. This route has a blue circle nested in a group and a red
square to use as a color source. It needs neither CPython nor ffmpeg. Its native
windowed clock is 30 fps: frame zero is the constructed state, followed by 60
actual native wait frames. Offline skip/range/presenter flags are not applicable
to the live timeline and are rejected rather than reinterpreted.

| Control in the focused preview | Native editor operation |
|---|---|
| Ctrl/Command+T, then Ctrl/Command+click the circle | Switch selection scope to nested objects, then select the child |
| Hold G and drag; release G | Move the selection |
| Move the pointer away from the selection center, hold T, move radially, release T | Resize relative to the initial pointer distance |
| C, then click the red square | Pick its color for the selection |
| Ctrl/Command+C and Ctrl/Command+V | Copy and paste through the native clipboard |
| Ctrl/Command+Z; Ctrl/Command+Shift+Z | Undo; redo |
| Arrow keys | Nudge the native selection |

The family tree selects an **inspection entry**, not an editor target; use
native canvas selection for edits. Accepted input is committed at its original
frame in deterministic order. Scrubbing forward retains earlier edits;
returning before an edit shows the prior state. Editing the past replaces only
the later input branch. **Restart** recreates the native worker and verifies
re-executed commands against the recorded input/source and state hashes,
including the editor's selection and undo history.

The browser queues admitted key/pointer transitions rather than dropping
releases during a render. Pointer capture and focus-loss handling release held
edit modes. Before a seek or restart, previously admitted transitions and their
releases drain first. Queue limits refuse additional input explicitly, retaining
capacity for owned release events. A transport or revision error disables input
and discards unsent actions; it does not roll back actions already committed.

Every shipped HTTP event requires `worker_generation`, `frame`, and `revision`.
The inspector supplies the generation through `X-FMN-Worker-Generation` and the
revision through `view.input_revision`. Host generation checks and dispatch are
one serialized operation. Stale revisions, unsupported explicit targets,
malformed input, and coordinate/delta budget overflow cannot silently edit a
different worker. Capture-local inspector IDs are not durable target handles;
explicit HTTP `target` values are currently refused. Normal canvas input uses
the native hit-testing and selection owner instead.

This is a registered affine native editing surface, not arbitrary Rust-source
compilation, perspective picking, a Python Studio worker, or graphical edits
written back into source files. Journal/checkpoint authority is session-local:
worker restart is supported, but closing the entire Studio host does not save
an edit project for a later invocation. The reusable native worker and explicit
source-project host are described in `docs/NATIVE_STUDIO_WORKER.md` and
`docs/NATIVE_PROJECT_STUDIO.md`; their own capability boundaries still apply.
No warm-restart latency or full G3/certification claim follows from this route.

## The acceptance tests

`crates/fmn-studio/tests/ui.rs` drives the real socket:

- each served script and stylesheet is byte-identical to its embedded entry,
  with matching SHA-256 content hashes;
- all UI routes carry `X-FMN-Studio-Version: <crate version>` and the shell
  carries the matching meta tag, proving the version coupling end to end;
- unknown and traversal-shaped paths have no route (404/400), proving there is
  nothing to reach a filesystem with.

`fmn_studio::ui`'s unit tests pin the table itself: routes exact and unique,
assets non-empty, the version equal to the crate version.

The Gauntlet fast tier registers `lifecycle.studio_native_worker.v1` and the
positive `lifecycle.studio_native_interactive.v1`, using Cargo's exact shipping
CLI artifact to exercise the real supervisor, isolated worker, authenticated
sockets, decoded PNG pixels, inspector, guarded edits and checkpoint replay.

`node --test crates/fmn-studio/tests/input_queue.mjs` exercises the actual
shipped controller's queue with isolated DOM/transport fixtures: rapid gestures,
key release ordering, focus/capture loss, modifier/layout changes, bounded
admission, restart/seek ordering, and generation/revision refusal. This layer
does not substitute for native rendering or a real browser.

After building `fmn`, run the real Chrome acceptance driver:

```bash
node crates/fmn-studio/tests/browser.mjs /absolute/path/to/fmn /absolute/evidence-directory
```

Use Node with built-in WebSocket support. `FMN_CHROME` can select the installed
Chrome executable. The driver checks animated fields, nested TeX spans,
scrubbing, overlays, restart, keyboard operation, a 390-pixel viewport, and
request refusals. Its live-canvas scenario selects a nested child, drags,
resizes, recolors, copies/pastes, undoes, resumes playback, scrubs and restarts;
it compares inspected state and decoded rendered pixels. Existing immutable
input-refusal scenarios remain. Screenshots and a capability-redacted receipt
record the source state, executable hash and actual results.
