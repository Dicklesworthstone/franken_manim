from __future__ import annotations

import contextlib
import io
import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import agent_brief
import agent_claim
import agent_claim_guard
import agent_next


def record(
    issue_id: str,
    *,
    status: str = "open",
    assignee: str | None = None,
    priority: int = 1,
    dependencies: list[dict] | None = None,
    updated_at: str = "2026-09-01T00:00:00Z",
) -> dict:
    row = {
        "id": issue_id,
        "title": "W10: guarded claim fixture",
        "status": status,
        "priority": priority,
        "issue_type": "task",
        "created_at": "2026-08-01T00:00:00Z",
        "updated_at": updated_at,
    }
    if assignee is not None:
        row["assignee"] = assignee
    if dependencies is not None:
        row["dependencies"] = dependencies
    return row


def dependency(owner: str, target: str, kind: str = "blocks") -> dict:
    return {"issue_id": owner, "depends_on_id": target, "type": kind}


class Fixture:
    def __init__(self, rows: list[dict], *, worktree: bool = False):
        self.directory = tempfile.TemporaryDirectory()
        self.root = Path(self.directory.name) / "repo"
        self.root.mkdir()
        (self.root / ".beads").mkdir()
        if worktree:
            self.git_common_dir = Path(self.directory.name) / "git"
            self.git_dir = self.git_common_dir / "worktrees" / "fixture"
            self.git_dir.mkdir(parents=True)
            (self.git_dir / "commondir").write_text("../..\n", encoding="utf-8")
            relative = os.path.relpath(self.git_dir, self.root)
            (self.root / ".git").write_text(f"gitdir: {relative}\n", encoding="utf-8")
        else:
            self.git_dir = self.root / ".git"
            self.git_dir.mkdir()
            self.git_common_dir = self.git_dir
        self.ledger = self.root / ".beads" / "issues.jsonl"
        self.write(rows)

    def write(self, rows: list[dict]) -> None:
        self.ledger.write_text(
            "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows),
            encoding="utf-8",
        )

    def rows(self) -> list[dict]:
        return [json.loads(line) for line in self.ledger.read_text(encoding="utf-8").splitlines()]

    def token(self, **policy) -> str:
        issues = agent_brief.load_issues(self.ledger)
        guard = agent_claim_guard.build_guard(
            issues,
            as_of=agent_next.parse_as_of(policy.get("as_of"), issues),
            stale_days=policy.get("stale_days", 2),
            activation_cap=policy.get("activation_cap", 4),
            limit=policy.get("limit", 20),
        )
        return guard["token"]

    def sibling_worktree(self, name: str) -> Path:
        root = Path(self.directory.name) / name
        root.mkdir()
        (root / ".beads").mkdir()
        (root / ".beads" / "issues.jsonl").write_bytes(self.ledger.read_bytes())
        git_dir = self.git_common_dir / "worktrees" / name
        git_dir.mkdir(parents=True)
        (git_dir / "commondir").write_text("../..\n", encoding="utf-8")
        relative = os.path.relpath(git_dir, root)
        (root / ".git").write_text(f"gitdir: {relative}\n", encoding="utf-8")
        return root

    def cleanup(self) -> None:
        self.directory.cleanup()


class FakeRunner:
    def __init__(
        self,
        fixture: Fixture,
        *,
        update_code: int = 0,
        sync_code: int = 0,
        mutate: bool = True,
        stdout: bytes = b"",
        stderr: bytes = b"",
    ):
        self.fixture = fixture
        self.update_code = update_code
        self.sync_code = sync_code
        self.mutate = mutate
        self.stdout = stdout
        self.stderr = stderr
        self.calls: list[tuple[tuple[str, ...], Path]] = []

    def __call__(self, argv: tuple[str, ...], cwd: Path) -> agent_claim.CommandResult:
        self.calls.append((argv, cwd))
        if argv[1] == "update":
            if self.update_code == 0 and self.mutate:
                rows = self.fixture.rows()
                issue_id = argv[2]
                assignee = argv[argv.index("--assignee") + 1]
                for row in rows:
                    if row["id"] == issue_id:
                        row["status"] = "in_progress"
                        row["assignee"] = assignee
                        row["updated_at"] = "2026-09-01T00:00:01Z"
                self.fixture.write(rows)
            return agent_claim.CommandResult(self.update_code, self.stdout, self.stderr)
        if argv[1] == "sync":
            return agent_claim.CommandResult(self.sync_code, self.stdout, self.stderr)
        raise AssertionError(f"unexpected command: {argv}")


