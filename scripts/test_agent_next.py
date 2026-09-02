from __future__ import annotations

import contextlib
import datetime as dt
import io
import json
import tempfile
import unittest
from pathlib import Path

import agent_next


def issue(
    ident: str,
    title: str,
    status: str = "open",
    *,
    priority: int = 2,
    issue_type: str = "task",
    assignee: str | None = None,
    blockers: tuple[str, ...] = (),
    parent: str | None = None,
) -> dict:
    row = {
        "id": ident,
        "title": title,
        "status": status,
        "priority": priority,
        "issue_type": issue_type,
        "created_at": "2026-08-01T00:00:00Z",
        "updated_at": "2026-08-31T00:00:00Z",
    }
    if assignee is not None:
        row["assignee"] = assignee
    dependencies = [
        {"issue_id": ident, "depends_on_id": blocker, "type": "blocks"}
        for blocker in blockers
    ]
    if parent is not None:
        dependencies.append(
            {"issue_id": ident, "depends_on_id": parent, "type": "parent-child"}
        )
    if dependencies:
        row["dependencies"] = dependencies
    return row


class AgentNextTests(unittest.TestCase):
    def ledger(self, rows: list[dict]) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "issues.jsonl"
        path.write_text(
            "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows),
            encoding="utf-8",
        )
        return path

    def plan(self, rows: list[dict], *, cap: int = 4) -> dict:
        path = self.ledger(rows)
        issues = agent_next.agent_brief.load_issues(path)
        return agent_next.build_plan(
            issues,
            as_of=dt.datetime(2026, 8, 31, tzinfo=dt.timezone.utc),
            stale_days=2,
            activation_cap=cap,
            limit=20,
        )

    def test_non_epic_parent_with_live_child_is_not_claimable(self) -> None:
        plan = self.plan(
            [
                issue("fm-parent", "W10: parent", priority=0),
                issue("fm-child", "W10: child", parent="fm-parent"),
            ]
        )
        self.assertTrue(plan["integrity"]["ok"])
        self.assertEqual(plan["recommendation"]["issue"]["id"], "fm-child")
        self.assertEqual(plan["ready_containers"][0]["id"], "fm-parent")
        self.assertEqual(plan["ready_containers"][0]["live_children"], ["fm-child"])
        self.assertEqual(plan["counts"]["non_epic_containers"], 1)

    def test_closed_child_releases_parent_and_assigned_or_epic_never_win(self) -> None:
        released = self.plan(
            [
                issue("fm-parent", "W10: parent"),
                issue("fm-child", "W10: child", "closed", parent="fm-parent"),
            ]
        )
        self.assertEqual(released["recommendation"]["issue"]["id"], "fm-parent")

        guarded = self.plan(
            [
                issue("fm-assigned", "W10: assigned", priority=0, assignee="peer"),
                issue("fm-epic", "W10: epic", priority=0, issue_type="epic"),
                issue("fm-leaf", "W10: leaf", priority=2),
            ]
        )
        self.assertEqual(guarded["recommendation"]["issue"]["id"], "fm-leaf")

    def test_active_workstream_precedes_new_activation(self) -> None:
        plan = self.plan(
            [
                issue("fm-active", "W10: active", "in_progress", assignee="agent"),
                issue("fm-same", "W10: same", priority=2),
                issue("fm-new", "W11: new", priority=1),
            ]
        )
        self.assertEqual(plan["recommendation"]["issue"]["id"], "fm-same")
        self.assertFalse(plan["recommendation"]["activates_workstream"])

    def test_same_priority_prefers_immediate_unblocking(self) -> None:
        plan = self.plan(
            [
                issue("fm-a", "W10: blocker", priority=1),
                issue("fm-b", "W10: ordinary", priority=1),
                issue("fm-d1", "W10: dependent", blockers=("fm-a",)),
                issue("fm-d2", "W10: dependent", blockers=("fm-a",)),
            ]
        )
        selected = plan["recommendation"]["issue"]
        self.assertEqual(selected["id"], "fm-a")
        self.assertEqual(selected["immediate_unblocks"], 2)

    def test_unscoped_open_leaf_is_visible_but_never_claimable(self) -> None:
        rows = [issue("fm-unscoped", "Maintenance without a workstream", priority=0)]
        plan = self.plan(rows)
        self.assertTrue(plan["integrity"]["ok"])
        self.assertIsNone(plan["recommendation"]["issue"])
        self.assertIn("only unscoped leaves remain", plan["recommendation"]["reason"])
        self.assertEqual(plan["counts"]["claimable_leaves"], 0)
        self.assertEqual(plan["counts"]["unscoped_leaves"], 1)
        self.assertEqual(plan["unscoped_leaves"][0]["id"], "fm-unscoped")

        path = self.ledger(rows)
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = agent_next.main(["--ledger", str(path), "--require"])
        self.assertEqual(status, 3)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("no claimable recommendation", stderr.getvalue())

    def test_scoped_leaf_wins_over_a_higher_priority_unscoped_leaf(self) -> None:
        plan = self.plan(
            [
                issue("fm-unscoped", "Maintenance without a workstream", priority=0),
                issue("fm-scoped", "W10: governed work", priority=3),
            ]
        )
        self.assertTrue(plan["integrity"]["ok"])
        self.assertEqual(plan["recommendation"]["issue"]["id"], "fm-scoped")
        self.assertEqual(plan["counts"]["claimable_leaves"], 1)
        self.assertEqual(plan["counts"]["unscoped_leaves"], 1)

    def test_unscoped_active_claim_invalidates_the_plan(self) -> None:
        rows = [
            issue(
                "fm-unscoped-active",
                "Maintenance without a workstream",
                "in_progress",
                assignee="agent",
            ),
            issue("fm-scoped", "W10: governed work"),
        ]
        plan = self.plan(rows)
        self.assertFalse(plan["integrity"]["ok"])
        self.assertEqual(
            plan["integrity"]["unscoped_active"], ["fm-unscoped-active"]
        )
        self.assertEqual(plan["counts"]["unscoped_active"], 1)
        self.assertIsNone(plan["recommendation"]["issue"])
        self.assertIn("unscoped active claims", plan["recommendation"]["reason"])

        path = self.ledger(rows)
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = agent_next.main(["--ledger", str(path), "--require"])
        self.assertEqual(status, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("task-graph integrity failed", stderr.getvalue())

    def test_parent_child_cycles_fail_before_plan_publication(self) -> None:
        rows = [
            issue("fm-a", "W10: a", parent="fm-b"),
            issue("fm-b", "W10: b", parent="fm-a"),
        ]
        plan = self.plan(rows)
        self.assertFalse(plan["integrity"]["ok"])
        self.assertTrue(plan["integrity"]["blocking_within_contract"])
        self.assertEqual(plan["integrity"]["containment_cycles"], [["fm-a", "fm-b"]])
        self.assertEqual(plan["counts"]["containment_cycles"], 1)
        self.assertIsNone(plan["recommendation"]["issue"])
        self.assertIn("containment cycles", plan["recommendation"]["reason"])

        path = self.ledger(rows)
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = agent_next.main(["--ledger", str(path), "--require"])
        self.assertEqual(status, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("task-graph integrity failed", stderr.getvalue())

    def test_deep_parent_child_chain_is_iterative_and_selects_the_true_leaf(self) -> None:
        rows = [
            issue(
                f"fm-{index:04d}",
                "W10: hierarchy",
                parent=f"fm-{index - 1:04d}" if index else None,
            )
            for index in range(1_500)
        ]
        plan = self.plan(rows)
        self.assertTrue(plan["integrity"]["ok"])
        self.assertEqual(plan["integrity"]["containment_cycles"], [])
        self.assertEqual(plan["recommendation"]["issue"]["id"], "fm-1499")
        self.assertEqual(plan["counts"]["ready_containers"], 1_499)
        self.assertEqual(plan["counts"]["claimable_leaves"], 1)

    def test_integrity_and_activation_fail_closed(self) -> None:
        bad = self.ledger([issue("fm-bad", "W10: bad", blockers=("fm-missing",))])
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = agent_next.main(["--ledger", str(bad), "--require"])
        self.assertEqual(status, 1)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("integrity failed", stderr.getvalue())

        rows = [
            issue(f"fm-{n}", f"W{n}: active", "in_progress", assignee="agent")
            for n in range(1, 5)
        ]
        rows.append(issue("fm-new", "W9: new"))
        capped = self.ledger(rows)
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(io.StringIO()):
            status = agent_next.main(["--ledger", str(capped), "--require"])
        self.assertEqual(status, 3)
        self.assertEqual(stdout.getvalue(), "")

    def test_machine_json_is_canonical_and_require_has_distinct_empty_exit(self) -> None:
        path = self.ledger([issue("fm-next", "W10: next", priority=1)])
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            status = agent_next.main(
                ["--ledger", str(path), "--format", "json", "--as-of", "2026-08-31T00:00:00Z"]
            )
        self.assertEqual(status, 0)
        self.assertTrue(stdout.getvalue().endswith("\n"))
        payload = json.loads(stdout.getvalue())
        self.assertEqual(
            stdout.getvalue(),
            json.dumps(payload, sort_keys=True, separators=(",", ":")) + "\n",
        )
        self.assertEqual(payload["schema"], "fmn.agent.next")
        self.assertEqual(payload["version"], 3)
        self.assertTrue(payload["integrity"]["ok"])
        self.assertEqual(payload["recommendation"]["issue"]["id"], "fm-next")

        required = io.StringIO()
        with contextlib.redirect_stdout(required):
            status = agent_next.main(["--ledger", str(path), "--require"])
        self.assertEqual(status, 0)
        self.assertEqual(required.getvalue(), "fm-next\n")

        empty = self.ledger([issue("fm-done", "W10: done", "closed")])
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(io.StringIO()):
            status = agent_next.main(["--ledger", str(empty), "--require"])
        self.assertEqual(status, 3)
        self.assertEqual(stdout.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
