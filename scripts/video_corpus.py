#!/usr/bin/env python3
"""VIDEO_CORPUS.lock tooling — fm-rqc (§15.3-15.4, R13).

The corpus gate's machinery: `VIDEO_CORPUS.lock` pins an exact scene
allowlist from the pinned `3b1b/videos` tree — scene names, module blob
hashes, era, per-scene attribution, and the asset manifest — under the
CC BY-NC-SA fixture policy (private gallery fixtures, public permissive
primitive corpus, per-scene attribution). The G4a criterion the lock
serves: the enumerated scenes run source-unedited through fmn-python
under the documented shims and pass structural assertions plus
Look-Gallery review — never pixel diffs.

Deterministic and stdlib-only, like scripts/harvest_tex_corpus.py: the
same pins reproduce the same lock byte-for-byte (sorted iteration
everywhere, no timestamps).

Subcommands
-----------
scan    Advisory census of the pinned videos tree: every Scene subclass,
        scored for self-containedness (imports, asset references, custom
        GLSL, TeX usage). Guides seed curation; commits nothing.
verify  Recompute every hash-bearing field of the committed
        VIDEO_CORPUS.lock against the pinned checkout and byte-compare.
        Exit 0 clean; exit 1 drift (named per row); exit 2 when the
        gitignored checkout is absent (with the exact clone commands).
emit    Regenerate VIDEO_CORPUS.lock from the curated SEED table below
        plus the pinned checkout. Run under VIDEO_CORPUS_UPDATE=1 only —
        the blessing ritual mirrors the coverage ratchet's.

The pins are read from SUITE.lock `[reference]`; the checkout convention
is scripts/videos_ref (gitignored), exactly as G0-4 established.
"""

from __future__ import annotations

import ast
import hashlib
import os
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
LOCK_PATH = REPO / "VIDEO_CORPUS.lock"
VIDEOS_REF = REPO / "scripts" / "videos_ref"

# ---------------------------------------------------------------------------
# The curated seed allowlist (fm-rqc tranche 1).
#
# Selection rule (mechanical, from `scan`): modern-era modules whose
# imports resolve entirely inside manimlib (no per-video helper modules,
# no third-party imports), with no image/SVG/sound asset references and
# no custom-GLSL signals. Each entry names one Scene subclass the G4a
# harness will drive source-unedited. Attribution rows satisfy the
# CC BY-NC-SA fixture policy: the source tree is 3b1b/videos (Grant
# Sanderson), per-scene provenance recorded here.
#
# status vocabulary (the lock's whole vocabulary):
#   allowlisted                 in the gallery; the harness must run it
#   pending-with-named-constructs   TeX outruns the current fmd-math tier;
#                               the named constructs feed the W6 ratchet
#   excluded                    considered and rejected, reason recorded
# ---------------------------------------------------------------------------
SEED: tuple[dict[str, str], ...] = ()  # populated by curation below


def read_reference_pins() -> dict[str, str]:
    pins: dict[str, str] = {}
    in_ref = False
    for line in (REPO / "SUITE.lock").read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_ref = stripped == "[reference]"
            continue
        if not in_ref or not stripped or stripped.startswith("#"):
            continue
        fields = stripped.split("\t")
        if len(fields) >= 2:
            pins[fields[0]] = fields[1]
    return pins


def require_checkout() -> None:
    if not (VIDEOS_REF / ".git").exists():
        pins = read_reference_pins()
        sys.stderr.write(
            "videos checkout missing. Recreate it exactly as G0-4 did:\n"
            "  git clone https://github.com/3b1b/videos scripts/videos_ref\n"
            f"  git -C scripts/videos_ref checkout {pins['3b1b/videos']}\n"
        )
        raise SystemExit(2)


def blob_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


ASSET_SUFFIXES = (
    ".png",
    ".jpg",
    ".jpeg",
    ".svg",
    ".gif",
    ".mp3",
    ".wav",
    ".mp4",
    ".obj",
    ".glb",
)
GLSL_MARKERS = ("set_color_by_code", "shader_folder", "glsl", "Shader")
TEX_CONSTRUCTORS = {"Tex", "TexText", "OldTex", "OldTexText", "SingleStringTex"}


