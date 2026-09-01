from __future__ import annotations

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import agent_claim_guard as guard


def record(
    issue_id: str,
    *,
    title: str = "W10: ready",
    status: str = "open",
    priority: int = 2,
    issue_type: str = "task",
    assignee: str | None = None,
    updated_at: str = "2026-08-31T08:15:00Z",
    dependencies: list[dict] | None = None,
    comments: list[dict] | None = None,
) -> dict:
    row = {
        "id": issue_id,
        "title": title,
        "status": status,
        "priority": priority,
        "issue_type": issue_type,
        "created_at": "2026-08-01T00:00:00Z",
        "updated_at": updated_at,
    }
    if assignee is not None:
        row["assignee"] = assignee
    if dependencies is not None:
        row["dependencies"] = dependencies
    if comments is not None:
        row["comments"] = comments
    return row


def dependency(owner: str, target: str, kind: str = "blocks") -> dict:
    return {"issue_id": owner, "depends_on_id": target, "type": kind}


class AgentClaimGuardTests(unittest.TestCase):
    def ledger(self, rows: list[dict]) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        path = Path(directory.name) / "issues.jsonl"
        path.write_text(
            "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows),
            encoding="utf-8",
        )
        return path

    def invoke(self, path: Path, *arguments: str) -> tuple[int, str, str]:
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = guard.main(["--ledger", str(path), *arguments])
        return status, stdout.getvalue(), stderr.getvalue()

    def test_token_round_trip_revalidates_the_same_graph_and_recommendation(self) -> None:
        path = self.ledger([record("fm-next", priority=1)])
        status, output, error = self.invoke(path, "--format", "token", "--require")
        self.assertEqual(status, 0)
        self.assertEqual(error, "")
        token = output.strip()
        self.assertRegex(token, r"^v1:[0-9a-f]{64}:fm-next$")

        status, output, error = self.invoke(
            path,
            "--format",
            "json",
            "--expect-token",
            token,
            "--require",
        )
        self.assertEqual(status, 0)
        self.assertEqual(error, "")
        payload = json.loads(output)
        self.assertEqual(payload["schema"], "fmn.agent.claim-guard")
        self.assertEqual(payload["version"], 1)
        self.assertEqual(payload["token"], token)
        self.assertEqual(payload["recommendation_id"], "fm-next")

    def test_any_authoritative_state_change_invalidates_the_token(self) -> None:
        path = self.ledger([record("fm-next", comments=[{"text": "first"}])])
        status, output, _error = self.invoke(path)
        self.assertEqual(status, 0)
        token = output.strip()

        path.write_text(
            json.dumps(
                record("fm-next", comments=[{"text": "first"}, {"text": "new handoff"}]),
                separators=(",", ":"),
            )
            + "\n",
            encoding="utf-8",
        )
        status, output, error = self.invoke(path, "--expect-token", token)
        self.assertEqual(status, 4)
        self.assertEqual(output, "")
        self.assertIn("claim token is stale", error)

    def test_recommendation_change_invalidates_the_token(self) -> None:
        path = self.ledger(
            [
                record("fm-first", priority=1),
                record("fm-second", priority=2),
            ]
        )
        status, output, _error = self.invoke(path)
        self.assertEqual(status, 0)
        token = output.strip()
        self.assertTrue(token.endswith(":fm-first"))

        path.write_text(
            "".join(
                json.dumps(row, separators=(",", ":")) + "\n"
                for row in [
                    record("fm-first", priority=3),
                    record("fm-second", priority=1),
                ]
            ),
            encoding="utf-8",
        )
        status, output, error = self.invoke(path, "--expect-token", token)
        self.assertEqual(status, 4)
        self.assertEqual(output, "")
        self.assertIn("claim token is stale", error)

    def test_digest_is_independent_of_issue_and_dependency_row_order(self) -> None:
        parent = record("fm-parent", status="closed")
        child_dependencies = [
            dependency("fm-child", "fm-parent", "blocks"),
            dependency("fm-child", "fm-container", "parent-child"),
        ]
        child = record("fm-child", dependencies=child_dependencies)
        container = record("fm-container", issue_type="epic")
        first = self.ledger([parent, child, container])
        second = self.ledger(
            [
                container,
                record("fm-child", dependencies=list(reversed(child_dependencies))),
                parent,
            ]
        )
        first_issues = guard.agent_brief.load_issues(first)
        second_issues = guard.agent_brief.load_issues(second)
        self.assertEqual(guard.graph_digest(first_issues), guard.graph_digest(second_issues))

    def test_no_recommendation_has_a_stable_none_token_and_require_exit(self) -> None:
        path = self.ledger(
            [record("fm-assigned", assignee="peer", priority=1)]
        )
        first = self.invoke(path, "--format", "token")
        second = self.invoke(path, "--format", "token")
        self.assertEqual(first, second)
        self.assertEqual(first[0], 0)
        self.assertRegex(first[1].strip(), r"^v1:[0-9a-f]{64}:none$")

        status, output, error = self.invoke(path, "--require")
        self.assertEqual(status, 3)
        self.assertEqual(output, "")
        self.assertIn("no claimable recommendation", error)

    def test_malformed_token_and_output_budget_fail_without_stdout(self) -> None:
        path = self.ledger([record("fm-next")])
        status, output, error = self.invoke(path, "--expect-token", "not-a-token")
        self.assertEqual(status, 2)
        self.assertEqual(output, "")
        self.assertIn("must have the form", error)

        with mock.patch.object(guard, "MAX_OUTPUT_BYTES", 8):
            status, output, error = self.invoke(path, "--format", "json")
        self.assertEqual(status, 2)
        self.assertEqual(output, "")
        self.assertIn("8-byte limit", error)

    def test_integrity_failure_precedes_expected_token_validation(self) -> None:
        path = self.ledger(
            [
                record(
                    "fm-bad",
                    dependencies=[dependency("fm-bad", "fm-absent")],
                )
            ]
        )
        status, output, error = self.invoke(path, "--expect-token", "malformed")
        self.assertEqual(status, 1)
        self.assertEqual(output, "")
        self.assertIn("task-graph integrity failed", error)
        self.assertNotIn("must have the form", error)


if __name__ == "__main__":
    unittest.main()
