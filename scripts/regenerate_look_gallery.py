#!/usr/bin/env python3
"""Regenerate the Look Gallery from installed release artefacts (fm-5wq.50).

Every panel is an ordinary scene class under gallery/scenes/. The installed
portal (`fmn-python`) renders it as a certified final-state PNG, and the build
id comes from that render's provenance sidecar. With --reference-python, the
pinned Reference renders the same unedited source (`-s -w`, under xvfb) into
the private capture directory (plan §15.3). The capture is gitignored; its
SHA-256 goes into the manifest.

The manifest (crates/fmn-conformance/fixtures/look_gallery.tsv, format v2)
names the release under review once; every row carries the build id that
produced its render, so fmn_conformance::gallery::render_pairs refuses a panel
left over from another build. Advisory (agent) verdicts reset to
`unreviewed` whenever a render changes: a verdict judges pixels, and these are
new pixels. Owner verdicts live in look_gallery_owner_verdicts.tsv, bound to the
render digest they judged.

Usage:
  regenerate_look_gallery.py --fmn-python PATH [--reference-python PATH]
      [--reference-root scripts/manim_ref] [--resolution 1920x1080] [--panel ID ...]
Exit status: 0 when every requested panel rendered; 1 otherwise (no manifest
is written); 2 on bad arguments.
"""
from __future__ import annotations

import argparse
import datetime
import hashlib
import json
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
SCENES = ROOT / "gallery" / "scenes"
RENDERS = ROOT / "gallery" / "renders"
REFERENCE_CAPTURES_ROOT = ROOT / "gallery" / "reference_captures"
PROVENANCE = REFERENCE_CAPTURES_ROOT / "PROVENANCE.json"
# Regeneration captures live apart from the original capture set (capture_reference_imagery.py,
# PROVENANCE.json) one level up, so this tool can never overwrite it.
CAPTURES = REFERENCE_CAPTURES_ROOT / "panels"
MANIFEST = ROOT / "crates" / "fmn-conformance" / "fixtures" / "look_gallery.tsv"
HEADER = "# fmn-look-gallery v2"
COLUMNS = ("panel", "source", "reference", "reference_sha256", "render", "render_sha256",
           "build_id", "advisory_verdict", "changed")
# fmn-cli/src/build_identity.rs compose_build_identity: git:<commit>[+dirty:<digest>|+unverified].
BUILD_ID = re.compile(rb"git:[0-9a-f]{40}(?:\+dirty:[0-9a-f]{16,64}|\+unverified)?")
CLEAN_BUILD_ID = re.compile(r"^git:[0-9a-f]{40}$")

PRIMITIVES = ("gradient_fills:GradientFills", "self_intersections:SelfIntersections",
              "joints_and_caps:JointsAndCaps", "glow:Glow", "lighting_3d:Lighting3D",
              "text_sample:TextSample", "math_formula:MathFormula")
README = ("SquareToCircle", "Hello", "GraphOnAxes", "PlaneAndVectors", "BracedMatrix", "PiecewiseCases",
          "TrackerTrace", "SurfaceWithAxes", "SubclassedPolygon", "NumberReadouts", "TitleAndList",
          "ArrowsAndLabels", "ColoredWords", "ShapeGrid", "NumberLineMarks", "CodeSnippet", "DashesAndDots",
          "Highlights", "GroupsArranged", "FadeTransformResult")


def snake(name: str) -> str:
    return re.sub(r"(?<=[a-z0-9])(?=[A-Z])|(?<=[A-Z])(?=[A-Z][a-z])", "_", name).lower()


def panels():
    """(panel id, scene file relative to the repo, scene class), in manifest order."""
    rows = [(panel, "gallery/scenes/primitives.py", scene)
            for panel, scene in (item.split(":") for item in PRIMITIVES)]
    rows += [(f"readme.{snake(scene)}", "gallery/scenes/readme.py", scene) for scene in README]
    rows += [(f"math.sheet_{n}", "gallery/scenes/math_sheet.py", f"MathSheet{n}") for n in (1, 2)]
    return sorted(rows)


