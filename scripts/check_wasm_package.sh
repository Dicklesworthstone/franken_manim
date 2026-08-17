#!/usr/bin/env bash
# Build and consume the actual fmn-wasm npm artifact. This is the W11 release
# gate: wasm-pack -> npm tarball/dry-run -> fresh consumer -> webpack -> Chrome.
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo_root"

wasm_pack_bin=${FMN_WASM_PACK:-wasm-pack}
webpack_bin=${FMN_WEBPACK:-webpack}
chrome_bin=${FMN_CHROME:-google-chrome}

for command_name in "$wasm_pack_bin" "$webpack_bin" "$chrome_bin" npm python3 rustup git gzip; do
    if ! command -v "$command_name" >/dev/null 2>&1; then
        echo "ERROR: required WASM package-gate tool is unavailable: $command_name" >&2
        exit 1
    fi
done

if [[ -n "${FMN_NODE:-}" ]]; then
    node_bin=$FMN_NODE
elif node --version >/dev/null 2>&1; then
    node_bin=$(command -v node)
elif [[ -x /usr/bin/node ]] && /usr/bin/node --version >/dev/null 2>&1; then
    # This host intentionally puts Bun's limited `node` compatibility shim
    # first. npm itself needs a real Node runtime.
    node_bin=/usr/bin/node
else
    echo "ERROR: npm/webpack require a real Node.js runtime (set FMN_NODE)" >&2
    exit 1
fi
node_bin=$(command -v "$node_bin")
js_path="$(dirname "$node_bin"):$PATH"
node_path=${FMN_NODE_PATH:-${NODE_PATH:-}}
npm_bin=$(command -v npm)
webpack_bin=$(command -v "$webpack_bin")
node_version=$("$node_bin" --version)
npm_version=$(PATH="$js_path" "$npm_bin" --version)
chrome_version=$($chrome_bin --version)

wasm_pack_version=$($wasm_pack_bin --version)
if [[ "$wasm_pack_version" != "wasm-pack 0.15.0" ]]; then
    echo "ERROR: fmn-wasm release packaging requires wasm-pack 0.15.0; found: $wasm_pack_version" >&2
    exit 1
fi
webpack_version=$(PATH="$js_path" NODE_PATH="$node_path" "$node_bin" -p 'require("webpack/package.json").version')
webpack_cli_version=$(PATH="$js_path" NODE_PATH="$node_path" "$node_bin" -p 'require("webpack-cli/package.json").version')
if [[ "$webpack_version" != "5.109.2" || "$webpack_cli_version" != "7.2.2" ]]; then
    echo "ERROR: browser consumer requires webpack 5.109.2 + webpack-cli 7.2.2; found $webpack_version + $webpack_cli_version" >&2
    exit 1
fi

workspace_version=$(python3 - <<'PY'
import tomllib
with open("Cargo.toml", "rb") as source:
    print(tomllib.load(source)["workspace"]["package"]["version"])
PY
)
source_commit=$(git rev-parse HEAD)
source_dirty=false
if ! git diff --quiet || ! git diff --cached --quiet; then
    source_dirty=true
    if [[ "${FMN_WASM_PACKAGE_ALLOW_DIRTY:-0}" != "1" ]]; then
        echo "ERROR: tracked source is dirty; commit it or use FMN_WASM_PACKAGE_ALLOW_DIRTY=1 for a non-release diagnostic" >&2
        exit 1
    fi
fi

if [[ -n "${FMN_WASM_PACKAGE_ROOT:-}" ]]; then
    task_root=$FMN_WASM_PACKAGE_ROOT
    if [[ -e "$task_root" ]]; then
        echo "ERROR: FMN_WASM_PACKAGE_ROOT must name a new path: $task_root" >&2
        exit 1
    fi
    mkdir -p "$task_root"
else
    task_root=$(mktemp -d "${TMPDIR:-/tmp}/fmn-wasm-package.XXXXXX")
fi
package_dir=$task_root/package
consumer_dir=$task_root/consumer
mkdir -p "$consumer_dir"

echo "==> fmn-wasm package evidence root: $task_root"
echo "==> wasm-pack bundler artifact ($wasm_pack_version)"
# The RCH fleet does not carry wasm32-unknown-unknown. Resolve the pinned local
# toolchain directly so this required host artifact cannot silently fall back
# from, or be misreported as, a remote build.
real_cargo=$(rustup which cargo)
PATH="$(dirname "$real_cargo"):$PATH" \
    "$wasm_pack_bin" build --target bundler --release --out-dir "$package_dir" crates/fmn-wasm

for required in fmn_wasm.js fmn_wasm_bg.js fmn_wasm_bg.wasm \
    fmn_wasm.d.ts fmn_wasm_bg.wasm.d.ts package.json README.md; do
    if [[ ! -f "$package_dir/$required" ]]; then
        echo "ERROR: wasm-pack omitted required package member: $required" >&2
        exit 1
    fi
done

