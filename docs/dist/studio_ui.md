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
change clears selection. Debug layers draw tiles, control points, bounds,
winding, and depth over the preview using the renderer's coordinate map.
The browser verifies multipart framing, publication order, PNG hashes and
dimensions before marking the inspector synchronized. Large trees and
overlays have explicit display limits.

Scene input is opt-in and enabled only when the worker advertises a live
adapter. Current native preview artifacts report no such adapter and return
a typed refusal; the page explains this without disabling timeline controls.
The capability is removed from browser history after asset startup and is
redacted from displayed errors.

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

The Gauntlet fast tier registers `lifecycle.studio_native_worker.v1`, using
Cargo's exact shipping CLI artifact to exercise the real supervisor, isolated
worker, authenticated sockets, PNG stream, inspector, and checkpoint replay.

After building `fmn` through RCH, run the real Chrome acceptance driver:

```bash
/usr/bin/node crates/fmn-studio/tests/browser.mjs /absolute/path/to/fmn /absolute/evidence-directory
```

`FMN_CHROME` can select the installed Chrome executable. The driver checks
animated fields, nested TeX spans, scrubbing, overlays, restart, keyboard
operation, a 390-pixel viewport, and request refusals. It writes screenshots
and a capability-redacted receipt containing the source state and binary hash.
