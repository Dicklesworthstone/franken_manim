#!/usr/bin/env python3
"""Execute one bounded, tracker-native Beads operation from a strict JSON request."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any

REQUEST_SCHEMA = "fmn.agent.remote-operation"
REQUEST_VERSION = 1
REPORT_SCHEMA = "fmn.agent.remote-operation-report"
REPORT_VERSION = 1
MAX_REQUEST_BYTES = 128 * 1024
MAX_TEXT_BYTES = 64 * 1024
MAX_COMMAND_OUTPUT_BYTES = 8 * 1024 * 1024
ISSUE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,255}$")
ASSIGNEE_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._@/-]{0,255}$")
DEFAULT_INSPECT_ISSUES = ("fm-c53", "fm-5wq.4")


class RemoteOperationError(ValueError):
    pass


class DuplicateJsonKey(ValueError):
    pass


def strict_object(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise DuplicateJsonKey(key)
        value[key] = item
    return value


def read_request(path: Path | None) -> tuple[dict[str, Any], str | None]:
    if path is None:
        return {
            "schema": REQUEST_SCHEMA,
            "version": REQUEST_VERSION,
            "operation": "inspect",
            "issues": list(DEFAULT_INSPECT_ISSUES),
        }, None
    try:
        data = path.read_bytes()
    except OSError as exc:
        raise RemoteOperationError(f"cannot read request {path}: {exc}") from exc
    if len(data) > MAX_REQUEST_BYTES:
        raise RemoteOperationError(
            f"request exceeds the {MAX_REQUEST_BYTES}-byte limit ({len(data)} bytes)"
        )
    try:
        request = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=strict_object,
            parse_constant=lambda token: (_ for _ in ()).throw(
                RemoteOperationError(f"non-finite JSON number {token!r}")
            ),
        )
    except UnicodeDecodeError as exc:
        raise RemoteOperationError(f"request is not UTF-8: {path}") from exc
    except DuplicateJsonKey as exc:
        raise RemoteOperationError(f"request contains duplicate key {exc.args[0]!r}") from exc
    except json.JSONDecodeError as exc:
        raise RemoteOperationError(
            f"request is not valid JSON at line {exc.lineno}, column {exc.colno}: {exc.msg}"
        ) from exc
    if not isinstance(request, dict):
        raise RemoteOperationError("request root must be a JSON object")
    return request, str(path)


def require_exact_keys(
    request: dict[str, Any],
    required: set[str],
    optional: set[str] = frozenset(),
) -> None:
    keys = set(request)
    missing = sorted(required - keys)
    unknown = sorted(keys - required - optional)
    if missing:
        raise RemoteOperationError(f"request is missing required keys: {', '.join(missing)}")
    if unknown:
        raise RemoteOperationError(f"request contains unknown keys: {', '.join(unknown)}")


def require_text(
    request: dict[str, Any],
    key: str,
    *,
    pattern: re.Pattern[str] | None = None,
    max_bytes: int = MAX_TEXT_BYTES,
) -> str:
    value = request.get(key)
    if not isinstance(value, str) or not value or value.strip() != value:
        raise RemoteOperationError(f"{key} must be a non-empty string without outer whitespace")
    if "\x00" in value:
        raise RemoteOperationError(f"{key} must not contain NUL")
    size = len(value.encode("utf-8"))
    if size > max_bytes:
        raise RemoteOperationError(f"{key} exceeds the {max_bytes}-byte limit ({size} bytes)")
    if pattern is not None and pattern.fullmatch(value) is None:
        raise RemoteOperationError(f"{key} has an invalid format")
    return value


def validate_request(request: dict[str, Any]) -> dict[str, Any]:
    if request.get("schema") != REQUEST_SCHEMA:
        raise RemoteOperationError(f"schema must be {REQUEST_SCHEMA!r}")
    if request.get("version") != REQUEST_VERSION:
        raise RemoteOperationError(f"version must be {REQUEST_VERSION}")
    operation = request.get("operation")
    if operation == "inspect":
        require_exact_keys(request, {"schema", "version", "operation"}, {"issues"})
        issues = request.get("issues", list(DEFAULT_INSPECT_ISSUES))
        if not isinstance(issues, list) or len(issues) > 100:
            raise RemoteOperationError("issues must be an array containing at most 100 issue IDs")
        normalized: list[str] = []
        seen: set[str] = set()
        for index, value in enumerate(issues):
            if not isinstance(value, str) or ISSUE_ID_RE.fullmatch(value) is None:
                raise RemoteOperationError(f"issues[{index}] is not a valid issue ID")
            if value in seen:
                raise RemoteOperationError(f"issues contains duplicate ID {value!r}")
            seen.add(value)
            normalized.append(value)
        return {
            "schema": REQUEST_SCHEMA,
            "version": REQUEST_VERSION,
            "operation": operation,
            "issues": normalized,
        }
    if operation == "claim_recommended":
        require_exact_keys(
            request,
            {
                "schema",
                "version",
                "operation",
                "expected_issue",
                "assignee",
                "transition_comment",
            },
        )
        return {
            "schema": REQUEST_SCHEMA,
            "version": REQUEST_VERSION,
            "operation": operation,
            "expected_issue": require_text(request, "expected_issue", pattern=ISSUE_ID_RE),
            "assignee": require_text(
                request,
                "assignee",
                pattern=ASSIGNEE_RE,
                max_bytes=256,
            ),
            "transition_comment": require_text(request, "transition_comment"),
        }
    if operation == "close":
        require_exact_keys(
            request,
            {"schema", "version", "operation", "issue", "reason"},
            {"expected_assignee"},
        )
        normalized = {
            "schema": REQUEST_SCHEMA,
            "version": REQUEST_VERSION,
            "operation": operation,
            "issue": require_text(request, "issue", pattern=ISSUE_ID_RE),
            "reason": require_text(request, "reason"),
        }
        if "expected_assignee" in request:
            normalized["expected_assignee"] = require_text(
                request,
                "expected_assignee",
                pattern=ASSIGNEE_RE,
                max_bytes=256,
            )
        return normalized
    raise RemoteOperationError("operation must be one of: inspect, claim_recommended, close")


def run_command(argv: list[str], cwd: Path) -> tuple[Any, dict[str, Any]]:
    try:
        completed = subprocess.run(
            argv,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=300,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise RemoteOperationError(f"command failed to execute: {argv!r}: {exc}") from exc
    if (
        len(completed.stdout) > MAX_COMMAND_OUTPUT_BYTES
        or len(completed.stderr) > MAX_COMMAND_OUTPUT_BYTES
    ):
        raise RemoteOperationError(f"command output exceeded limit: {argv!r}")
    stdout = completed.stdout.decode("utf-8", errors="replace")
    stderr = completed.stderr.decode("utf-8", errors="replace")
    receipt = {"argv": argv, "returncode": completed.returncode, "stderr": stderr}
    if completed.returncode != 0:
        raise RemoteOperationError(
            f"command returned {completed.returncode}: {argv!r}: {stderr.strip() or stdout.strip()}"
        )
    try:
        payload = json.loads(stdout)
    except json.JSONDecodeError as exc:
        raise RemoteOperationError(f"command did not emit JSON: {argv!r}: {exc}") from exc
    receipt["stdout_bytes"] = len(completed.stdout)
    return payload, receipt


def run_text_command(argv: list[str], cwd: Path) -> tuple[str, dict[str, Any]]:
    try:
        completed = subprocess.run(
            argv,
            cwd=cwd,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=300,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise RemoteOperationError(f"command failed to execute: {argv!r}: {exc}") from exc
    if (
        len(completed.stdout) > MAX_COMMAND_OUTPUT_BYTES
        or len(completed.stderr) > MAX_COMMAND_OUTPUT_BYTES
    ):
        raise RemoteOperationError(f"command output exceeded limit: {argv!r}")
    stdout = completed.stdout.decode("utf-8", errors="replace")
    stderr = completed.stderr.decode("utf-8", errors="replace")
    receipt = {
        "argv": argv,
        "returncode": completed.returncode,
        "stderr": stderr,
        "stdout_bytes": len(completed.stdout),
    }
    if completed.returncode != 0:
        raise RemoteOperationError(
            f"command returned {completed.returncode}: {argv!r}: {stderr.strip() or stdout.strip()}"
        )
    return stdout.strip(), receipt


def issue_object(payload: Any, issue_id: str) -> dict[str, Any]:
    candidates: list[Any] = [payload]
    if isinstance(payload, dict):
        candidates.extend(payload.get(key) for key in ("issue", "data", "result"))
        issues = payload.get("issues")
        if isinstance(issues, list):
            candidates.extend(issues)
    elif isinstance(payload, list):
        candidates.extend(payload)
    for candidate in candidates:
        if isinstance(candidate, dict) and candidate.get("id") == issue_id:
            return candidate
    raise RemoteOperationError(f"br show did not return issue {issue_id!r}")


def inspect_state(repo: Path, issues: list[str]) -> tuple[dict[str, Any], list[dict[str, Any]]]:
    commands: list[dict[str, Any]] = []
    python = sys.executable
    brief, receipt = run_command(
        [python, "scripts/agent_brief.py", "--format", "json", "--check"], repo
    )
    commands.append(receipt)
    plan, receipt = run_command(
        [python, "scripts/agent_next.py", "--format", "json", "--check"], repo
    )
    commands.append(receipt)
    guard, receipt = run_command(
        [python, "scripts/agent_claim_guard.py", "--format", "json"], repo
    )
    commands.append(receipt)
    shown: dict[str, Any] = {}
    for issue_id in issues:
        payload, receipt = run_command(["br", "show", issue_id, "--json"], repo)
        commands.append(receipt)
        shown[issue_id] = issue_object(payload, issue_id)
    return {"brief": brief, "plan": plan, "claim_guard": guard, "issues": shown}, commands


def execute(
    repo: Path,
    request: dict[str, Any],
) -> tuple[dict[str, Any], list[dict[str, Any]], str | None]:
    operation = request["operation"]
    if operation == "inspect":
        state, commands = inspect_state(repo, request["issues"])
        return state, commands, None
    if operation == "claim_recommended":
        expected = request["expected_issue"]
        state, commands = inspect_state(repo, [expected])
        guard = state["claim_guard"]
        if not isinstance(guard, dict) or guard.get("recommendation_id") != expected:
            actual = guard.get("recommendation_id") if isinstance(guard, dict) else None
            raise RemoteOperationError(
                f"planner recommendation changed: expected {expected!r}, got {actual!r}"
            )
        token = guard.get("token")
        if not isinstance(token, str):
            raise RemoteOperationError("claim guard did not emit a token")
        claim, receipt = run_command(
            [
                sys.executable,
                "scripts/agent_claim.py",
                "--expect-token",
                token,
                "--issue",
                expected,
                "--assignee",
                request["assignee"],
                "--transition-comment",
                request["transition_comment"],
            ],
            repo,
        )
        commands.append(receipt)
        _, receipt = run_text_command(["br", "sync", "--flush-only"], repo)
        commands.append(receipt)
        shown, receipt = run_command(["br", "show", expected, "--json"], repo)
        commands.append(receipt)
        issue = issue_object(shown, expected)
        if issue.get("status") != "in_progress":
            raise RemoteOperationError(
                f"claim postcondition failed: status is {issue.get('status')!r}"
            )
        if issue.get("assignee") != request["assignee"]:
            raise RemoteOperationError(
                f"claim postcondition failed: assignee is {issue.get('assignee')!r}"
            )
        return {
            "pre_state": state,
            "claim": claim,
            "issue": issue,
        }, commands, f"chore(beads): claim {expected}"
    issue_id = request["issue"]
    pre_payload, pre_receipt = run_command(["br", "show", issue_id, "--json"], repo)
    pre_issue = issue_object(pre_payload, issue_id)
    if pre_issue.get("status") != "in_progress":
        raise RemoteOperationError(
            f"close requires in_progress status, got {pre_issue.get('status')!r}"
        )
    expected_assignee = request.get("expected_assignee")
    if expected_assignee is not None and pre_issue.get("assignee") != expected_assignee:
        raise RemoteOperationError(
            f"close assignee mismatch: expected {expected_assignee!r}, "
            f"got {pre_issue.get('assignee')!r}"
        )
    close_payload, close_receipt = run_command(
        ["br", "close", issue_id, "--reason", request["reason"], "--json"], repo
    )
    _, sync_receipt = run_text_command(["br", "sync", "--flush-only"], repo)
    post_payload, post_receipt = run_command(["br", "show", issue_id, "--json"], repo)
    post_issue = issue_object(post_payload, issue_id)
    if post_issue.get("status") != "closed":
        raise RemoteOperationError(
            f"close postcondition failed: status is {post_issue.get('status')!r}"
        )
    return {
        "pre_issue": pre_issue,
        "close": close_payload,
        "issue": post_issue,
    }, [pre_receipt, close_receipt, sync_receipt, post_receipt], f"chore(beads): close {issue_id}"


def write_report(path: Path, report: dict[str, Any]) -> None:
    text = json.dumps(
        report,
        ensure_ascii=False,
        allow_nan=False,
        sort_keys=True,
        indent=2,
    ) + "\n"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    sys.stdout.write(text)


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument("--request", type=Path)
    parser.add_argument("--report", type=Path, required=True)
    parser.add_argument("--commit-message", type=Path, required=True)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    try:
        repo = args.repo.resolve(strict=True)
        if not repo.is_dir():
            raise RemoteOperationError(f"repository root is not a directory: {repo}")
        request, request_path = read_request(args.request)
        request = validate_request(request)
        result, commands, commit_message = execute(repo, request)
        report = {
            "schema": REPORT_SCHEMA,
            "version": REPORT_VERSION,
            "ok": True,
            "head": os.environ.get("GITHUB_SHA"),
            "request_path": request_path,
            "request": request,
            "mutated": commit_message is not None,
            "result": result,
            "commands": commands,
        }
        args.commit_message.parent.mkdir(parents=True, exist_ok=True)
        args.commit_message.write_text(
            "" if commit_message is None else commit_message + "\n",
            encoding="utf-8",
        )
        write_report(args.report, report)
        return 0
    except (OSError, RemoteOperationError, TypeError, ValueError) as exc:
        report = {
            "schema": REPORT_SCHEMA,
            "version": REPORT_VERSION,
            "ok": False,
            "head": os.environ.get("GITHUB_SHA"),
            "request_path": str(args.request) if args.request is not None else None,
            "error": str(exc),
        }
        try:
            write_report(args.report, report)
        except OSError:
            print(json.dumps(report, sort_keys=True), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
