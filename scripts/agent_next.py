#!/usr/bin/env python3
"""Fail-closed claim planner layered over :mod:`agent_brief`.

``agent_brief`` owns bounded ledger parsing and blocking-graph analysis. This
module adds the narrower question an autonomous worker actually needs answered:
which *unassigned dependency-ready leaf* is the best collision-free next claim?
A non-epic parent with live ``parent-child`` descendants is a container, not a
leaf. Among otherwise equal candidates, work that immediately releases more
blocked issues wins deterministically.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any

import agent_brief

SCHEMA = "fmn.agent.next"
SCHEMA_VERSION = 2
UTC = dt.timezone.utc


def live_children(issues: dict[str, agent_brief.Issue]) -> dict[str, tuple[str, ...]]:
    """Return live child IDs keyed by their existing parent ID."""

    children: dict[str, list[str]] = defaultdict(list)
    for child in issues.values():
        if child.status not in agent_brief.OPEN_STATUSES:
            continue
        for dependency in child.dependencies:
            if dependency.kind == "parent-child" and dependency.depends_on_id in issues:
                children[dependency.depends_on_id].append(child.id)
    return {parent: tuple(sorted(ids)) for parent, ids in children.items()}


def strongly_connected_components(
    adjacency: dict[str, tuple[str, ...]],
) -> tuple[tuple[str, ...], ...]:
    """Return deterministic nontrivial SCCs without Python recursion."""

    reverse_lists = {issue_id: [] for issue_id in adjacency}
    for issue_id, targets in adjacency.items():
        for target in targets:
            reverse_lists[target].append(issue_id)
    reverse = {
        issue_id: tuple(sorted(targets))
        for issue_id, targets in reverse_lists.items()
    }

    visited: set[str] = set()
    finish_order: list[str] = []
    for root in sorted(adjacency):
        if root in visited:
            continue
        visited.add(root)
        stack: list[tuple[str, int]] = [(root, 0)]
        while stack:
            issue_id, next_index = stack[-1]
            targets = adjacency[issue_id]
            if next_index < len(targets):
                target = targets[next_index]
                stack[-1] = (issue_id, next_index + 1)
                if target not in visited:
                    visited.add(target)
                    stack.append((target, 0))
            else:
                stack.pop()
                finish_order.append(issue_id)

    assigned: set[str] = set()
    components: list[tuple[str, ...]] = []
    for root in reversed(finish_order):
        if root in assigned:
            continue
        assigned.add(root)
        component: list[str] = []
        stack = [root]
        while stack:
            issue_id = stack.pop()
            component.append(issue_id)
            for target in reversed(reverse[issue_id]):
                if target not in assigned:
                    assigned.add(target)
                    stack.append(target)
        if len(component) > 1:
            components.append(tuple(sorted(component)))
    return tuple(sorted(components))


def containment_cycles(
    issues: dict[str, agent_brief.Issue],
    children: dict[str, tuple[str, ...]],
) -> tuple[tuple[str, ...], ...]:
    """Return live ``parent-child`` cycles as deterministic SCCs."""

    live_ids = {issue.id for issue in issues.values() if issue.status in agent_brief.OPEN_STATUSES}
    adjacency = {
        issue_id: tuple(child for child in children.get(issue_id, ()) if child in live_ids)
        for issue_id in live_ids
    }
    return strongly_connected_components(adjacency)


def blocker_pressure(
    issues: dict[str, agent_brief.Issue],
) -> tuple[dict[str, int], dict[str, int]]:
    direct: dict[str, int] = defaultdict(int)
    immediate: dict[str, int] = defaultdict(int)
    for issue in issues.values():
        if issue.status not in agent_brief.OPEN_STATUSES:
            continue
        blockers = agent_brief.unresolved_blockers(issue, issues)
        for blocker in blockers:
            direct[blocker] += 1
        if len(blockers) == 1:
            immediate[blockers[0]] += 1
    return dict(direct), dict(immediate)


def rank_key(
    issue: agent_brief.Issue,
    *,
    immediate: dict[str, int],
    direct: dict[str, int],
) -> tuple[int, int, int, int, float, str]:
    return (
        issue.priority,
        -immediate.get(issue.id, 0),
        -direct.get(issue.id, 0),
        1 if issue.workstream == "UNSCOPED" else 0,
        -issue.updated_at.timestamp(),
        issue.id,
    )


def build_plan(
    issues: dict[str, agent_brief.Issue],
    *,
    as_of: dt.datetime,
    stale_days: int,
    activation_cap: int,
    limit: int,
) -> dict[str, Any]:
    base = agent_brief.build_snapshot(
        issues,
        as_of=as_of,
        stale_days=stale_days,
        activation_cap=activation_cap,
        limit=limit,
    )
    children = live_children(issues)
    hierarchy_cycles = containment_cycles(issues, children)
    direct, immediate = blocker_pressure(issues)
    dependency_ready = [
        issue
        for issue in issues.values()
        if issue.status == "open" and not agent_brief.unresolved_blockers(issue, issues)
    ]
    assigned = [issue for issue in dependency_ready if issue.assignee is not None]
    containers = [
        issue
        for issue in dependency_ready
        if issue.assignee is None
        and (issue.issue_type in agent_brief.NON_CLAIMABLE_TYPES or children.get(issue.id))
    ]
    leaves = [
        issue
        for issue in dependency_ready
        if issue.assignee is None
        and issue.issue_type not in agent_brief.NON_CLAIMABLE_TYPES
        and not children.get(issue.id)
    ]
    active_workstreams = set(base["activation"]["active_workstreams"])
    base_integrity = base["integrity"]
    blocking_ok = bool(base_integrity["within_contract"])
    integrity = {
        "ok": blocking_ok and not hierarchy_cycles,
        "blocking_within_contract": blocking_ok,
        "blocking_cycles": base_integrity["blocking_cycles"],
        "containment_cycles": [list(component) for component in hierarchy_cycles],
        "missing_blockers": base_integrity["missing_blockers"],
        "missing_links": base_integrity["missing_links"],
    }

    recommendation: agent_brief.Issue | None = None
    reason: str
    if not blocking_ok:
        reason = "blocking-graph integrity must be repaired before claiming work"
    elif hierarchy_cycles:
        reason = "parent-child containment cycles must be repaired before claiming work"
    else:
        active_leaves = [issue for issue in leaves if issue.workstream in active_workstreams]
        candidates = active_leaves or leaves
        if not candidates:
            reason = "no dependency-ready unassigned leaf exists"
        elif not active_leaves and len(active_workstreams) >= activation_cap:
            reason = "activation cap is full and no claimable leaf belongs to an active workstream"
        else:
            recommendation = min(
                candidates,
                key=lambda issue: rank_key(
                    issue,
                    immediate=immediate,
                    direct=direct,
                ),
            )
            if active_leaves:
                reason = f"best leaf in already-active {recommendation.workstream}"
            elif recommendation.workstream == "UNSCOPED":
                reason = "best leaf is unscoped; verify governance before claiming"
            else:
                reason = f"best leaf; claiming it activates {recommendation.workstream}"
            if immediate.get(recommendation.id, 0):
                reason += (
                    f"; completion immediately unblocks {immediate[recommendation.id]} live issue(s)"
                )
            elif direct.get(recommendation.id, 0):
                reason += f"; it participates in blocking {direct[recommendation.id]} live issue(s)"

    def row(issue: agent_brief.Issue) -> dict[str, Any]:
        return {
            "id": issue.id,
            "priority": issue.priority,
            "workstream": issue.workstream,
            "issue_type": issue.issue_type,
            "assignee": issue.assignee,
            "updated_at": issue.updated_at.isoformat().replace("+00:00", "Z"),
            "title": agent_brief.compact_title(issue.title),
            "live_children": list(children.get(issue.id, ())),
            "direct_unblocks": direct.get(issue.id, 0),
            "immediate_unblocks": immediate.get(issue.id, 0),
        }

    activates = bool(
        recommendation is not None
        and recommendation.workstream != "UNSCOPED"
        and recommendation.workstream not in active_workstreams
    )
    sort = lambda issue: rank_key(issue, immediate=immediate, direct=direct)
    return {
        "schema": SCHEMA,
        "version": SCHEMA_VERSION,
        "as_of": as_of.astimezone(UTC).isoformat().replace("+00:00", "Z"),
        "integrity": integrity,
        "activation": base["activation"],
        "recommendation": {
            "issue": row(recommendation) if recommendation is not None else None,
            "reason": reason,
            "activates_workstream": activates,
        },
        "counts": {
            "claimable_leaves": len(leaves),
            "assigned_ready": len(assigned),
            "ready_containers": len(containers),
            "non_epic_containers": sum(
                issue.issue_type not in agent_brief.NON_CLAIMABLE_TYPES for issue in containers
            ),
            "containment_cycles": len(hierarchy_cycles),
        },
        "claimable_leaves": [row(issue) for issue in sorted(leaves, key=sort)[:limit]],
        "ready_containers": [row(issue) for issue in sorted(containers, key=sort)[:limit]],
        "assigned_ready": [row(issue) for issue in sorted(assigned, key=sort)[:limit]],
    }


def render_json(plan: dict[str, Any]) -> str:
    return json.dumps(plan, sort_keys=True, separators=(",", ":")) + "\n"


def render_markdown(plan: dict[str, Any]) -> str:
    recommendation = plan["recommendation"]
    issue = recommendation["issue"]
    selected = "none" if issue is None else f"P{issue['priority']} `{issue['id']}` [{issue['workstream']}]"
    lines = [
        "# FrankenManim claim plan",
        "",
        f"As of `{plan['as_of']}`; Beads remains authoritative.",
        "",
        f"- Graph integrity: **{'valid' if plan['integrity']['ok'] else 'INVALID'}**.",
        (
            f"- Active workstreams: **{plan['activation']['count']}/{plan['activation']['cap']}**; "
            + (", ".join(f"`{item}`" for item in plan["activation"]["active_workstreams"]) or "none")
            + "."
        ),
        f"- Recommended next: **{selected}** — {recommendation['reason']}.",
        (
            f"- Queues: {plan['counts']['claimable_leaves']} claimable leaves, "
            f"{plan['counts']['ready_containers']} ready containers, "
            f"{plan['counts']['assigned_ready']} assigned-ready, "
            f"{plan['counts']['containment_cycles']} containment cycles."
        ),
        "",
        "A task with any live `parent-child` descendant is a container even when its issue type is not `epic`.",
        "",
    ]
    for component in plan["integrity"]["containment_cycles"]:
        lines.append(
            "- Parent-child cycle: " + " → ".join(f"`{issue_id}`" for issue_id in component)
        )
    if plan["integrity"]["containment_cycles"]:
        lines.append("")
    return "\n".join(lines)


def parse_as_of(value: str | None) -> dt.datetime:
    if value is None:
        return dt.datetime.now(UTC)
    return agent_brief.parse_timestamp(value, field="--as-of", issue_id="command line")


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ledger", type=Path, default=Path(".beads/issues.jsonl"))
    parser.add_argument("--format", choices=("id", "json", "markdown"), default="id")
    parser.add_argument("--as-of")
    parser.add_argument("--stale-days", type=int, default=2)
    parser.add_argument("--activation-cap", type=int, default=4)
    parser.add_argument("--limit", type=int, default=20)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--require", action="store_true")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    if args.stale_days < 0 or args.activation_cap < 1 or not 1 <= args.limit <= 1000:
        print("error: invalid planner bounds", file=sys.stderr)
        return 2
    try:
        issues = agent_brief.load_issues(args.ledger)
        plan = build_plan(
            issues,
            as_of=parse_as_of(args.as_of),
            stale_days=args.stale_days,
            activation_cap=args.activation_cap,
            limit=args.limit,
        )
    except agent_brief.BriefError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2

    enforce = args.check or args.require
    if enforce and not plan["integrity"]["ok"]:
        print("error: Beads task-graph integrity failed", file=sys.stderr)
        return 1
    if enforce and not plan["activation"]["within_cap"]:
        print("error: active workstream cap breached", file=sys.stderr)
        return 1
    if args.require and plan["recommendation"]["issue"] is None:
        print("error: no claimable recommendation exists", file=sys.stderr)
        return 3

    if args.format == "json":
        print(render_json(plan), end="")
    elif args.format == "markdown":
        print(render_markdown(plan), end="")
    else:
        issue = plan["recommendation"]["issue"]
        print(issue["id"] if issue is not None else "none")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