class ModuleFacts(ast.NodeVisitor):
    """One pass over a videos-tree module: scenes, imports, asset signals."""

    def __init__(self) -> None:
        self.scene_classes: list[str] = []
        self.imports: set[str] = set()
        self.import_star_manimlib = False
        self.asset_literals: set[str] = set()
        self.glsl_signals: set[str] = set()
        self.tex_calls = 0

    def visit_ClassDef(self, node: ast.ClassDef) -> None:
        for base in node.bases:
            name = base.id if isinstance(base, ast.Name) else (
                base.attr if isinstance(base, ast.Attribute) else ""
            )
            if name.endswith("Scene"):
                self.scene_classes.append(node.name)
                break
        self.generic_visit(node)

    def visit_Import(self, node: ast.Import) -> None:
        for alias in node.names:
            self.imports.add(alias.name.split(".")[0])

    def visit_ImportFrom(self, node: ast.ImportFrom) -> None:
        root = (node.module or "").split(".")[0]
        if node.level:
            root = f".{root}" if root else "."
        self.imports.add(root)
        if root == "manimlib" and any(a.name == "*" for a in node.names):
            self.import_star_manimlib = True

    def visit_Call(self, node: ast.Call) -> None:
        callee = node.func.id if isinstance(node.func, ast.Name) else (
            node.func.attr if isinstance(node.func, ast.Attribute) else ""
        )
        if callee in TEX_CONSTRUCTORS:
            self.tex_calls += 1
        self.generic_visit(node)

    def visit_Constant(self, node: ast.Constant) -> None:
        if isinstance(node.value, str):
            lowered = node.value.lower()
            if lowered.endswith(ASSET_SUFFIXES):
                self.asset_literals.add(node.value)
            for marker in GLSL_MARKERS:
                if marker.lower() in lowered and marker == "glsl":
                    self.glsl_signals.add(node.value)

    def visit_Attribute(self, node: ast.Attribute) -> None:
        if node.attr in ("shader_folder", "set_color_by_code"):
            self.glsl_signals.add(node.attr)
        self.generic_visit(node)


STDLIB_OK = {
    "manimlib",
    "numpy",
    "np",
    "math",
    "itertools",
    "functools",
    "random",
    "typing",
    "fractions",
    "collections",
    "copy",
    "operator",
    "os",
    "sys",
    "re",
    "string",
    "colorsys",
}


def scan(argv: list[str]) -> int:
    require_checkout()
    rows: list[tuple[str, str, str, str]] = []
    for path in sorted(VIDEOS_REF.rglob("*.py")):
        rel = path.relative_to(VIDEOS_REF).as_posix()
        if rel.startswith((".", "manim_imports_ext")):
            continue
        try:
            tree = ast.parse(path.read_text(encoding="utf-8", errors="replace"))
        except SyntaxError:
            continue
        facts = ModuleFacts()
        facts.visit(tree)
        if not facts.scene_classes:
            continue
        foreign = sorted(
            i for i in facts.imports if i not in STDLIB_OK and not i.startswith(".")
        ) + sorted(i for i in facts.imports if i.startswith("."))
        flags: list[str] = []
        if foreign:
            flags.append("imports:" + ",".join(foreign))
        if facts.asset_literals:
            flags.append(f"assets:{len(facts.asset_literals)}")
        if facts.glsl_signals:
            flags.append("glsl")
        if facts.tex_calls:
            flags.append(f"tex:{facts.tex_calls}")
        verdict = "self-contained" if not foreign and not facts.asset_literals and not facts.glsl_signals else "entangled"
        rows.append((verdict, rel, ";".join(facts.scene_classes), " ".join(flags) or "-"))
    only_clean = "--self-contained" in argv
    for verdict, rel, scenes, flags in rows:
        if only_clean and verdict != "self-contained":
            continue
        sys.stdout.write(f"{verdict}\t{rel}\t{scenes}\t{flags}\n")
    return 0


def main() -> int:
    if len(sys.argv) < 2 or sys.argv[1] not in ("scan", "verify", "emit"):
        sys.stderr.write(__doc__ or "")
        return 2
    if sys.argv[1] == "scan":
        return scan(sys.argv[2:])
    sys.stderr.write("verify/emit land with the lock in this tranche\n")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
