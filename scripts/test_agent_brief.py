from __future__ import annotations

import datetime as dt
import json
import tempfile
import unittest
from pathlib import Path

import agent_brief


def record(
    issue_id: str,
    title: str,
    status: str,
    *,
    priority: int = 2,
    assignee: str | None = None,
    updated_at: str = "2026-08-30T00:00:00Z",
    blockers: tuple[str, ...] = (),
) -> dict:
    row = {
        "id": issue_id,
        "title": title,
        "status": status,
        "priority": priority,
        "issue_type": "task",
        "created_at": "2026-08-01T00:00:00Z",
        "updated_at": updated_at,
    }
    if assignee is not None:
        row["assignee"] = assignee
    if blockers:
        row["dependencies"] = [
            {"issue_id": issue_id, "depends_on_id": blocker, "type": "blocks"}
            for blocker in blockers
        ]
    return row


class AgentBriefTests(unittest.TestCase):
    def write_ledger(self, rows: list[dict], *, final_lf: bool = True) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "issues.jsonl"
        text = "\n".join(json.dumps(row, separators=(",", ":")) for row in rows)
        path.write_text(text + ("\n" if final_lf else ""), encoding="utf-8")
        return path

    def test_snapshot_distinguishes_ready_blocked_and_stale_claims(self) -> None:
        rows = [
            record("fm-done", "W1: dependency", "closed"),
            record(
                "fm-active",
                "W10: active",
                "in_progress",
                priority=1,
                assignee="Lilac",
                updated_at="2026-08-20T00:00:00Z",
            ),
            record("fm-ready", "W10: ready", "open", priority=1, blockers=("fm-done",)),
            record("fm-blocked", "W11: blocked", "open", blockers=("fm-missing",)),
        ]
        issues = agent_brief.load_issues(self.write_ledger(rows))
        snapshot = agent_brief.build_snapshot(
            issues,
            as_of=dt.datetime(2026, 8, 31, tzinfo=dt.timezone.utc),
            stale_days=2,
            activation_cap=4,
            limit=20,
        )
        self.assertEqual(snapshot["activation"]["active_workstreams"], ["W10"])
        self.assertEqual([row["id"] for row in snapshot["ready"]], ["fm-ready"])
        self.assertEqual([row["id"] for row in snapshot["blocked"]], ["fm-blocked"])
        self.assertEqual(snapshot["blocked"][0]["blockers"], ["fm-missing"])
        self.assertEqual([row["id"] for row in snapshot["stale_claims"]], ["fm-active"])

    def test_parent_child_edges_do_not_block_readiness(self) -> None:
        row = record("fm-child", "W10: child", "open")
        row["dependencies"] = [
            {"issue_id": "fm-child", "depends_on_id": "fm-parent", "type": "parent-child"}
        ]
        issues = agent_brief.load_issues(self.write_ledger([row]))
        self.assertEqual(agent_brief.unresolved_blockers(issues["fm-child"], issues), ())

    def test_activation_cap_is_fail_closed(self) -> None:
        rows = [
            record(f"fm-{index}", f"W{index}: active", "in_progress", assignee="agent")
            for index in range(1, 6)
        ]
        path = self.write_ledger(rows)
        status = agent_brief.main(
            [
                "--ledger",
                str(path),
                "--as-of",
                "2026-08-31T00:00:00Z",
                "--check",
            ]
        )
        self.assertEqual(status, 1)

    def test_duplicate_ids_and_missing_final_lf_are_rejected(self) -> None:
        duplicate = record("fm-x", "W1: x", "open")
        with self.assertRaisesRegex(agent_brief.BriefError, "duplicate issue id"):
            agent_brief.load_issues(self.write_ledger([duplicate, duplicate]))
        with self.assertRaisesRegex(agent_brief.BriefError, "missing its final LF"):
            agent_brief.load_issues(self.write_ledger([duplicate], final_lf=False))

    def test_markdown_is_compact_and_names_authority(self) -> None:
        issues = agent_brief.load_issues(
            self.write_ledger([record("fm-x", "W10: active", "in_progress", assignee="agent")])
        )
        snapshot = agent_brief.build_snapshot(
            issues,
            as_of=dt.datetime(2026, 8, 31, tzinfo=dt.timezone.utc),
            stale_days=2,
            activation_cap=4,
            limit=20,
        )
        rendered = agent_brief.render_markdown(snapshot)
        self.assertIn("Read-only projection of `.beads/issues.jsonl`", rendered)
        self.assertIn("**1/4** active", rendered)
        self.assertIn("`fm-x`", rendered)
        self.assertIn("Beads as authoritative", rendered)


if __name__ == "__main__":
    unittest.main()
