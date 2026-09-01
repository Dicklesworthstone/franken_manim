#!/usr/bin/env python3
"""Generate or verify the deterministic committed FrankenManim agent brief.

The Beads JSONL ledger remains authoritative. This command derives
``docs/AGENT_BRIEF.md`` from that ledger using the newest issue timestamp as
``as_of``; wall-clock time never enters the artifact, so identical ledger bytes
produce identical Markdown. The broad situational projection and the
machine-readable leaf planner are computed from the same parsed issue graph,
so the human recommendation cannot disagree with autonomous triage.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import stat
import sys
from pathlib import Path
from typing import Any

import agent_brief
import agent_next

DEFAULT_LEDGER = Path(".beads/issues.jsonl")
DEFAULT_OUTPUT = Path("docs/AGENT_BRIEF.md")
MAX_OUTPUT_BYTES = 4 * 1024 * 1024


class GenerateError(ValueError):
    pass


def ledger_as_of(issues: dict[str, agent_brief.Issue]):
    if not issues:
        raise GenerateError("the Beads ledger contains no issues")
    return max(issue.updated_at for issue in issues.values())


def render_leaf_section(plan: dict[str, Any]) -> str:
    recommendation = plan["recommendation"]
    issue = recommendation["issue"]
    if issue is None:
        selected = "**none**"
    else:
        selected = (
            f"**P{issue['priority']} `{issue['id']}`** "
            f"[{issue['workstream']}]"
        )
    lines = [
        "## Leaf-safe claim plan",
        "",
        f"- Recommended next: {selected} — {recommendation['reason']}.",
        (
            f"- Queues: **{plan['counts']['claimable_leaves']}** claimable leaves, "
            f"**{plan['counts']['ready_containers']}** ready containers "
            f"(**{plan['counts']['non_epic_containers']}** non-epic), "
            f"**{plan['counts']['assigned_ready']}** assigned-ready."
        ),
        "- A task with any live `parent-child` descendant is a container, regardless of its issue type.",
        "- This section is the claim contract; the broader queue below is situational context only.",
        "",
    ]
    return "\n".join(lines)


def build_document(
    ledger: Path,
    *,
    stale_days: int,
    activation_cap: int,
    limit: int,
) -> tuple[str, dict[str, Any]]:
    issues = agent_brief.load_issues(ledger)
    as_of = ledger_as_of(issues)
    snapshot = agent_brief.build_snapshot(
        issues,
        as_of=as_of,
        stale_days=stale_days,
        activation_cap=activation_cap,
        limit=limit,
    )
    plan = agent_next.build_plan(
        issues,
        as_of=as_of,
        stale_days=stale_days,
        activation_cap=activation_cap,
        limit=limit,
    )
    # Preserve the broad queues for situational awareness, but make every
    # human-visible recommendation use the exact machine planner decision.
    snapshot["recommendation"] = plan["recommendation"]
    snapshot["claim_plan"] = plan
    broad = agent_brief.render_markdown(snapshot).rstrip("\n")
    broad = broad.replace(
        "## Claimable ready queue",
        "## Broad dependency-ready queue",
        1,
    ).replace(
        "No claimable ready leaf issues.",
        "No broad dependency-ready open issues.",
        1,
    )
    marker = "\n## Active claims\n"
    if marker not in broad:
        raise GenerateError("agent brief renderer omitted the Active claims section")
    document = broad.replace(
        marker,
        "\n" + render_leaf_section(plan) + "## Active claims\n",
        1,
    ).rstrip("\n") + "\n"
    encoded = document.encode("utf-8")
    if len(encoded) > MAX_OUTPUT_BYTES:
        raise GenerateError(
            f"generated agent brief exceeds the {MAX_OUTPUT_BYTES}-byte output limit"
        )
    return document, snapshot


def digest(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def read_existing(path: Path) -> str:
    if path.is_symlink():
        raise GenerateError(f"refusing symlink output path {path}")
    try:
        metadata = path.stat()
    except FileNotFoundError as exc:
        raise GenerateError(f"generated agent brief is missing: {path}") from exc
    except OSError as exc:
        raise GenerateError(f"cannot stat generated agent brief {path}: {exc}") from exc
    if not stat.S_ISREG(metadata.st_mode):
        raise GenerateError(f"generated agent brief is not a regular file: {path}")
    if metadata.st_size > MAX_OUTPUT_BYTES:
        raise GenerateError(
            f"{path} exceeds the {MAX_OUTPUT_BYTES}-byte generated-output limit"
        )
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise GenerateError(f"cannot read generated agent brief {path}: {exc}") from exc


def write_atomic(path: Path, document: str) -> None:
    if path.is_symlink():
        raise GenerateError(f"refusing symlink output path {path}")
    if path.exists() and not path.is_file():
        raise GenerateError(f"generated agent brief path is not a regular file: {path}")
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
    except OSError as exc:
        raise GenerateError(f"cannot create output directory {path.parent}: {exc}") from exc

    temporary = path.with_name(f".{path.name}.tmp")
    if temporary.is_symlink():
        raise GenerateError(f"refusing symlink temporary path {temporary}")
    if temporary.exists():
        raise GenerateError(
            f"refusing pre-existing temporary path {temporary}; "
            "inspect it before retrying"
        )
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(temporary, flags, 0o644)
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as handle:
            handle.write(document)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except OSError as exc:
        raise GenerateError(f"cannot publish generated agent brief {path}: {exc}") from exc


def check_current(path: Path, expected: str) -> None:
    observed = read_existing(path)
    if observed != expected:
        raise GenerateError(
            f"generated agent brief is stale: {path}; "
            f"expected_sha256={digest(expected)} observed_sha256={digest(observed)}; "
            "run `python3 scripts/generate_agent_brief.py`"
        )


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ledger", type=Path, default=DEFAULT_LEDGER)
    parser.add_argument("--output", type=Path, default=DEFAULT_OUTPUT)
    parser.add_argument("--stale-days", type=int, default=2)
    parser.add_argument("--activation-cap", type=int, default=4)
    parser.add_argument("--limit", type=int, default=20)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--check",
        action="store_true",
        help="verify committed bytes and governance state without writing",
    )
    mode.add_argument(
        "--stdout",
        action="store_true",
        help="render exact Markdown to stdout without reading or writing the output path",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    if args.stale_days < 0:
        print("error: --stale-days must be nonnegative", file=sys.stderr)
        return 2
    if args.activation_cap < 1:
        print("error: --activation-cap must be positive", file=sys.stderr)
        return 2
    if args.limit < 1 or args.limit > 1000:
        print("error: --limit must be between 1 and 1000", file=sys.stderr)
        return 2
    try:
        document, snapshot = build_document(
            args.ledger,
            stale_days=args.stale_days,
            activation_cap=args.activation_cap,
            limit=args.limit,
        )
    except (agent_brief.BriefError, GenerateError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    if not snapshot["integrity"]["within_contract"]:
        counts = snapshot["counts"]
        print(
            "error: Beads dependency integrity is invalid "
            f"({counts['blocking_cycles']} blocking cycles, "
            f"{counts['missing_blockers']} missing blockers)",
            file=sys.stderr,
        )
        return 1
    if not snapshot["activation"]["within_cap"]:
        print(
            "error: active workstream cap breached "
            f"({snapshot['activation']['count']}/{snapshot['activation']['cap']})",
            file=sys.stderr,
        )
        return 1

    try:
        if args.stdout:
            sys.stdout.write(document)
            return 0
        if args.check:
            check_current(args.output, document)
        else:
            write_atomic(args.output, document)
    except GenerateError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1

    action = "current" if args.check else "generated"
    print(
        f"agent brief {action}: {args.output} "
        f"sha256={digest(document)} as_of={snapshot['as_of']}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