if [[ -e "$package_dir/LICENSE" || -e "$package_dir/FONT_BUNDLE.json" || -e "$package_dir/licenses" ]]; then
    echo "ERROR: wasm-pack unexpectedly populated a governed license path" >&2
    exit 1
fi
cp LICENSE "$package_dir/LICENSE"
cp dist/FONT_BUNDLE.json "$package_dir/FONT_BUNDLE.json"
cp -R dist/licenses "$package_dir/licenses"

python3 - "$package_dir/package.json" "$workspace_version" "$source_commit" "$source_dirty" <<'PY'
import json
import sys

path, version, commit, dirty = sys.argv[1:]
with open(path, encoding="utf-8") as source:
    package = json.load(source)
if package.get("version") != version:
    raise SystemExit(f"generated npm version {package.get('version')!r} != workspace {version!r}")
package["files"] = [
    "fmn_wasm_bg.wasm",
    "fmn_wasm.js",
    "fmn_wasm_bg.js",
    "fmn_wasm.d.ts",
    "fmn_wasm_bg.wasm.d.ts",
    "README.md",
    "LICENSE",
    "FONT_BUNDLE.json",
    "licenses/",
]
package["exports"] = {
    ".": {"types": "./fmn_wasm.d.ts", "import": "./fmn_wasm.js"}
}
package["publishConfig"] = {"access": "public"}
package["frankenManim"] = {
    "engineVersion": version,
    "sourceCommit": commit,
    "sourceDirty": dirty == "true",
    "timelineSchema": "FMTL/1",
    "threading": "single",
    "certified": False,
}
with open(path, "w", encoding="utf-8", newline="\n") as output:
    json.dump(package, output, indent=2, ensure_ascii=False)
    output.write("\n")
PY

if ! grep -q 'engine_version' "$package_dir/fmn_wasm.d.ts"; then
    echo "ERROR: generated TypeScript surface omitted engine_version()" >&2
    exit 1
fi

budget_for() {
    awk -F '\t' -v artifact="$1" '$1 == artifact { print $3; found = 1 } END { if (!found) exit 1 }' \
        crates/fmn-wasm/SIZE_BUDGET.tsv
}

wasm_path=$package_dir/fmn_wasm_bg.wasm
raw_bytes=$(stat -c '%s' "$wasm_path")
raw_budget=$(budget_for wasm-bindgen-bundler-pkg)
if (( raw_bytes > raw_budget )); then
    echo "ERROR: bundler wasm is $raw_bytes bytes, over the $raw_budget-byte budget" >&2
    exit 1
fi
gzip_path=$task_root/fmn_wasm_bg.wasm.gz
gzip -n -9 -c "$wasm_path" > "$gzip_path"
gzip_bytes=$(stat -c '%s' "$gzip_path")
gzip_budget=$(budget_for wasm-bindgen-bundler-pkg-gzip)
if (( gzip_bytes > gzip_budget )); then
    echo "ERROR: gzip bundler wasm is $gzip_bytes bytes, over the $gzip_budget-byte budget" >&2
    exit 1
fi

echo "==> npm pack + publish dry-run"
PATH="$js_path" "$npm_bin" pack "$package_dir" --ignore-scripts --json --pack-destination "$task_root" \
    > "$task_root/npm-pack.json"
(cd "$package_dir" && PATH="$js_path" "$npm_bin" publish --dry-run --ignore-scripts --json) \
    > "$task_root/npm-publish-dry-run.json"

tarball_name=$(python3 - "$task_root/npm-pack.json" "$task_root/npm-publish-dry-run.json" <<'PY'
import json
import os
import sys

pack_path, publish_path = sys.argv[1:]
with open(pack_path, encoding="utf-8") as source:
    pack = json.load(source)
with open(publish_path, encoding="utf-8") as source:
    publish = json.load(source)
pack = pack[0] if isinstance(pack, list) else pack
publish = publish[0] if isinstance(publish, list) else publish

expected = {
    "package.json",
    "README.md",
    "LICENSE",
    "FONT_BUNDLE.json",
    "fmn_wasm.js",
    "fmn_wasm_bg.js",
    "fmn_wasm_bg.wasm",
    "fmn_wasm.d.ts",
    "fmn_wasm_bg.wasm.d.ts",
}
for root, _, files in os.walk("dist/licenses"):
    for filename in files:
        relative = os.path.relpath(os.path.join(root, filename), "dist")
        expected.add(relative.replace(os.sep, "/"))

def inventory(document, label):
    actual = {member["path"] for member in document.get("files", [])}
    if actual != expected:
        missing = sorted(expected - actual)
        extra = sorted(actual - expected)
        raise SystemExit(f"{label} inventory mismatch; missing={missing}, extra={extra}")

inventory(pack, "npm pack")
inventory(publish, "npm publish --dry-run")
print(pack["filename"])
PY
)
tarball=$task_root/$tarball_name
if [[ ! -f "$tarball" ]]; then
    echo "ERROR: npm pack receipt names a missing tarball: $tarball" >&2
    exit 1
