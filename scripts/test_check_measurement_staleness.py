#!/usr/bin/env python3
"""Unit tests for scripts/check_measurement_staleness.py on a scratch git repository."""
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import check_measurement_staleness as cms  # noqa: E402


def git(repo, *argv):
    return subprocess.run(["git", "-C", str(repo), *argv], capture_output=True, text=True,
                          check=True).stdout.strip()


class StalenessTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = pathlib.Path(self.tmp.name)
        git(self.repo, "init", "-q")
        git(self.repo, "config", "user.email", "test@example.invalid")
        git(self.repo, "config", "user.name", "test")
        self.commits = []
        for index in range(6):
            (self.repo / "file.txt").write_text(str(index))
            git(self.repo, "add", "file.txt")
            git(self.repo, "commit", "-q", "-m", f"c{index}")
            self.commits.append(git(self.repo, "rev-parse", "--short=12", "HEAD"))

    def tearDown(self):
        self.tmp.cleanup()

    def dashboard(self, name, text):
        path = self.repo / "docs" / name
        path.parent.mkdir(exist_ok=True)
        path.write_text(text)
        return f"docs/{name}"

    def header(self, commit, limit=3, status=None):
        return cms.header_line(commit, "2026-10-22", f"python3 make_{commit}.py", "sha256:00",
                               limit, status) + "\n# Dashboard\n"

    def verdicts(self, names, readme=""):
        (self.repo / "README.md").write_text(readme)
        return {record["path"]: record for record in cms.check(self.repo, names)}

    def test_fresh_stale_malformed_and_unknown_commit(self):
        fresh = self.dashboard("fresh.md", self.header(self.commits[3], limit=3))  # age 2
        stale = self.dashboard("stale.md", self.header(self.commits[0], limit=3))  # age 5
        missing = self.dashboard("missing.md", "# no header\n")
        malformed = self.dashboard("bad.md", "<!-- fmn-measurement {\"measured_commit\": 1} -->\n")
        unknown = self.dashboard("unknown.md", self.header("deadbeefcafe", limit=3))
        records = self.verdicts([fresh, stale, missing, malformed, unknown, "docs/absent.md"])
        self.assertEqual(records[fresh]["verdict"], "fresh")
        self.assertEqual(records[fresh]["age_commits"], 2)
        self.assertEqual((records[stale]["verdict"], records[stale]["age_commits"]), ("stale", 5))
        self.assertEqual(records[missing]["verdict"], "missing-header")
        self.assertEqual(records[malformed]["verdict"], "malformed")
        self.assertEqual(records[unknown]["verdict"], "unknown-commit")
        self.assertEqual(records["docs/absent.md"]["verdict"], "missing-header")

    def test_a_readme_citation_caps_the_limit(self):
        cited = self.dashboard("cited.md", self.header(self.commits[0], limit=200))
        self.assertEqual(self.verdicts([cited])[cited]["verdict"], "fresh")
        original = cms.README_MAX_AGE
        cms.README_MAX_AGE = 4
        try:
            record = self.verdicts([cited], readme="see docs/cited.md\n")[cited]
        finally:
            cms.README_MAX_AGE = original
        self.assertEqual((record["verdict"], record["max_age"]), ("stale", 4))

    def test_historical_records_need_no_limit_but_must_not_be_cited(self):
        old = self.dashboard("old.md", self.header(self.commits[0], limit=None, status="historical"))
        self.assertEqual(self.verdicts([old])[old]["verdict"], "historical")
        self.assertEqual(self.verdicts([old], readme="[x](docs/old.md)")[old]["verdict"],
                         "historical-cited")
        # A historical record with a limit, or an unknown status, is malformed.
        self.dashboard("odd.md", self.header(self.commits[0], limit=5, status="historical"))
        self.dashboard("odd2.md", self.header(self.commits[0], limit=5, status="draft"))
        records = self.verdicts(["docs/odd.md", "docs/odd2.md"])
        self.assertEqual({record["verdict"] for record in records.values()}, {"malformed"})

    def test_main_fails_naming_the_producer_and_writes_ndjson(self):
        stale = self.dashboard("stale.md", self.header(self.commits[0], limit=1))
        original = cms.DASHBOARDS
        try:
            cms.DASHBOARDS = (stale,)
            stdout, stderr = __import__("io").StringIO(), __import__("io").StringIO()
            with __import__("contextlib").redirect_stdout(stdout), \
                    __import__("contextlib").redirect_stderr(stderr):
                code = cms.main(["--repo", str(self.repo)])
        finally:
            cms.DASHBOARDS = original
        self.assertEqual(code, 1)
        record = json.loads(stdout.getvalue())
        self.assertEqual((record["path"], record["verdict"], record["max_age"]), (stale, "stale", 1))
        self.assertIn(f"re-run: python3 make_{self.commits[0]}.py", stderr.getvalue())

    def test_a_producer_stamps_a_readable_header_only_when_told_the_commit(self):
        import corpus_speed_ratio as csr
        out = self.repo / "speed"
        out.mkdir()
        runs = [{"module": "m.py", "scene": "A", "engine": engine, "rep": 0, "seconds": seconds,
                 "exit": 0} for engine, seconds in (("portal", 1.0), ("reference", 2.0))]
        (out / "runs.ndjson").write_text("".join(json.dumps(r) + "\n" for r in runs))
        csr.write_dashboard(out, out / "unstamped.md")
        self.assertEqual(cms.read_header((out / "unstamped.md").read_text())[1], "missing-header")
        csr.write_dashboard(out, out / "stamped.md", measured_commit=self.commits[4])
        fields, problem = cms.read_header((out / "stamped.md").read_text())
        self.assertIsNone(problem)
        self.assertEqual(fields["measured_commit"], self.commits[4])
        self.assertTrue(fields["inputs_digest"].endswith("over 1 scene ids"))
        relative = str((out / "stamped.md").relative_to(self.repo))
        self.assertEqual(self.verdicts([relative])[relative]["verdict"], "fresh")

    def test_the_repository_dashboards_all_carry_a_header(self):
        for relative in cms.DASHBOARDS:
            fields, problem = cms.read_header((cms.ROOT / relative).read_text(encoding="utf-8"))
            self.assertIsNone(problem, relative)
            self.assertIn("producer", fields)


if __name__ == "__main__":
    unittest.main(verbosity=1)
