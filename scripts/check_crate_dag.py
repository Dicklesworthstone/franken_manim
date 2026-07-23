#!/usr/bin/env python3
"""Hold the workspace crate graph to the plan's §19 DAG.

Compares `cargo metadata`'s workspace-internal dependency edges against the
expected adjacency below (the single in-repo source of truth for the crate
map). Fails on ANY drift: a missing crate, an extra crate, a missing edge,
an extra edge, or an edge that points upward in the layering. Run by
scripts/check.sh and therefore by CI.

Stdlib only; requires cargo on PATH.
"""

import json
import subprocess
import sys

# §19 crate map. Order is the layering: a crate may depend only on crates
# that appear EARLIER in this dict (strictly-downward edges, cycle-free by
# construction). Keep in lockstep with COMPREHENSIVE_PLAN §19.
EXPECTED: dict[str, set[str]] = {
    "fmn-core": set(),
    "fmn-dmath": {"fmn-core"},
    "fmn-hash": {"fmn-core"},
    "fmn-config": {"fmn-core", "fmn-hash"},
    "fmn-platform": {"fmn-core"},
    "fmn-frame": {"fmn-core", "fmn-dmath"},  # dmath: deterministic transfer functions (D-17, fm-a25)
    "fmn-codec": {"fmn-core", "fmn-frame", "fmn-hash"},
    "fmn-cache": {"fmn-core", "fmn-hash", "fmn-platform"},
    "fmn-geom": {"fmn-core", "fmn-dmath"},
    "fmn-mobject": {"fmn-core", "fmn-geom", "fmn-hash"},
    "fmn-anim": {"fmn-core", "fmn-dmath", "fmn-mobject"},
    "fmn-render": {"fmn-core", "fmn-dmath", "fmn-geom", "fmn-mobject", "fmn-frame", "fmn-hash", "fmn-cache"},
    "fmn-text": {"fmn-core", "fmn-geom", "fmn-mobject"},
    "fmn-tex": {"fmn-core", "fmn-config", "fmn-mobject", "fmn-text", "fmn-cache"},
    "fmn-library": {"fmn-core", "fmn-geom", "fmn-mobject", "fmn-anim", "fmn-text", "fmn-tex"},
    "fmn-scene": {"fmn-core", "fmn-config", "fmn-platform", "fmn-mobject", "fmn-anim", "fmn-render", "fmn-hash"},  # hash: journal serialization + digests (§13.4, fm-y7u)
    "fmn-studio": {"fmn-core", "fmn-platform", "fmn-frame", "fmn-codec", "fmn-render", "fmn-scene"},
    "fmn-output": {"fmn-core", "fmn-hash", "fmn-platform", "fmn-frame", "fmn-codec", "fmn-cache"},
    "fmn-runtime": {"fmn-core", "fmn-platform"},
    "fmn-cli": {"fmn-core", "fmn-config", "fmn-platform", "fmn-runtime", "fmn-scene", "fmn-studio", "fmn-output", "fmn-library"},
    "fmn-conformance": {"fmn-core", "fmn-hash", "fmn-geom", "fmn-mobject", "fmn-anim", "fmn-render", "fmn-library", "fmn-scene", "fmn-output"},
    "fmn-python": {"fmn-core", "fmn-config", "fmn-mobject", "fmn-anim", "fmn-library", "fmn-scene"},
}

LAYER = {name: i for i, name in enumerate(EXPECTED)}


def main() -> int:
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--format-version", "1", "--no-deps"],
            check=True, capture_output=True, text=True,
        ).stdout
    )
    workspace_ids = set(meta["workspace_members"])
    actual: dict[str, set[str]] = {}
    spikes: set[str] = set()
    for pkg in meta["packages"]:
        if pkg["id"] not in workspace_ids:
            continue
        deps = {d["name"] for d in pkg["dependencies"] if d["name"].startswith("fmn-")}
        # G0 spikes (spikes/, fmn-spike-*) are sanctioned prototype crates
        # outside the §19 map (§20.1). They are exempt from the map itself,
        # but no §19 crate may ever depend on one (checked below).
        if pkg["name"].startswith("fmn-spike-"):
            spikes.add(pkg["name"])
            continue
        actual[pkg["name"]] = deps

    errors: list[str] = []
    for name, deps in sorted(actual.items()):
        for dep in sorted(deps & spikes):
            errors.append(f"{name}: production crate depends on spike {dep}")
    for missing in EXPECTED.keys() - actual.keys():
        errors.append(f"crate missing from workspace: {missing}")
    for extra in actual.keys() - EXPECTED.keys():
        errors.append(f"crate not in plan §19: {extra}")

    for name in sorted(EXPECTED.keys() & actual.keys(), key=LAYER.__getitem__):
        want, got = EXPECTED[name], actual[name]
        for e in sorted(want - got):
            errors.append(f"{name}: missing declared dependency on {e}")
        for e in sorted(got - want):
            errors.append(f"{name}: undeclared-in-plan dependency on {e}")
        for e in sorted(got & want):
            if LAYER[e] >= LAYER[name]:
                errors.append(f"{name}: edge to {e} points upward in the layering")

    if errors:
        print("crate-DAG check FAILED:", file=sys.stderr)
        for e in errors:
            print(f"  - {e}", file=sys.stderr)
        return 1
    print(f"crate-DAG check OK: {len(actual)} crates match plan §19, all edges downward")
    return 0


if __name__ == "__main__":
    sys.exit(main())
