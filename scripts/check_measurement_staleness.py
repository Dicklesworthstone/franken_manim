#!/usr/bin/env python3
"""Fail when a measured dashboard is older than its limit (fm-dashboard-staleness-dew9).

Each generated dashboard in DASHBOARDS carries one machine-readable header line:

    <!-- fmn-measurement {"measured_commit": "...", "measured_at": "YYYY-MM-DD",
         "producer": "...", "inputs_digest": "...", "max_age_commits": 200} -->

The age is `git rev-list --count <measured_commit>..HEAD`. A dashboard the README
cites has a limit of at most README_MAX_AGE, so a headline number cannot drift far
from the code it describes. A record kept for history says
"status": "historical" with "max_age_commits": null. It has no age limit, but the
README must not link it, because history is not a current claim.

One NDJSON line per dashboard: path, measured_commit, age_commits, max_age,
verdict (fresh, stale, historical, missing-header, malformed, unknown-commit,
historical-cited). The exit code is 1 if any verdict is a failure, and a
failure line names the producer command to re-run.
"""
from __future__ import annotations

import argparse
import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
DASHBOARDS = (
    "docs/ratchet/scene_dashboard.md",
    "docs/ratchet/differential_dashboard.md",
    "docs/ratchet/speed_ratio.md",
    "docs/ratchet/speed_ratio_frame_grid.md",
    "docs/ratchet/offload_payoff.md",
)
README_MAX_AGE = 50
DEFAULT_MAX_AGE = 200
HEADER = re.compile(r"^<!-- fmn-measurement (\{.*\}) -->$", re.M)
FIELDS = ("measured_commit", "measured_at", "producer", "inputs_digest", "max_age_commits")
FAILURES = ("stale", "missing-header", "malformed", "unknown-commit", "historical-cited")


def header_line(measured_commit, measured_at, producer, inputs_digest,
                max_age_commits=DEFAULT_MAX_AGE, status=None):
    """The header a producer writes as its dashboard's first line."""
    fields = {"measured_commit": measured_commit, "measured_at": measured_at,
              "producer": producer, "inputs_digest": inputs_digest,
              "max_age_commits": max_age_commits}
    if status is not None:
        fields["status"] = status
    return f"<!-- fmn-measurement {json.dumps(fields, sort_keys=True)} -->"


def producer_header(measured_commit, input_ids, argv=None, max_age_commits=DEFAULT_MAX_AGE):
    """The header a producer stamps on a dashboard it writes.

    It records today's date, the producer's own command line, and a digest of the
    measured inputs (the sorted scene ids). `measured_commit` is the commit the
    measured engine was built from, which the operator passes (--measured-commit),
    because a wheel does not report it. Without it a producer writes no header, and
    this check fails the dashboard as missing-header.
    """
    import datetime
    import hashlib
    digest = hashlib.sha256("\n".join(sorted(input_ids)).encode("utf-8")).hexdigest()[:16]
    command = "python3 " + " ".join(argv if argv is not None else sys.argv)
    return header_line(measured_commit, datetime.date.today().isoformat(), command,
                       f"sha256:{digest} over {len(input_ids)} scene ids", max_age_commits)


def read_header(text):
    """(fields, None) from a dashboard's text, or (None, reason)."""
    found = HEADER.findall(text)
    if not found:
        return None, "missing-header"
    if len(found) > 1:
        return None, "malformed"
    try:
        fields = json.loads(found[0])
    except json.JSONDecodeError:
        return None, "malformed"
    if not isinstance(fields, dict) or any(key not in fields for key in FIELDS):
        return None, "malformed"
    limit, status = fields["max_age_commits"], fields.get("status")
    if status == "historical":
        if limit is not None:
            return None, "malformed"
    elif status is not None or not isinstance(limit, int) or isinstance(limit, bool) or limit < 0:
        return None, "malformed"
    if not re.fullmatch(r"[0-9a-f]{7,40}", str(fields["measured_commit"])):
        return None, "malformed"
    return fields, None


def commit_age(repo, commit):
    """Commits from `commit` to HEAD, or None if `commit` is not an ancestor of HEAD."""
    ancestor = subprocess.run(["git", "-C", str(repo), "merge-base", "--is-ancestor", commit, "HEAD"],
                              capture_output=True, check=False)
    if ancestor.returncode != 0:
        return None
    count = subprocess.run(["git", "-C", str(repo), "rev-list", "--count", f"{commit}..HEAD"],
                           capture_output=True, text=True, check=False)
    return int(count.stdout.strip()) if count.returncode == 0 else None


def check(repo, dashboards=None, readme="README.md"):
    """One verdict record per dashboard (DASHBOARDS by default)."""
    repo = pathlib.Path(repo)
    dashboards = DASHBOARDS if dashboards is None else dashboards
    readme_path = repo / readme
    readme_text = readme_path.read_text(encoding="utf-8") if readme_path.exists() else ""
    records = []
    for relative in dashboards:
        path = repo / relative
        record = {"path": relative, "measured_commit": None, "age_commits": None, "max_age": None}
        fields, problem = read_header(path.read_text(encoding="utf-8")) if path.exists() \
            else (None, "missing-header")
        cited = pathlib.PurePosixPath(relative).name in readme_text
        if problem:
            records.append(dict(record, verdict=problem))
            continue
        record["measured_commit"] = fields["measured_commit"]
        record["producer"] = fields["producer"]
        age = commit_age(repo, fields["measured_commit"])
        record["age_commits"] = age
        if fields.get("status") == "historical":
            records.append(dict(record, verdict="historical-cited" if cited else "historical"))
            continue
        limit = min(fields["max_age_commits"], README_MAX_AGE) if cited else fields["max_age_commits"]
        record["max_age"] = limit
        if age is None:
            verdict = "unknown-commit"
        else:
            verdict = "stale" if age > limit else "fresh"
        records.append(dict(record, verdict=verdict))
    return records


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--repo", default=str(ROOT))
    args = parser.parse_args(argv)
    records = check(args.repo)
    failed = False
    for record in records:
        print(json.dumps(record, sort_keys=True))
        if record["verdict"] in FAILURES:
            failed = True
            rerun = record.get("producer") or "the dashboard's producer"
            print(f"stale measurement: {record['path']} is {record['verdict']}"
                  f" (age {record['age_commits']}, limit {record['max_age']}); re-run: {rerun}",
                  file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