class AgentClaimTests(unittest.TestCase):
    def fixture(self, rows: list[dict], *, worktree: bool = False) -> Fixture:
        fixture = Fixture(rows, worktree=worktree)
        self.addCleanup(fixture.cleanup)
        return fixture

    def test_dry_run_revalidates_without_invoking_br_and_pins_exact_argv(self) -> None:
        fixture = self.fixture([record("fm-next")])
        runner = FakeRunner(fixture)
        receipt = agent_claim.execute_claim(
            fixture.root,
            fixture.token(),
            "agent@example.com",
            requested_issue="fm-next",
            transition_comment="claiming after reservation check",
            dry_run=True,
            runner=runner,
        )
        self.assertEqual(runner.calls, [])
        self.assertEqual(receipt["schema"], "fmn.agent.claim")
        self.assertEqual(receipt["version"], 1)
        self.assertEqual(receipt["mode"], "dry-run")
        self.assertFalse(receipt["claimed"])
        self.assertEqual(receipt["issue_id"], "fm-next")
        self.assertEqual(
            receipt["commands"],
            [
                [
                    "br",
                    "update",
                    "fm-next",
                    "--status",
                    "in_progress",
                    "--assignee",
                    "agent@example.com",
                    "--transition-comment",
                    "claiming after reservation check",
                ],
                ["br", "sync", "--flush-only"],
            ],
        )
        self.assertEqual(fixture.rows()[0]["status"], "open")
        self.assertTrue((fixture.git_common_dir / agent_claim.LOCK_FILE_NAME).is_file())

    def test_claim_runs_update_then_flush_and_verifies_the_postcondition(self) -> None:
        fixture = self.fixture([record("fm-next")])
        runner = FakeRunner(fixture)
        receipt = agent_claim.execute_claim(
            fixture.root,
            fixture.token(),
            "agent@example.com",
            runner=runner,
        )
        self.assertEqual([call[0][1] for call in runner.calls], ["update", "sync"])
        self.assertTrue(all(call[1] == fixture.root for call in runner.calls))
        self.assertTrue(receipt["claimed"])
        self.assertEqual(receipt["status"], "in_progress")
        self.assertRegex(receipt["before_claim_sha256"], r"^[0-9a-f]{64}$")
        self.assertRegex(receipt["before_graph_sha256"], r"^[0-9a-f]{64}$")
        self.assertRegex(receipt["after_graph_sha256"], r"^[0-9a-f]{64}$")
        self.assertNotEqual(receipt["before_graph_sha256"], receipt["after_graph_sha256"])
        row = fixture.rows()[0]
        self.assertEqual(row["status"], "in_progress")
        self.assertEqual(row["assignee"], "agent@example.com")

    def test_stale_token_and_issue_mismatch_never_invoke_br(self) -> None:
        fixture = self.fixture([record("fm-next"), record("fm-other", priority=2)])
        token = fixture.token()
        rows = fixture.rows()
        rows[0]["title"] = "W10: changed after token issuance"
        rows[0]["updated_at"] = "2026-09-01T00:00:02Z"
        fixture.write(rows)
        runner = FakeRunner(fixture)
        with self.assertRaises(agent_claim.ClaimError) as stale:
            agent_claim.execute_claim(fixture.root, token, "agent@example.com", runner=runner)
        self.assertEqual(stale.exception.exit_code, 4)
        self.assertEqual(runner.calls, [])

        fresh = fixture.token()
        with self.assertRaises(agent_claim.ClaimError) as mismatch:
            agent_claim.execute_claim(
                fixture.root,
                fresh,
                "agent@example.com",
                requested_issue="fm-other",
                runner=runner,
            )
        self.assertEqual(mismatch.exception.exit_code, 2)
        self.assertIn("disagrees", str(mismatch.exception))
        self.assertEqual(runner.calls, [])

    def test_update_failure_stops_before_sync(self) -> None:
        fixture = self.fixture([record("fm-next")])
        runner = FakeRunner(fixture, update_code=9, stderr=b"policy refused")
        with self.assertRaises(agent_claim.ClaimError) as raised:
            agent_claim.execute_claim(
                fixture.root,
                fixture.token(),
                "agent@example.com",
                runner=runner,
            )
        self.assertEqual(raised.exception.exit_code, 5)
        self.assertIn("policy refused", str(raised.exception))
        self.assertEqual([call[0][1] for call in runner.calls], ["update"])

    def test_sync_failure_and_missing_mutation_never_emit_a_success_receipt(self) -> None:
        fixture = self.fixture([record("fm-next")])
        sync_failure = FakeRunner(fixture, sync_code=7, stderr=b"flush failed")
        with self.assertRaises(agent_claim.ClaimError) as raised:
            agent_claim.execute_claim(
                fixture.root,
                fixture.token(),
                "agent@example.com",
                runner=sync_failure,
            )
        self.assertEqual(raised.exception.exit_code, 5)
        self.assertIn("flush failed", str(raised.exception))
        self.assertEqual([call[0][1] for call in sync_failure.calls], ["update", "sync"])

        fixture = self.fixture([record("fm-next")])
        no_mutation = FakeRunner(fixture, mutate=False)
        with self.assertRaises(agent_claim.ClaimError) as raised:
            agent_claim.execute_claim(
                fixture.root,
                fixture.token(),
                "agent@example.com",
                runner=no_mutation,
            )
        self.assertEqual(raised.exception.exit_code, 5)
        self.assertIn("postcondition failed", str(raised.exception))

    def test_integrity_and_no_recommendation_have_guard_compatible_exit_codes(self) -> None:
        bad = self.fixture(
            [record("fm-bad", dependencies=[dependency("fm-bad", "fm-missing")])]
        )
        with self.assertRaises(agent_claim.ClaimError) as raised:
            agent_claim.execute_claim(
                bad.root,
                "malformed",
                "agent@example.com",
                dry_run=True,
            )
        self.assertEqual(raised.exception.exit_code, 1)
        self.assertIn("integrity", str(raised.exception))

        assigned = self.fixture([record("fm-assigned", assignee="peer@example.com")])
        with self.assertRaises(agent_claim.ClaimError) as raised:
            agent_claim.execute_claim(
                assigned.root,
                assigned.token(),
                "agent@example.com",
                dry_run=True,
            )
        self.assertEqual(raised.exception.exit_code, 3)
        self.assertIn("no claimable recommendation", str(raised.exception))

    def test_worktree_gitdir_markers_resolve_to_a_persistent_shared_lock(self) -> None:
        fixture = self.fixture([record("fm-next")], worktree=True)
        receipt = agent_claim.execute_claim(
            fixture.root,
            fixture.token(),
            "agent@example.com",
            dry_run=True,
        )
        self.assertFalse(receipt["claimed"])
        lock = fixture.git_common_dir / agent_claim.LOCK_FILE_NAME
        self.assertTrue(lock.is_file())
        self.assertEqual(lock.read_bytes(), b"0")
        self.assertFalse((fixture.git_dir / agent_claim.LOCK_FILE_NAME).exists())

    @unittest.skipIf(os.name == "nt", "fcntl contention fixture is Unix-specific")
    def test_linked_worktrees_contend_on_the_same_common_lock(self) -> None:
        fixture = self.fixture([record("fm-next")], worktree=True)
        sibling = fixture.sibling_worktree("sibling")
        self.assertEqual(
            agent_claim.resolve_git_common_dir(fixture.root),
            agent_claim.resolve_git_common_dir(sibling),
        )
        with agent_claim.claim_lock(fixture.root):
            with self.assertRaises(agent_claim.ClaimError) as raised:
                with agent_claim.claim_lock(sibling):
                    self.fail("sibling worktree acquired the shared claim lock")
        self.assertEqual(raised.exception.exit_code, 5)
        self.assertIn("already in progress", str(raised.exception))

    @unittest.skipIf(os.name == "nt", "fcntl contention fixture is Unix-specific")
    def test_local_lock_contention_fails_before_guard_or_mutation(self) -> None:
        fixture = self.fixture([record("fm-next")])
        token = fixture.token()
        with agent_claim.claim_lock(fixture.root):
            with self.assertRaises(agent_claim.ClaimError) as raised:
                agent_claim.execute_claim(
                    fixture.root,
                    token,
                    "agent@example.com",
                    dry_run=True,
                )
        self.assertEqual(raised.exception.exit_code, 5)
        self.assertIn("already in progress", str(raised.exception))

    def test_runner_and_receipt_bounds_fail_closed(self) -> None:
        fixture = self.fixture([record("fm-next")])
        oversized = FakeRunner(
            fixture,
            update_code=1,
            stderr=b"x" * (agent_claim.MAX_COMMAND_OUTPUT_BYTES + 1),
        )
        with self.assertRaises(agent_claim.ClaimError) as raised:
            agent_claim.execute_claim(
                fixture.root,
                fixture.token(),
                "agent@example.com",
                runner=oversized,
            )
        self.assertEqual(raised.exception.exit_code, 5)
        self.assertIn("command-output limit", str(raised.exception))

        with mock.patch.object(agent_claim, "MAX_OUTPUT_BYTES", 8):
            with self.assertRaises(agent_claim.ClaimError) as raised:
                agent_claim.render_json({"schema": "too-large"})
        self.assertEqual(raised.exception.exit_code, 5)
        self.assertIn("8-byte limit", str(raised.exception))

    def test_cli_failures_publish_no_stdout(self) -> None:
        fixture = self.fixture([record("fm-next")])
        token = fixture.token()
        rows = fixture.rows()
        rows[0]["updated_at"] = "2026-09-01T00:00:05Z"
        fixture.write(rows)
        stdout = io.StringIO()
        stderr = io.StringIO()
        with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            status = agent_claim.main(
                [
                    "--repo",
                    str(fixture.root),
                    "--expect-token",
                    token,
                    "--assignee",
                    "agent@example.com",
                    "--dry-run",
                ]
            )
        self.assertEqual(status, 4)
        self.assertEqual(stdout.getvalue(), "")
        self.assertIn("claim token is stale", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