fi
npm_tarball_bytes=$(stat -c '%s' "$tarball")
npm_tarball_budget=$(budget_for npm-package-tarball)
if (( npm_tarball_bytes > npm_tarball_budget )); then
    echo "ERROR: npm tarball is $npm_tarball_bytes bytes, over the $npm_tarball_budget-byte budget" >&2
    exit 1
fi

echo "==> fresh npm consumer + webpack bundle"
(cd "$consumer_dir" && PATH="$js_path" "$npm_bin" init --yes >/dev/null)
(cd "$consumer_dir" && PATH="$js_path" "$npm_bin" install --ignore-scripts --no-audit --no-fund --no-package-lock "$tarball")
cp demo/wasm/smoke.mjs "$consumer_dir/smoke.mjs"
cp demo/wasm/webpack.config.cjs "$consumer_dir/webpack.config.cjs"
mkdir -p "$consumer_dir/dist"
(cd "$consumer_dir" && PATH="$js_path" NODE_PATH="$node_path" "$webpack_bin" --config webpack.config.cjs)
cp demo/wasm/smoke.html "$consumer_dir/dist/smoke.html"
cp demo/wasm/bundle.fmtl "$consumer_dir/dist/bundle.fmtl"

echo "==> real Chromium package smoke"
server_log=$task_root/http-server.log
python3 -u -m http.server 0 --bind 127.0.0.1 --directory "$consumer_dir/dist" \
    > "$server_log" 2>&1 &
server_pid=$!
stop_server() {
    if kill -0 "$server_pid" 2>/dev/null; then
        kill "$server_pid"
        wait "$server_pid" 2>/dev/null || true
    fi
}
trap stop_server EXIT

port=
for _ in $(seq 1 100); do
    port=$(sed -n 's/.* port \([0-9][0-9]*\) .*/\1/p' "$server_log" | head -n 1)
    if [[ -n "$port" ]]; then
        break
    fi
    if ! kill -0 "$server_pid" 2>/dev/null; then
        echo "ERROR: HTTP server exited before reporting its port" >&2
        cat "$server_log" >&2
        exit 1
    fi
    sleep 0.1
done
if [[ -z "$port" ]]; then
    echo "ERROR: HTTP server did not report a port" >&2
    exit 1
fi

chrome_profile=$task_root/chrome-profile
chrome_dom=$task_root/chrome-dom.html
chrome_log=$task_root/chrome.log
mkdir -p "$chrome_profile"
"$chrome_bin" --headless=new --disable-gpu --disable-dev-shm-usage \
    --no-first-run --no-default-browser-check --user-data-dir="$chrome_profile" \
    --virtual-time-budget=30000 --dump-dom \
    "http://127.0.0.1:$port/smoke.html?version=$workspace_version" \
    > "$chrome_dom" 2> "$chrome_log"
stop_server
trap - EXIT

if ! grep -q 'data-status="success"' "$chrome_dom"; then
    echo "ERROR: packaged browser smoke did not report success" >&2
    cat "$chrome_dom" >&2
    cat "$chrome_log" >&2
    exit 1
fi

python3 - "$task_root" "$workspace_version" "$source_commit" "$source_dirty" \
    "$raw_bytes" "$gzip_bytes" "$wasm_pack_version" "$webpack_version" \
    "$webpack_cli_version" "$node_version" "$npm_version" "$chrome_version" \
    "$tarball" "$chrome_dom" <<'PY'
import hashlib
import html
import json
import re
import sys

(root, version, commit, dirty, raw_bytes, gzip_bytes, wasm_pack, webpack,
 webpack_cli, node, npm, chrome, tarball, dom_path) = sys.argv[1:]
with open(tarball, "rb") as source:
    tarball_bytes = source.read()
with open(dom_path, encoding="utf-8") as source:
    dom = source.read()
match = re.search(r'<pre id="result" data-status="success">(.*?)</pre>', dom, re.S)
if match is None:
    match = re.search(r'<pre data-status="success" id="result">(.*?)</pre>', dom, re.S)
if match is None:
    raise SystemExit("success marker exists but result payload is missing")
browser = json.loads(html.unescape(match.group(1)))
receipt = {
    "schema": "fmn-wasm-package-receipt/1",
    "version": version,
    "source_commit": commit,
    "source_dirty": dirty == "true",
    "wasm_pack": wasm_pack,
    "webpack": webpack,
    "webpack_cli": webpack_cli,
    "node": node,
    "npm": npm,
    "chrome": chrome,
    "wasm_raw_bytes": int(raw_bytes),
    "wasm_gzip_bytes": int(gzip_bytes),
    "npm_tarball": {
        "path": tarball,
        "bytes": len(tarball_bytes),
        "sha256": hashlib.sha256(tarball_bytes).hexdigest(),
    },
    "browser": browser,
}
with open(f"{root}/receipt.json", "w", encoding="utf-8", newline="\n") as output:
    json.dump(receipt, output, indent=2, sort_keys=True)
    output.write("\n")
print(json.dumps(receipt, sort_keys=True))
PY

echo "OK: fmn-wasm npm package gate green; evidence preserved at $task_root"
