from __future__ import annotations

import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import agent_claim


class AgentClaimSubprocessTests(unittest.TestCase):
    def cwd(self) -> Path:
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        return Path(directory.name)

    def run_python(self, source: str) -> agent_claim.CommandResult:
        return agent_claim.run_command((sys.executable, "-c", source), self.cwd())

    def test_exact_limit_is_retained_for_both_streams(self) -> None:
        with mock.patch.object(agent_claim, "MAX_COMMAND_OUTPUT_BYTES", 64):
            result = self.run_python(
                "import sys; "
                "sys.stdout.buffer.write(b'x' * 64); "
                "sys.stderr.buffer.write(b'y' * 64)"
            )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b"x" * 64)
        self.assertEqual(result.stderr, b"y" * 64)

    def test_stdout_overflow_is_detected_after_the_pipe_is_fully_drained(self) -> None:
        with mock.patch.object(agent_claim, "MAX_COMMAND_OUTPUT_BYTES", 64):
            with self.assertRaisesRegex(
                agent_claim.ClaimError,
                "stdout exceeded the 64-byte command-output limit",
            ):
                self.run_python(
                    "import sys; "
                    "sys.stdout.buffer.write(b'x' * 1048576); "
                    "sys.stderr.buffer.write(b'ok')"
                )

    def test_stderr_overflow_is_detected_without_deadlocking_on_stdout(self) -> None:
        with mock.patch.object(agent_claim, "MAX_COMMAND_OUTPUT_BYTES", 64):
            with self.assertRaisesRegex(
                agent_claim.ClaimError,
                "stderr exceeded the 64-byte command-output limit",
            ):
                self.run_python(
                    "import sys; "
                    "sys.stdout.buffer.write(b'ok'); "
                    "sys.stderr.buffer.write(b'y' * 1048576)"
                )

    def test_dual_large_streams_are_drained_concurrently_and_fail_closed(self) -> None:
        with mock.patch.object(agent_claim, "MAX_COMMAND_OUTPUT_BYTES", 64):
            with self.assertRaisesRegex(agent_claim.ClaimError, "command-output limit"):
                self.run_python(
                    "import sys, threading; "
                    "a=threading.Thread(target=lambda: sys.stdout.buffer.write(b'x' * 1048576)); "
                    "b=threading.Thread(target=lambda: sys.stderr.buffer.write(b'y' * 1048576)); "
                    "a.start(); b.start(); a.join(); b.join()"
                )

    def test_nonzero_exit_keeps_bounded_diagnostics(self) -> None:
        result = self.run_python(
            "import sys; sys.stderr.write('bounded failure'); raise SystemExit(7)"
        )
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stdout, b"")
        self.assertEqual(result.stderr, b"bounded failure")


if __name__ == "__main__":
    unittest.main()
