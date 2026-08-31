#!/usr/bin/env python3
"""Deterministic, bounded operational brief for the Beads work graph.

This is a read-only projection of .beads/issues.jsonl. It never edits Beads and
never becomes a second source of truth. The output is intentionally compact so
an agent can reconstruct current work, blockers, and activation pressure
without loading the multi-megabyte ledger.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

MAX_LEDGER_BYTES = 32 * 1024 * 1024
MAX_LINE_BYTES = 2 * 1024 * 1024
MAX_ISSUES = 100_000
ACTIVE_STATUS = "in_progress"
OPEN_STATUSES = frozenset({"open", ACTIVE_STATUS})
CLOSED_STATUSES = frozenset({"closed", "tombstone"})
WORKSTREAM_RE = re.compile(r"^W(?P<number>\d+)(?:\b|:)")
UTC = dt.timezone.utc


class BriefError(ValueError):
    pass


@dataclass(frozen=True)
class Dependency:
    issue_id: str
    depends_on_id: str
    kind: str


@dataclass(frozen=True)
class Issue:
    id: str
    title: str
    status: str
    priority: int
    issue_type: str
    assignee: str | None
    updated_at: dt.datetime
    dependencies: tuple[Dependency, ...]
    comments: tuple[dict[str, Any], ...]

    @property
    def workstream(self) -> str:
        match = WORKSTREAM_RE.match(self.title)
        return f"W{match.group('number')}" if match else "UNSCOPED"


def parse_timestamp(value: str, *, field: str, issue_id: str) -> dt.datetime:
    if not isinstance(value, str) or not value:
        raise BriefError(f"{issue_id}: {field} must be a non-empty timestamp")
    normalized = value[:-1] + "+00:00" if value.endswith("Z") else value
    try:
        parsed = dt.datetime.fromisoformat(normalized)
    except ValueError as exc:
        raise BriefError(f"{issue_id}: invalid {field} timestamp {value!r}") from exc
    if parsed.tzinfo is None:
        raise BriefError(f"{issue_id}: {field} must include a timezone")
    return parsed.astimezone(UTC)


def _text(value: Any, *, field: str, issue_id: str, optional: bool = False) -> str | None:
    if optional and value is None:
        return None
    if not isinstance(value, str) or not value:
        raise BriefError(f"{issue_id}: {field} must be a non-empty string")
    return value


def parse_issue(raw: Any, *, line: int) -> Issue:
    if not isinstance(raw, dict):
        raise BriefError(f"line {line}: issue record must be an object")
    issue_id = _text(raw.get("id"), field="id", issue_id=f"line {line}")
    assert issue_id is not None
    title = _text(raw.get("title"), field="title", issue_id=issue_id)
    status = _text(raw.get("status"), field="status", issue_id=issue_id)
    issue_type = _text(raw.get("issue_type"), field="issue_type", issue_id=issue_id)
    priority = raw.get("priority")
    if isinstance(priority, bool) or not isinstance(priority, int) or priority < 0:
        raise BriefError(f"{issue_id}: priority must be a nonnegative integer")
    assignee = _text(raw.get("assignee"), field="assignee", issue_id=issue_id, optional=True)
    updated_raw = raw.get("updated_at") or raw.get("created_at")
    updated_at = parse_timestamp(updated_raw, field="updated_at", issue_id=issue_id)

    dependencies: list[Dependency] = []
    for index, item in enumerate(raw.get("dependencies") or ()):
        if not isinstance(item, dict):
            raise BriefError(f"{issue_id}: dependency {index} must be an object")
        owner = _text(item.get("issue_id"), field="dependency.issue_id", issue_id=issue_id)
        target = _text(item.get("depends_on_id"), field="dependency.depends_on_id", issue_id=issue_id)
        kind = _text(item.get("type"), field="dependency.type", issue_id=issue_id)
        dependencies.append(Dependency(owner or "", target or "", kind or ""))

    comments_raw = raw.get("comments") or []
    if not isinstance(comments_raw, list):
        raise BriefError(f"{issue_id}: comments must be an array")
    comments = tuple(item for item in comments_raw if isinstance(item, dict))
    return Issue(
        id=issue_id,
        title=title or "",
        status=status or "",
        priority=priority,
        issue_type=issue_type or "",
        assignee=assignee,
        updated_at=updated_at,
        dependencies=tuple(dependencies),
        comments=comments,
    )


def load_issues(path: Path) -> dict[str, Issue]:
    try:
        size = path.stat().st_size
    except OSError as exc:
        raise BriefError(f"cannot stat {path}: {exc}") from exc
    if size > MAX_LEDGER_BYTES:
        raise BriefError(f"{path} exceeds the {MAX_LEDGER_BYTES}-byte ledger limit")
    issues: dict[str, Issue] = {}
    try:
        with path.open("rb") as handle:
            for line_number, raw_line in enumerate(handle, 1):
                if len(raw_line) > MAX_LINE_BYTES:
                    raise BriefError(
                        f"{path}:{line_number} exceeds the {MAX_LINE_BYTES}-byte line limit"
                    )
                if not raw_line.endswith(b"\n"):
                    raise BriefError(f"{path}:{line_number} is missing its final LF")
                if not raw_line.strip():
                    raise BriefError(f"{path}:{line_number} is blank")
                try:
                    record = json.loads(raw_line)
                except (UnicodeDecodeError, json.JSONDecodeError) as exc:
                    raise BriefError(f"{path}:{line_number}: invalid UTF-8 JSON: {exc}") from exc
                issue = parse_issue(record, line=line_number)
                if issue.id in issues:
                    raise BriefError(f"{path}:{line_number}: duplicate issue id {issue.id}")
                issues[issue.id] = issue
                if len(issues) > MAX_ISSUES:
                    raise BriefError(f"{path} exceeds the {MAX_ISSUES}-issue limit")
    except OSError as exc:
        raise BriefError(f"cannot read {path}: {exc}") from exc
    return issues


def blocking_dependencies(issue: Issue) -> tuple[str, ...]:
    return tuple(
        dep.depends_on_id
        for dep in issue.dependencies
        if dep.issue_id == issue.id and dep.kind == "blocks"
    )


def unresolved_blockers(issue: Issue, issues: dict[str, Issue]) -> tuple[str, ...]:
    unresolved: list[str] = []
    for blocker_id in blocking_dependencies(issue):
        blocker = issues.get(blocker_id)
        if blocker is None or blocker.status not in CLOSED_STATUSES:
            unresolved.append(blocker_id)
    return tuple(sorted(set(unresolved)))


def issue_sort_key(issue: Issue) -> tuple[int, int, float, str]:
    status_rank = 0 if issue.status == ACTIVE_STATUS else 1
    return (status_rank, issue.priority, -issue.updated_at.timestamp(), issue.id)


def compact_title(title: str, limit: int = 100) -> str:
    normalized = " ".join(title.split())
    return normalized if len(normalized) <= limit else normalized[: limit - 1] + "…"


def latest_comment(issue: Issue) -> str | None:
    for item in reversed(issue.comments):
        text = item.get("text")
        if isinstance(text, str) and text.strip():
            return compact_title(text, 140)
    return None


def build_snapshot(
    issues: dict[str, Issue],
    *,
    as_of: dt.datetime,
    stale_days: int,
    activation_cap: int,
    limit: int,
) -> dict[str, Any]:
    if as_of.tzinfo is None:
        raise BriefError("as_of must include a timezone")
    as_of = as_of.astimezone(UTC)
    active = sorted(
        (issue for issue in issues.values() if issue.status == ACTIVE_STATUS),
        key=issue_sort_key,
    )
    open_issues = sorted(
        (issue for issue in issues.values() if issue.status in OPEN_STATUSES),
        key=issue_sort_key,
    )
    active_workstreams = sorted({issue.workstream for issue in active})
    ready = [
        issue
        for issue in open_issues
        if issue.status == "open" and not unresolved_blockers(issue, issues)
    ]
    blocked = [
        (issue, unresolved_blockers(issue, issues))
        for issue in open_issues
        if unresolved_blockers(issue, issues)
    ]
    stale_before = as_of - dt.timedelta(days=stale_days)
    stale = [
        issue
        for issue in active
        if issue.assignee is not None and issue.updated_at < stale_before
    ]

    def row(issue: Issue) -> dict[str, Any]:
        blockers = unresolved_blockers(issue, issues)
        return {
            "id": issue.id,
            "priority": issue.priority,
            "status": issue.status,
            "workstream": issue.workstream,
            "assignee": issue.assignee,
            "updated_at": issue.updated_at.isoformat().replace("+00:00", "Z"),
            "title": compact_title(issue.title),
            "blockers": list(blockers),
            "latest_comment": latest_comment(issue),
        }

    return {
        "schema_version": 1,
        "as_of": as_of.isoformat().replace("+00:00", "Z"),
        "activation": {
            "cap": activation_cap,
            "active_workstreams": active_workstreams,
            "count": len(active_workstreams),
            "within_cap": len(active_workstreams) <= activation_cap,
        },
        "counts": {
            "total": len(issues),
            "open": sum(issue.status == "open" for issue in issues.values()),
            "in_progress": len(active),
            "closed": sum(issue.status in CLOSED_STATUSES for issue in issues.values()),
            "ready": len(ready),
            "blocked": len(blocked),
            "stale_claims": len(stale),
        },
        "active": [row(issue) for issue in active[:limit]],
        "ready": [row(issue) for issue in ready[:limit]],
        "blocked": [
            {**row(issue), "blockers": list(blockers)}
            for issue, blockers in blocked[:limit]
        ],
        "stale_claims": [row(issue) for issue in stale[:limit]],
    }


def render_markdown(snapshot: dict[str, Any]) -> str:
    activation = snapshot["activation"]
    counts = snapshot["counts"]
    lines = [
        "# FrankenManim agent brief",
        "",
        f"As of `{snapshot['as_of']}`. Read-only projection of `.beads/issues.jsonl`.",
        "",
        "## Control plane",
        "",
        (
            f"- Workstreams: **{activation['count']}/{activation['cap']}** active "
            f"({'within cap' if activation['within_cap'] else 'CAP BREACHED'}): "
            + (", ".join(f"`{item}`" for item in activation["active_workstreams"]) or "none")
        ),
        (
            f"- Issues: **{counts['in_progress']}** in progress, **{counts['open']}** open, "
            f"**{counts['ready']}** ready, **{counts['blocked']}** blocked, "
            f"**{counts['stale_claims']}** stale claims."
        ),
    ]

    def section(title: str, rows: Iterable[dict[str, Any]], empty: str) -> None:
        lines.extend(["", f"## {title}", ""])
        materialized = list(rows)
        if not materialized:
            lines.append(empty)
            return
        for row in materialized:
            owner = f" @{row['assignee']}" if row["assignee"] else ""
            blockers = (
                " blocked by " + ", ".join(f"`{item}`" for item in row["blockers"])
                if row["blockers"]
                else ""
            )
            lines.append(
                f"- **P{row['priority']} `{row['id']}`** [{row['workstream']}]{owner}"
                f"{blockers}: {row['title']}"
            )

    section("Active claims", snapshot["active"], "No active claims.")
    section("Ready queue", snapshot["ready"], "No dependency-ready open issues.")
    section("Blocked queue", snapshot["blocked"], "No blocked open issues.")
    section("Stale claims", snapshot["stale_claims"], "No stale assigned claims.")
    lines.extend(
        [
            "",
            "## Agent protocol",
            "",
            "1. Refresh this brief immediately before claiming work.",
            "2. Treat Beads as authoritative; this projection never edits status.",
            "3. Do not activate a fifth workstream. Prefer a ready bead in an already-active workstream.",
            "4. Re-run after every claim, dependency change, close, or handoff.",
            "",
        ]
    )
    return "\n".join(lines)


def parse_as_of(value: str | None) -> dt.datetime:
    if value is None:
        return dt.datetime.now(UTC)
    return parse_timestamp(value, field="--as-of", issue_id="command line")


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ledger", type=Path, default=Path(".beads/issues.jsonl"))
    parser.add_argument("--format", choices=("markdown", "json"), default="markdown")
    parser.add_argument("--as-of", help="ISO-8601 timestamp; defaults to the current UTC time")
    parser.add_argument("--stale-days", type=int, default=2)
    parser.add_argument("--activation-cap", type=int, default=4)
    parser.add_argument("--limit", type=int, default=20)
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit nonzero when the activation cap is breached or the ledger is malformed",
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
        issues = load_issues(args.ledger)
        snapshot = build_snapshot(
            issues,
            as_of=parse_as_of(args.as_of),
            stale_days=args.stale_days,
            activation_cap=args.activation_cap,
            limit=args.limit,
        )
    except BriefError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    if args.format == "json":
        print(json.dumps(snapshot, indent=2, sort_keys=True))
    else:
        print(render_markdown(snapshot), end="")
    if args.check and not snapshot["activation"]["within_cap"]:
        print(
            f"error: active workstream cap breached "
            f"({snapshot['activation']['count']}/{snapshot['activation']['cap']})",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
