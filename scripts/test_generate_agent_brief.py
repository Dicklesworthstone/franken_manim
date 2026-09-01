from __future__ import annotations

import contextlib
import io
import json
import tempfile
import unittest
from pathlib import Path

import generate_agent_brief as generator


def record(
    issue_id: str,
    title: str,
    status: str,
    *,
    updated_at: str,
    priority: int = 2,
    issue_type: str = "task",
    assignee: str | None = None,
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
    return row


class GenerateAgentBriefTests(unittest.TestCase):
    def fixture(self) -> tuple[Path, Path]:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        root = Path(directory.name)
        return root / "issues.jsonl", root / "AGENT_BRIEF.md"

    def write_ledger(self, path: Path, rows: list[dict]) -> None:
        path.write_text(
            "".join(json.dumps(row, separators=(",", ":")) + "\n" for row in rows),
            encoding="utf-8",
        )

    def rows(self) -> list[dict]:
        return [
            record(
                "fm-active",
                "W10: active",
                "in_progress",
                updated_at="2026-08-30T12:00:00Z",
                assignee="agent",
            ),
            record(
                "fm-next",
                "W10: ready",
                "open",
                updated_at="2026-08-31T08:15:00Z",
                priority=1,
            ),
        ]

    def test_newest_ledger_timestamp_is_the_deterministic_as_of(self) -> None:
        ledger, _output = self.fixture()
        self.write_ledger(ledger, self.rows())
        document, snapshot = generator.build_document(
            ledger, stale_days=2, activation_cap=4, limit=20
        )
        self.assertEqual(snapshot["as_of"], "2026-08-31T08:15:00Z")
        self.assertIn("As of `2026-08-31T08:15:00Z`", document)
        self.assertTrue(document.endswith("\n"))
        self.assertFalse(document.endswith("\n\n"))

    def test_identical_ledger_bytes_produce_identical_document_bytes(self) -> None:
        ledger, _output = self.fixture()
        self.write_ledger(ledger, self.rows())
        first, _ = generator.build_document(ledger, stale_days=2, activation_cap=4, limit=20)
        second, _ = generator.build_document(ledger, stale_days=2, activation_cap=4, limit=20)
        self.assertEqual(first.encode(), second.encode())

    def test_generate_then_check_succeeds_and_stale_or_missing_refuses(self) -> None:
        ledger, output = self.fixture()
        self.write_ledger(ledger, self.rows())
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            status = generator.main(["--ledger", str(ledger), "--output", str(output)])
        self.assertEqual(status, 0)
        self.assertTrue(output.is_file())
        self.assertIn("agent brief generated", stdout.getvalue())

        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            status = generator.main(
                ["--ledger", str(ledger), "--output", str(output), "--check"]
            )
        self.assertEqual(status, 0)
        self.assertIn("agent brief current", stdout.getvalue())

        output.write_text("stale\n", encoding="utf-8")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            status = generator.main(
                ["--ledger", str(ledger), "--output", str(output), "--check"]
            )
        self.assertEqual(status, 1)
        self.assertIn("is stale", stderr.getvalue())
        _unused_ledger, missing_output = self.fixture()
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            status = generator.main(
                ["--ledger", str(ledger), "--output", str(missing_output), "--check"]
            )
        self.assertEqual(status, 1)
        self.assertIn("is missing", stderr.getvalue())

    def test_malformed_or_empty_ledger_never_overwrites_existing_output(self) -> None:
        ledger, output = self.fixture()
        output.write_text("sentinel\n", encoding="utf-8")
        ledger.write_text("not-json\n", encoding="utf-8")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            status = generator.main(["--ledger", str(ledger), "--output", str(output)])
        self.assertEqual(status, 1)
        self.assertIn("invalid UTF-8 JSON", stderr.getvalue())
        self.assertEqual(output.read_text(encoding="utf-8"), "sentinel\n")

        ledger.write_text("", encoding="utf-8")
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            status = generator.main(["--ledger", str(ledger), "--output", str(output)])
        self.assertEqual(status, 1)
        self.assertIn("contains no issues", stderr.getvalue())
        self.assertEqual(output.read_text(encoding="utf-8"), "sentinel\n")

    def test_check_fails_when_activation_cap_is_breached(self) -> None:
        ledger, output = self.fixture()
        rows = [
            record(
                f"fm-{index}",
                f"W{index}: active",
                "in_progress",
                updated_at=f"2026-08-{20 + index:02d}T00:00:00Z",
                assignee="agent",
            )
            for index in range(1, 6)
        ]
        self.write_ledger(ledger, rows)
        first_stderr = io.StringIO()
        with contextlib.redirect_stderr(first_stderr):
            status = generator.main(["--ledger", str(ledger), "--output", str(output)])
        self.assertEqual(status, 1)
        self.assertIn("workstream cap breached", first_stderr.getvalue())
        self.assertTrue(output.is_file())
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            status = generator.main(
                ["--ledger", str(ledger), "--output", str(output), "--check"]
            )
        self.assertEqual(status, 1)
        self.assertIn("workstream cap breached", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
