//! The embedded Studio UI assets (§13.5): compiled into the binary, versioned
//! with it, never served from the filesystem.
//!
//! The Studio's browser UI is exactly what this module compiles in — a static
//! script and an index-shell template. The HTTP host ([`crate::host`]) serves
//! these bytes at exact routes and has no mechanism to read UI files at
//! runtime: there is no path from a request to a filesystem open. An asset
//! change is a source change, which means it is reviewed, versioned, and
//! content-addressed like any other code.
//!
//! # Version coupling
//!
//! [`STUDIO_UI_VERSION`] is the fmn-studio crate version, baked in at compile
//! time: the UI asset set is versioned **with the binary** by construction —
//! a UI change without a crate-version change cannot ship through the release
//! pipeline, because the served `X-FMN-Studio-Version` header and the
//! version meta tag would fail the release's own hash checks. The host stamps
//! every UI response with that header so a connected browser can detect a
//! stale page against a restarted Studio.

/// The UI asset set's version — exactly the fmn-studio crate version.
///
/// Because the assets are compiled in, this is also the version of every byte
/// the Studio's UI routes can serve. `env!("CARGO_PKG_VERSION")` is a
/// compile-time constant, so the coupling cannot drift at runtime.
pub const STUDIO_UI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The response header that stamps every served UI asset with
/// [`STUDIO_UI_VERSION`].
pub const STUDIO_UI_VERSION_HEADER: &str = "X-FMN-Studio-Version";

/// One embedded UI asset: an exact route, its content type, and its bytes.
#[derive(Clone, Copy, Debug)]
pub struct UiAsset {
    /// The exact request path this asset serves at.
    pub route: &'static str,
    /// The response `Content-Type`.
    pub content_type: &'static str,
    /// The complete response body, compiled into the binary.
    pub bytes: &'static [u8],
}

/// The Studio browser script, embedded verbatim.
const STUDIO_JS: &str = r#""use strict";
const query = new URLSearchParams(window.location.search);
const capability = query.get("cap");
if (!capability) throw new Error("missing Studio capability");
const headers = {"X-FMN-Capability": capability};
document.getElementById("preview").src =
  "/stream?cap=" + encodeURIComponent(capability);
document.getElementById("inspect").addEventListener("click", async () => {
  const response = await fetch("/api/inspect", {headers});
  document.getElementById("result").textContent = await response.text();
});
document.getElementById("seek").addEventListener("click", async () => {
  const frame = document.getElementById("frame").value;
  const response = await fetch("/api/scrub", {
    method: "POST",
    headers: {...headers, "Content-Type": "application/x-www-form-urlencoded"},
    body: "frame=" + encodeURIComponent(frame) + "&commit=true"
  });
  document.getElementById("result").textContent = await response.text();
});
"#;

/// The index shell template. The capability placeholder is substituted by
/// [`studio_index_html`]; the UI version meta tag is baked in at compile time
/// via [`STUDIO_UI_VERSION`].
const STUDIO_INDEX_HTML: &str = concat!(
    "<!doctype html><meta charset=\"utf-8\">",
    "<meta name=\"referrer\" content=\"no-referrer\">",
    "<meta name=\"fmn-studio-ui-version\" content=\"",
    env!("CARGO_PKG_VERSION"),
    "\">",
    "<title>FrankenManim Studio</title><h1>FrankenManim Studio</h1>",
    "<img id=\"preview\" alt=\"Live preview\">",
    "<p><input id=\"frame\" type=\"number\" min=\"0\" value=\"0\">",
    "<button id=\"seek\">Seek</button><button id=\"inspect\">Inspect</button></p>",
    "<pre id=\"result\"></pre>",
    "<script src=\"/studio.js?cap=__FMN_CAPABILITY__\"></script>"
);

const CAPABILITY_PLACEHOLDER: &str = "__FMN_CAPABILITY__";

/// The static embedded UI asset set, in route order.
///
/// The index shell is not in this table: it is per-session (it carries the
/// capability token) and is produced by [`studio_index_html`].
static UI_ASSETS: &[UiAsset] = &[UiAsset {
    route: "/studio.js",
    content_type: "text/javascript; charset=utf-8",
    bytes: STUDIO_JS.as_bytes(),
}];

/// The complete embedded UI asset set.
pub fn ui_assets() -> &'static [UiAsset] {
    UI_ASSETS
}

/// Look up the embedded asset for an exact request path.
#[must_use]
pub fn ui_asset(route: &str) -> Option<&'static UiAsset> {
    UI_ASSETS.iter().find(|asset| asset.route == route)
}

/// Render the index shell for one session's capability token (hex form).
///
/// The token appears only inside the script URL's `cap` query, exactly as
/// [`CapabilityToken::expose_hex`](crate::CapabilityToken::expose_hex)
/// formats it; the template performs no other substitution.
#[must_use]
pub fn studio_index_html(capability_hex: &str) -> String {
    STUDIO_INDEX_HTML.replace(CAPABILITY_PLACEHOLDER, capability_hex)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_the_crate_version_and_the_template_is_baked() {
        assert_eq!(STUDIO_UI_VERSION, env!("CARGO_PKG_VERSION"));
        assert!(STUDIO_INDEX_HTML.contains(&format!(
            "name=\"fmn-studio-ui-version\" content=\"{STUDIO_UI_VERSION}\""
        )));
        assert_eq!(STUDIO_INDEX_HTML.matches(CAPABILITY_PLACEHOLDER).count(), 1);
        let rendered = studio_index_html("abc123");
        assert!(!rendered.contains(CAPABILITY_PLACEHOLDER));
        assert!(rendered.contains("/studio.js?cap=abc123"));
    }

    #[test]
    fn asset_routes_are_exact_and_unique() {
        let assets = ui_assets();
        assert!(!assets.is_empty());
        for asset in assets {
            assert!(asset.route.starts_with('/'));
            assert!(!asset.bytes.is_empty());
            assert_eq!(
                assets
                    .iter()
                    .filter(|other| other.route == asset.route)
                    .count(),
                1,
                "duplicate route {}",
                asset.route
            );
            assert_eq!(ui_asset(asset.route).map(|a| a.bytes), Some(asset.bytes));
        }
        assert!(ui_asset("/").is_none(), "the index shell is per-session");
        assert!(ui_asset("/studio.js/").is_none(), "no prefix matching");
        assert!(ui_asset("/etc/passwd").is_none());
    }
}