def sha256(path: pathlib.Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def verify_reference_capture_set(root: pathlib.Path = REFERENCE_CAPTURES_ROOT,
                                 provenance_path: pathlib.Path = PROVENANCE) -> dict[str, str]:
    """Verify that top-level gallery/reference_captures/*.png match PROVENANCE.json."""
    if not provenance_path.is_file():
        return {}
    data = json.loads(provenance_path.read_text(encoding="utf-8"))
    expected = dict(data.get("captures", {}))
    for supplement in data.get("supplements", []):
        expected.update(supplement.get("recaptured", {}))
    observed = {}
    for png in sorted(root.glob("*.png")):
        panel = png.stem
        digest = sha256(png)
        if panel not in expected:
            raise RuntimeError(f"unrecorded top-level Reference capture {png} (not in {provenance_path})")
        if digest != expected[panel]:
            raise RuntimeError(
                f"Reference capture {png} digest mismatch: expected {expected[panel]}, got {digest}"
            )
        observed[panel] = digest
    return observed


def render_portal(fmn_python, source, scene, resolution, work):
    """The portal's certified final still and the build id its provenance names."""
    png = work / "portal.png"
    argv = [fmn_python, "--robot", str(ROOT / source), scene, "--format", "png", "--reproducible",
            "--resolution", resolution, "--output", str(png)]
    done = subprocess.run(argv, cwd=ROOT, capture_output=True, text=True, timeout=900)
    lines = [line for line in done.stdout.splitlines() if line.startswith("{")]
    receipt = json.loads(lines[-1]) if lines else {}
    if done.returncode != 0 or receipt.get("status") != "success" or not png.is_file():
        raise RuntimeError(f"fmn-python exit {done.returncode}: {receipt.get('message') or done.stderr[-400:]}")
    sidecar = pathlib.Path(receipt["manifest"]["path"])
    found = BUILD_ID.findall(sidecar.read_bytes())
    if len(set(found)) != 1:
        raise RuntimeError(f"provenance sidecar names {len(set(found))} build ids, not one")
    return png, found[0].decode()


def render_reference(python, reference_root, source, scene, resolution, work):
    """The pinned Reference's final still of the same source, or None with a reason."""
    workdir = work / "reference"
    workdir.mkdir()
    # A private base directory keeps the Reference's LaTeX cache and outputs inside the job.
    (workdir / "custom_config.yml").write_text(f"directories:\n  base: \"{workdir}\"\n", encoding="utf-8")
    media = workdir / "media"
    env = {"PATH": "/usr/bin:/bin", "HOME": str(workdir), "PYTHONPATH": str(reference_root)}
    argv = ["xvfb-run", "-a", "-s", "-screen 0 1920x1080x24", python, "-m", "manimlib", str(ROOT / source),
            scene, "-s", "-w", "-r", resolution, "--video_dir", str(media)]
    done = subprocess.run(argv, cwd=workdir, env=env, capture_output=True, text=True, timeout=900)
    pngs = sorted(media.rglob("*.png")) if media.exists() else []
    if done.returncode != 0 or not pngs:
        tail = (done.stderr.strip().splitlines() or ["no output"])[-1][:240]
        return None, f"Reference exit {done.returncode}: {tail}"
    return pngs[-1], None


def parse_manifest(text):
    """Rows of the current manifest, v1 or v2, keyed by panel (for history only)."""
    rows = {}
    lines = text.splitlines()
    columns = next((line[len("# columns: "):].split("\t") for line in lines if line.startswith("# columns: ")), [])
    for line in lines:
        if line and not line.startswith("#"):
            row = dict(zip(columns, line.split("\t")))
            rows[row["panel"]] = row
    revision = next((int(line.split(": ")[1]) for line in lines if line.startswith("# revision: ")), 0)
    return revision, rows


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--fmn-python", required=True, help="the installed portal CLI under review")
    parser.add_argument("--reference-python", help="a Python with the Reference's requirements")
    parser.add_argument("--reference-root", type=pathlib.Path, default=ROOT / "scripts" / "manim_ref")
    parser.add_argument("--resolution", default="1920x1080")
    parser.add_argument("--panel", action="append", default=[], help="regenerate only this panel (repeatable)")
    parser.add_argument("--recapture-reference", action="store_true",
                        help="allow replacing an existing gallery/reference_captures/panels/<panel>.png")
    parser.add_argument("--allow-dirty-calibration", action="store_true",
                        help="allow +dirty/+unverified build_id for local scratch inspection (never writes look_gallery.tsv)")
    args = parser.parse_args(argv)
    if not re.fullmatch(r"\d{2,5}x\d{2,5}", args.resolution):
        parser.error("--resolution must be WIDTHxHEIGHT")
    chosen = [p for p in panels() if not args.panel or p[0] in args.panel]
    if args.panel and len(chosen) != len(set(args.panel)):
        parser.error(f"unknown panel among {args.panel}")
    try:
        pre_captures = verify_reference_capture_set()
    except RuntimeError as error:
        print(json.dumps({"status": "error", "error": str(error)}), file=sys.stderr)
        return 1
    reference_commit = subprocess.run(["git", "-C", str(args.reference_root), "rev-parse", "HEAD"],
                                      capture_output=True, text=True, check=True).stdout.strip()
    revision, previous = parse_manifest(MANIFEST.read_text(encoding="utf-8"))
    today = datetime.date.today().isoformat()
    RENDERS.mkdir(parents=True, exist_ok=True)
    CAPTURES.mkdir(parents=True, exist_ok=True)
    rows, builds, failures = {}, set(), []
    for panel, source, scene in chosen:
        with tempfile.TemporaryDirectory(prefix=f"fmn-gallery-{panel}-") as tmp:
            work = pathlib.Path(tmp)
            try:
                png, build = render_portal(args.fmn_python, source, scene, args.resolution, work)
            except (RuntimeError, OSError, subprocess.TimeoutExpired, KeyError, ValueError) as error:
                failures.append({"panel": panel, "error": str(error)})
                print(json.dumps({"panel": panel, "status": "error", "error": str(error)}), flush=True)
                continue
            if not CLEAN_BUILD_ID.fullmatch(build) and not args.allow_dirty_calibration:
                err = (f"refusing non-clean build_id {build!r}: commit and reinstall fmn-python from a clean tree, "
                       "or pass --allow-dirty-calibration for scratch inspection without writing look_gallery.tsv")
                failures.append({"panel": panel, "error": err})
                print(json.dumps({"panel": panel, "status": "error", "error": err}), flush=True)
                continue
            render = RENDERS / f"{panel}.png"
            shutil.copyfile(png, render)
            capture, reason = CAPTURES / f"{panel}.png", None
            old = previous.get(panel, {})
            if args.reference_python:
                if capture.exists() and not args.recapture_reference:
                    failures.append({
                        "panel": panel,
                        "error": f"refusing to overwrite existing {capture} without --recapture-reference",
                    })
                    continue
                ref_png, reason = render_reference(args.reference_python, args.reference_root, source, scene,
                                                   args.resolution, work)
                # Captures are private data: replace only a file this tool recorded.
                if ref_png is not None and capture.exists() and sha256(capture) != old.get("reference_sha256"):
                    failures.append({"panel": panel, "error": f"refusing to overwrite unrecorded {capture}"})
                    continue
                if ref_png is not None:
                    shutil.copyfile(ref_png, capture)
            builds.add(build)
            captured = capture.is_file()
            render_digest = sha256(render)
            same_pixels = old.get("render_sha256") == render_digest
            verdict = old.get("advisory_verdict") if same_pixels else None
            if verdict is None or (verdict == "reference-capture-missing" and captured):
                verdict = "unreviewed" if captured else "reference-capture-missing"
            note = old.get("changed") if same_pixels and old.get("advisory_verdict") == verdict else (
                f"fm-5wq.50 {today}: rendered by the installed fmn-python ({build}); "
                + ("awaiting review" if captured else f"no Reference capture ({reason or 'not requested'})")
                + (f"; earlier advisory verdict {old.get('advisory_verdict') or old.get('verdict')} judged a "
                   "different render" if old else ""))
            rows[panel] = (panel, f"{source}:{scene}", f"gallery/reference_captures/panels/{panel}.png",
                           sha256(capture) if captured else "-", f"gallery/renders/{panel}.png",
                           render_digest, build, verdict, note)
            print(json.dumps({"panel": panel, "status": "ok", "build_id": build, "captured": captured,
                              "reference_note": reason}), flush=True)
    try:
        post_captures = verify_reference_capture_set()
        if post_captures != pre_captures:
            raise RuntimeError("top-level gallery/reference_captures/*.png changed during regeneration")
    except RuntimeError as error:
        print(json.dumps({"status": "error", "error": str(error)}), file=sys.stderr)
        return 1
    if failures:
        print(json.dumps({"status": "error", "failed_panels": failures}), file=sys.stderr)
        return 1
    if len(builds) > 1:
        print(json.dumps({"status": "error", "error": f"panels came from {len(builds)} builds"}), file=sys.stderr)
        return 1
    release = next(iter(builds))
    if args.allow_dirty_calibration:
        print(json.dumps({
            "status": "calibration_only",
            "release": release,
            "panels": len(rows),
            "manifest_written": False,
        }))
        return 0
    if args.panel:
        # A partial run keeps the other rows, so it must come from the release already under review.
        stale = [p for p, row in previous.items() if p not in rows and row.get("build_id") != release]
        if stale:
            print(json.dumps({"status": "error", "error": "other panels are from another build", "panels": stale}),
                  file=sys.stderr)
            return 1
        for panel, row in previous.items():
            rows.setdefault(panel, tuple(row[c] for c in COLUMNS))
    text = "\n".join([HEADER, f"# revision: {revision + 1}", f"# release: {release}",
                      f"# reference: 3b1b/manim@{reference_commit}", "# columns: " + "\t".join(COLUMNS),
                      *("\t".join(rows[p]) for p in sorted(rows))]) + "\n"
    MANIFEST.write_text(text, encoding="utf-8")
    print(json.dumps({"status": "ok", "release": release, "panels": len(rows), "manifest": str(MANIFEST)}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
