# Embedded Studio UI Assets (§13.5)

The Studio's browser UI is **compiled into the binary and versioned with it**.
There is no runtime file serving: no route reads the filesystem, no asset
directory exists at install time, and no request path can name a file.

## The mechanism

The asset set lives in `fmn_studio::ui` as compile-time constants:

- `ui_assets()` — the static asset table (`UiAsset { route, content_type,
  bytes }`). Today: `/studio.js`, the Studio browser script, embedded verbatim
  as a `&'static str` constant. Adding an asset is adding a table entry — an
  asset change is a source change, reviewed and versioned like any other code.
- `studio_index_html(capability_hex)` — the index shell. It is *not* a static
  asset because it carries the per-session capability token; the version meta
  tag in it is baked at compile time via `concat!` + `env!`.

`include_bytes!` is the mechanism of record should a binary asset (a font, an
icon) ever join the table: same compiled-in guarantee, same versioning. The
current assets are text, so plain string constants are the boring correct
form — no build script, no asset pipeline.

## Version coupling

`fmn_studio::STUDIO_UI_VERSION` is `env!("CARGO_PKG_VERSION")` — the
fmn-studio crate version, a compile-time constant. Because the assets are
compiled in, the UI asset set is versioned with the binary **by
construction**: a UI change without a crate-version change cannot ship through
the release pipeline. The coupling is observable, not just structural:

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

Everything else under the UI surface is refused: unknown clean paths get 404
from the exact-route table; ambiguous paths (`..` components, percent escapes,
backslashes) get 400 at the request parser, before any routing. Dynamic
routes (`/stream`, `/api/scrub`, `/api/event`, `/api/inspect`,
`/api/overlays`) serve protocol data, never files.

Every request is authenticated with the per-session 256-bit capability
(header or `cap` query, never both), Host/Origin-validated, rate-limited, and
bounded (§13.5's security model); the socket is loopback-only and the session
expires.

## The acceptance tests

`crates/fmn-studio/tests/ui.rs` drives the real socket:

- the served `/studio.js` body is byte-identical to the embedded table entry,
  with matching SHA-256 content hashes;
- both UI routes carry `X-FMN-Studio-Version: <crate version>` and the shell
  carries the matching meta tag, proving the version coupling end to end;
- unknown and traversal-shaped paths have no route (404/400), proving there is
  nothing to reach a filesystem with.

`fmn_studio::ui`'s unit tests pin the table itself: routes exact and unique,
assets non-empty, the version equal to the crate version.
