#!/usr/bin/env python3
"""Claim one guarded Beads leaf under a shared repository lock.

``agent_claim_guard`` proves that a recommendation still matches the parsed
Beads graph, planner semantics, policy, and schema contract. This command keeps
that proof, the ``br update``, the explicit JSONL flush, and postcondition
verification inside one process while holding an advisory lock in Git's shared
common directory. It narrows the compare-before-set interval and produces an
auditable receipt. It is not a distributed lease: Agent Mail reservations and
other clones must still be checked before invoking it.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import math
import os
import signal
import stat
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import BinaryIO, Callable, Iterator, Sequence

import agent_brief
import agent_claim_guard
import agent_next

SCHEMA = "fmn.agent.claim"
SCHEMA_VERSION = 4
MAX_OUTPUT_BYTES = 1024 * 1024
MAX_COMMAND_OUTPUT_BYTES = 1024 * 1024
MAX_COMMAND_TOTAL_OUTPUT_BYTES = 16 * 1024 * 1024
MAX_TOKEN_BYTES = 4096
MAX_ISSUE_ID_BYTES = 1024
MAX_ASSIGNEE_BYTES = 1024
MAX_TRANSITION_COMMENT_BYTES = 64 * 1024
MAX_GIT_PATH_FILE_BYTES = 4096
COMMAND_READ_CHUNK_BYTES = 64 * 1024
DEFAULT_COMMAND_TIMEOUT_SECONDS = 60.0
MAX_COMMAND_TIMEOUT_SECONDS = 60.0 * 60.0
COMMAND_TERMINATION_WAIT_SECONDS = 5.0
COMMAND_READER_JOIN_SECONDS = 5.0
COMMAND_WAIT_POLL_SECONDS = 0.05
LOCK_FILE_NAME = "fmn-agent-claim.lock"


class ClaimError(ValueError):
    def __init__(self, message: str, exit_code: int = 2):
        super().__init__(message)
        self.exit_code = exit_code


@dataclass(frozen=True)
class CommandResult:
    returncode: int
    stdout: bytes = b""
    stderr: bytes = b""


@dataclass
class _DrainResult:
    payload: bytearray
    overflow: bool = False
    error: OSError | None = None


class _OutputBudget:
    def __init__(self, limit: int):
        if isinstance(limit, bool) or not isinstance(limit, int) or limit < 1:
            raise ClaimError("command total-output limit must be a positive integer", 5)
        self.limit = limit
        self._produced = 0
        self._lock = threading.Lock()
        self.exceeded = threading.Event()

    def consume(self, count: int) -> None:
        with self._lock:
            self._produced += count
            if self._produced > self.limit:
                self.exceeded.set()

    @property
    def produced(self) -> int:
        with self._lock:
            return self._produced


Runner = Callable[[tuple[str, ...], Path], CommandResult]
LockFactory = Callable[[Path], contextlib.AbstractContextManager[Path]]


def _bounded_text(name: str, value: str | None, limit: int, *, required: bool) -> str | None:
    if value is None:
        if required:
            raise ClaimError(f"{name} is required")
        return None
    if not value or value.strip() != value:
        raise ClaimError(f"{name} must be non-empty and have no surrounding whitespace")
    if "\x00" in value:
        raise ClaimError(f"{name} must not contain NUL")
    size = len(value.encode("utf-8"))
    if size > limit:
        raise ClaimError(f"{name} exceeds the {limit}-byte limit ({size} bytes)")
    return value


def _command_timeout(value: float) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ClaimError("--command-timeout-seconds must be a finite number")
    timeout = float(value)
    if not math.isfinite(timeout):
        raise ClaimError("--command-timeout-seconds must be finite")
    if timeout <= 0:
        raise ClaimError("--command-timeout-seconds must be positive")
    if timeout > MAX_COMMAND_TIMEOUT_SECONDS:
        raise ClaimError(
            "--command-timeout-seconds exceeds the "
            f"{MAX_COMMAND_TIMEOUT_SECONDS:g}-second limit"
        )
    return timeout


def _repo_root(path: Path) -> Path:
    try:
        resolved = path.resolve(strict=True)
    except OSError as exc:
        raise ClaimError(f"cannot resolve repository root {path}: {exc}") from exc
    if not resolved.is_dir():
        raise ClaimError(f"repository root is not a directory: {resolved}")
    return resolved


def _read_git_path_file(path: Path, label: str) -> str:
    flags = os.O_RDONLY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise ClaimError(f"cannot open {label} {path}: {exc}") from exc
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise ClaimError(f"{label} is not a regular file: {path}")
        if metadata.st_size > MAX_GIT_PATH_FILE_BYTES:
            raise ClaimError(
                f"{label} exceeds the {MAX_GIT_PATH_FILE_BYTES}-byte limit: {path}"
            )
        chunks: list[bytes] = []
        remaining = MAX_GIT_PATH_FILE_BYTES + 1
        while remaining > 0:
            chunk = os.read(descriptor, remaining)
            if not chunk:
                break
            chunks.append(chunk)
            remaining -= len(chunk)
        data = b"".join(chunks)
        if len(data) > MAX_GIT_PATH_FILE_BYTES:
            raise ClaimError(
                f"{label} exceeds the {MAX_GIT_PATH_FILE_BYTES}-byte limit: {path}"
            )
        try:
            return data.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ClaimError(f"{label} is not UTF-8: {path}") from exc
    finally:
        os.close(descriptor)


def _resolve_git_directory(base: Path, target_text: str, marker: Path, label: str) -> Path:
    if not target_text or "\x00" in target_text:
        raise ClaimError(f"malformed {label} target: {marker}")
    target = Path(target_text)
    if not target.is_absolute():
        target = base / target
    try:
        target = target.resolve(strict=True)
        target_metadata = os.lstat(target)
    except OSError as exc:
        raise ClaimError(f"cannot resolve {label} {target}: {exc}") from exc
    if stat.S_ISLNK(target_metadata.st_mode) or not stat.S_ISDIR(target_metadata.st_mode):
        raise ClaimError(f"{label} is not a real directory: {target}")
    return target


def resolve_git_dir(repo_root: Path) -> Path:
    marker = repo_root / ".git"
    try:
        metadata = os.lstat(marker)
    except OSError as exc:
        raise ClaimError(f"repository has no readable .git marker: {marker}: {exc}") from exc
    if stat.S_ISLNK(metadata.st_mode):
        raise ClaimError(f"refusing symlink .git marker: {marker}")
    if stat.S_ISDIR(metadata.st_mode):
        return marker
    if not stat.S_ISREG(metadata.st_mode):
        raise ClaimError(f".git marker is neither a directory nor a worktree file: {marker}")
    text = _read_git_path_file(marker, "worktree gitdir marker")
    if not text.endswith("\n") or text.count("\n") != 1 or not text.startswith("gitdir: "):
        raise ClaimError(f"malformed worktree gitdir marker: {marker}")
    return _resolve_git_directory(
        repo_root,
        text[len("gitdir: ") : -1],
        marker,
        "worktree git directory",
    )


def resolve_git_common_dir(repo_root: Path) -> Path:
    git_dir = resolve_git_dir(repo_root)
    marker = git_dir / "commondir"
    try:
        metadata = os.lstat(marker)
    except FileNotFoundError:
        return git_dir
    except OSError as exc:
        raise ClaimError(f"cannot inspect Git commondir marker {marker}: {exc}") from exc
    if stat.S_ISLNK(metadata.st_mode):
        raise ClaimError(f"refusing symlink Git commondir marker: {marker}")
    if not stat.S_ISREG(metadata.st_mode):
        raise ClaimError(f"Git commondir marker is not a regular file: {marker}")
    text = _read_git_path_file(marker, "Git commondir marker")
    if not text.endswith("\n") or text.count("\n") != 1:
        raise ClaimError(f"malformed Git commondir marker: {marker}")
    return _resolve_git_directory(
        git_dir,
        text[:-1],
        marker,
        "Git common directory",
    )


def _lock_descriptor(descriptor: int) -> None:
    if os.name == "nt":
        import msvcrt

        os.lseek(descriptor, 0, os.SEEK_SET)
        try:
            msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
        except OSError as exc:
            raise ClaimError(
                "another local agent claim is already in progress for this repository", 5
            ) from exc
    else:
        import fcntl

        try:
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as exc:
            raise ClaimError(
                "another local agent claim is already in progress for this repository", 5
            ) from exc


def _unlock_descriptor(descriptor: int) -> None:
    if os.name == "nt":
        import msvcrt

        os.lseek(descriptor, 0, os.SEEK_SET)
        msvcrt.locking(descriptor, msvcrt.LK_UNLCK, 1)
    else:
        import fcntl

        fcntl.flock(descriptor, fcntl.LOCK_UN)


@contextlib.contextmanager
def claim_lock(repo_root: Path) -> Iterator[Path]:
    git_common_dir = resolve_git_common_dir(repo_root)
    lock_path = git_common_dir / LOCK_FILE_NAME
    flags = os.O_RDWR | os.O_CREAT
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(lock_path, flags, 0o600)
    except OSError as exc:
        raise ClaimError(f"cannot open claim lock {lock_path}: {exc}", 5) from exc
    locked = False
    try:
        metadata = os.fstat(descriptor)
        if not stat.S_ISREG(metadata.st_mode):
            raise ClaimError(f"claim lock is not a regular file: {lock_path}", 5)
        if metadata.st_size == 0:
            os.write(descriptor, b"0")
            os.fsync(descriptor)
        _lock_descriptor(descriptor)
        locked = True
        yield lock_path
    finally:
        if locked:
            try:
                _unlock_descriptor(descriptor)
            except OSError:
                pass
        os.close(descriptor)


def _decode_process_output(value: bytes) -> str:
    return value.decode("utf-8", errors="replace").strip()


def _drain_command_stream(
    stream: BinaryIO,
    result: _DrainResult,
    budget: _OutputBudget,
) -> None:
    try:
        while True:
            chunk = stream.read(COMMAND_READ_CHUNK_BYTES)
            if not chunk:
                break
            budget.consume(len(chunk))
            remaining = MAX_COMMAND_OUTPUT_BYTES + 1 - len(result.payload)
            if remaining > 0:
                result.payload.extend(chunk[:remaining])
            if len(result.payload) > MAX_COMMAND_OUTPUT_BYTES:
                result.overflow = True
    except OSError as exc:
        result.error = exc
    finally:
        try:
            stream.close()
        except OSError as exc:
            if result.error is None:
                result.error = exc


def _close_process_streams(process: subprocess.Popen[bytes]) -> None:
    for stream in (process.stdout, process.stderr):
        if stream is not None:
            try:
                stream.close()
            except OSError:
                pass


def _windows_taskkill(process_id: int) -> None:
    system_root = os.environ.get("SystemRoot", r"C:\Windows")
    taskkill = Path(system_root) / "System32" / "taskkill.exe"
    if not taskkill.is_file():
        return
    creationflags = getattr(subprocess, "CREATE_NO_WINDOW", 0)
    try:
        subprocess.run(
            (str(taskkill), "/PID", str(process_id), "/T", "/F"),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            check=False,
            timeout=COMMAND_TERMINATION_WAIT_SECONDS,
            creationflags=creationflags,
        )
    except (OSError, subprocess.TimeoutExpired):
        pass


def _terminate_process_tree(process: subprocess.Popen[bytes]) -> None:
    if os.name == "nt":
        if process.poll() is None:
            _windows_taskkill(process.pid)
        if process.poll() is None:
            try:
                process.kill()
            except OSError:
                pass
    else:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except OSError:
            if process.poll() is None:
                try:
                    process.kill()
                except OSError:
                    pass
    try:
        process.wait(timeout=COMMAND_TERMINATION_WAIT_SECONDS)
    except (OSError, subprocess.TimeoutExpired):
        if process.poll() is None:
            try:
                process.kill()
            except OSError:
                pass
        try:
            process.wait(timeout=COMMAND_TERMINATION_WAIT_SECONDS)
        except (OSError, subprocess.TimeoutExpired):
            pass


def _join_command_readers(
    process: subprocess.Popen[bytes],
    threads: list[threading.Thread],
) -> tuple[bool, bool]:
    for thread in threads:
        thread.join(COMMAND_READER_JOIN_SECONDS)
    if not any(thread.is_alive() for thread in threads):
        return True, True
    _terminate_process_tree(process)
    _close_process_streams(process)
    for thread in threads:
        thread.join(COMMAND_READER_JOIN_SECONDS)
    return False, not any(thread.is_alive() for thread in threads)


def run_command(
    argv: tuple[str, ...],
    cwd: Path,
    *,
    timeout_seconds: float = DEFAULT_COMMAND_TIMEOUT_SECONDS,
) -> CommandResult:
    timeout_seconds = _command_timeout(timeout_seconds)
    env = os.environ.copy()
    env.setdefault("NO_COLOR", "1")
    env.setdefault("RUST_LOG", "error")
    popen_options: dict[str, object] = {}
    if os.name == "nt":
        popen_options["creationflags"] = getattr(
            subprocess, "CREATE_NEW_PROCESS_GROUP", 0
        )
    else:
        popen_options["start_new_session"] = True
    try:
        process = subprocess.Popen(
            argv,
            cwd=cwd,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,
            **popen_options,
        )
    except OSError as exc:
        raise ClaimError(f"could not execute {argv[0]!r}: {exc}", 5) from exc
    if process.stdout is None or process.stderr is None:
        _terminate_process_tree(process)
        _close_process_streams(process)
        raise ClaimError(f"could not capture output from {argv[0]!r}", 5)

    stdout = _DrainResult(bytearray())
    stderr = _DrainResult(bytearray())
    budget = _OutputBudget(MAX_COMMAND_TOTAL_OUTPUT_BYTES)
    threads = [
        threading.Thread(
            target=_drain_command_stream,
            args=(process.stdout, stdout, budget),
            name="fmn-agent-claim-stdout",
            daemon=True,
        ),
        threading.Thread(
            target=_drain_command_stream,
            args=(process.stderr, stderr, budget),
            name="fmn-agent-claim-stderr",
            daemon=True,
        ),
    ]
    started: list[threading.Thread] = []
    try:
        for thread in threads:
            thread.start()
            started.append(thread)
    except RuntimeError as exc:
        _terminate_process_tree(process)
        _close_process_streams(process)
        for thread in started:
            thread.join(COMMAND_READER_JOIN_SECONDS)
        raise ClaimError(
            f"could not start bounded output readers for {argv[0]!r}: {exc}", 5
        ) from exc

    deadline = time.monotonic() + timeout_seconds
    returncode: int | None = None
    while returncode is None:
        if budget.exceeded.is_set():
            _terminate_process_tree(process)
            _, readers_stopped = _join_command_readers(process, started)
            suffix = "" if readers_stopped else "; output readers did not stop"
            raise ClaimError(
                f"{argv[0]!r} produced more than "
                f"{MAX_COMMAND_TOTAL_OUTPUT_BYTES} total output bytes "
                f"across stdout and stderr{suffix}",
                5,
            )
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            _terminate_process_tree(process)
            _, readers_stopped = _join_command_readers(process, started)
            suffix = "" if readers_stopped else "; output readers did not stop"
            raise ClaimError(
                f"{argv[0]!r} timed out after {timeout_seconds:g} seconds{suffix}",
                5,
            )
        try:
            returncode = process.wait(
                timeout=min(remaining, COMMAND_WAIT_POLL_SECONDS)
            )
        except subprocess.TimeoutExpired:
            continue
        except OSError as exc:
            _terminate_process_tree(process)
            _join_command_readers(process, started)
            raise ClaimError(f"could not wait for {argv[0]!r}: {exc}", 5) from exc

    readers_closed_naturally, readers_stopped = _join_command_readers(process, started)
    if not readers_closed_naturally:
        suffix = "" if readers_stopped else "; output readers did not stop"
        raise ClaimError(
            f"{argv[0]!r} exited but its output pipes remained open; "
            f"forced cleanup was required{suffix}",
            5,
        )
    if budget.exceeded.is_set():
        raise ClaimError(
            f"{argv[0]!r} produced more than "
            f"{MAX_COMMAND_TOTAL_OUTPUT_BYTES} total output bytes "
            f"across stdout and stderr",
            5,
        )

    for stream_name, result in (("stdout", stdout), ("stderr", stderr)):
        if result.error is not None:
            raise ClaimError(
                f"could not read {argv[0]!r} {stream_name}: {result.error}", 5
            )
        if result.overflow:
            raise ClaimError(
                f"{argv[0]!r} {stream_name} exceeded the "
                f"{MAX_COMMAND_OUTPUT_BYTES}-byte command-output limit",
                5,
            )
    return CommandResult(returncode, bytes(stdout.payload), bytes(stderr.payload))


def _run_checked(runner: Runner, argv: tuple[str, ...], cwd: Path, label: str) -> None:
    result = runner(argv, cwd)
    if not isinstance(result, CommandResult):
        raise ClaimError(f"{label} runner returned an invalid result", 5)
    for stream_name, payload in (("stdout", result.stdout), ("stderr", result.stderr)):
        if not isinstance(payload, bytes):
            raise ClaimError(f"{label} runner returned non-byte {stream_name}", 5)
        if len(payload) > MAX_COMMAND_OUTPUT_BYTES:
            raise ClaimError(
                f"{label} {stream_name} exceeded the "
                f"{MAX_COMMAND_OUTPUT_BYTES}-byte command-output limit",
                5,
            )
    produced = len(result.stdout) + len(result.stderr)
    if produced > MAX_COMMAND_TOTAL_OUTPUT_BYTES:
        raise ClaimError(
            f"{label} produced more than "
            f"{MAX_COMMAND_TOTAL_OUTPUT_BYTES} total output bytes "
            "across stdout and stderr",
            5,
        )
    if result.returncode == 0:
        return
    detail = _decode_process_output(result.stderr) or _decode_process_output(result.stdout)
    suffix = f": {detail[:4096]}" if detail else ""
    raise ClaimError(f"{label} failed with exit {result.returncode}{suffix}", 5)


def _commands(
    br_program: str,
    issue_id: str,
    assignee: str,
    transition_comment: str | None,
) -> tuple[tuple[str, ...], tuple[str, ...]]:
    update = [
        br_program,
        "update",
        issue_id,
        "--status",
        agent_brief.ACTIVE_STATUS,
        "--assignee",
        assignee,
    ]
    if transition_comment is not None:
        update.extend(("--transition-comment", transition_comment))
    return tuple(update), (br_program, "sync", "--flush-only")


def _validate_guard(
    guard: dict,
    expect_token: str,
    requested_issue: str | None,
) -> str:
    if not guard["integrity"]["ok"]:
        raise ClaimError("Beads task-graph integrity failed", 1)
    if not guard["activation"]["within_cap"]:
        raise ClaimError("active workstream cap breached", 1)
    try:
        expected_digest, expected_issue = agent_claim_guard.parse_token(expect_token)
    except agent_claim_guard.GuardError as exc:
        raise ClaimError(str(exc), 2) from exc
    if (
        expected_digest != guard["claim_sha256"]
        or expected_issue != guard["recommendation_id"]
    ):
        raise ClaimError(
            "claim token is stale; refresh the plan and re-check reservations before claiming",
            4,
        )
    if expected_issue is None:
        raise ClaimError("no claimable recommendation exists", 3)
    if requested_issue is not None and requested_issue != expected_issue:
        raise ClaimError(
            f"--issue {requested_issue!r} disagrees with guarded recommendation {expected_issue!r}"
        )
    return expected_issue


def _verify_claim_only_delta(
    before: dict[str, agent_brief.Issue],
    after: dict[str, agent_brief.Issue],
    issue_id: str,
    assignee: str,
    transition_comment: str | None,
) -> dict[str, object]:
    before_ids = set(before)
    after_ids = set(after)
    if before_ids != after_ids:
        added = sorted(after_ids - before_ids)
        removed = sorted(before_ids - after_ids)
        raise ClaimError(
            "claim postcondition observed issue-membership drift: "
            f"added={added!r} removed={removed!r}",
            5,
        )

    for other_id in sorted(before_ids - {issue_id}):
        if before[other_id] != after[other_id]:
            raise ClaimError(
                f"claim postcondition observed unrelated graph drift in {other_id}",
                5,
            )

    original = before[issue_id]
    claimed = after[issue_id]
    if claimed.status != agent_brief.ACTIVE_STATUS or claimed.assignee != assignee:
        raise ClaimError(
            f"claim postcondition failed for {issue_id}: expected status "
            f"{agent_brief.ACTIVE_STATUS!r} and assignee {assignee!r}, got "
            f"status {claimed.status!r} and assignee {claimed.assignee!r}",
            5,
        )
    if claimed.updated_at < original.updated_at:
        raise ClaimError(
            f"claim postcondition regressed updated_at for {issue_id}: "
            f"{claimed.updated_at.isoformat()} < {original.updated_at.isoformat()}",
            5,
        )
    if (
        claimed.id != original.id
        or claimed.title != original.title
        or claimed.priority != original.priority
        or claimed.issue_type != original.issue_type
        or claimed.dependencies != original.dependencies
    ):
        raise ClaimError(
            f"claim postcondition changed non-claim fields on {issue_id}",
            5,
        )

    comment_appended = transition_comment is not None
    if transition_comment is None:
        comments_match = claimed.comments == original.comments
    else:
        comments_match = (
            len(claimed.comments) == len(original.comments) + 1
            and claimed.comments[:-1] == original.comments
            and claimed.comments[-1].get("text") == transition_comment
        )
    if not comments_match:
        expected = (
            "no comment change"
            if transition_comment is None
            else "one exact appended transition comment"
        )
        raise ClaimError(
            f"claim postcondition expected {expected} on {issue_id}",
            5,
        )

    return {
        "changed_issue_ids": [issue_id],
        "allowed_fields": [
            "assignee",
            "status",
            "updated_at",
            *([] if transition_comment is None else ["comments"]),
        ],
        "status_before": original.status,
        "status_after": claimed.status,
        "assignee_before": original.assignee,
        "assignee_after": claimed.assignee,
        "updated_at_before": original.updated_at.isoformat().replace("+00:00", "Z"),
        "updated_at_after": claimed.updated_at.isoformat().replace("+00:00", "Z"),
        "transition_comment_appended": comment_appended,
    }


def execute_claim(
    repo_root: Path,
    expect_token: str,
    assignee: str,
    *,
    requested_issue: str | None = None,
    transition_comment: str | None = None,
    as_of: str | None = None,
    stale_days: int = 2,
    activation_cap: int = 4,
    limit: int = 20,
    br_program: str = "br",
    command_timeout_seconds: float = DEFAULT_COMMAND_TIMEOUT_SECONDS,
    dry_run: bool = False,
    runner: Runner | None = None,
    lock_factory: LockFactory = claim_lock,
) -> dict:
    expect_token = _bounded_text(
        "--expect-token", expect_token, MAX_TOKEN_BYTES, required=True
    ) or ""
    assignee = _bounded_text("--assignee", assignee, MAX_ASSIGNEE_BYTES, required=True) or ""
    requested_issue = _bounded_text(
        "--issue", requested_issue, MAX_ISSUE_ID_BYTES, required=False
    )
    transition_comment = _bounded_text(
        "--transition-comment",
        transition_comment,
        MAX_TRANSITION_COMMENT_BYTES,
        required=False,
    )
    br_program = _bounded_text("--br", br_program, 4096, required=True) or "br"
    command_timeout_seconds = _command_timeout(command_timeout_seconds)
    if stale_days < 0:
        raise ClaimError("--stale-days must be nonnegative")
    if activation_cap < 1:
        raise ClaimError("--activation-cap must be positive")
    if not 1 <= limit <= 1000:
        raise ClaimError("--limit must be between 1 and 1000")

    if runner is None:
        def active_runner(argv: tuple[str, ...], cwd: Path) -> CommandResult:
            return run_command(
                argv,
                cwd,
                timeout_seconds=command_timeout_seconds,
            )
    else:
        active_runner = runner

    repo_root = _repo_root(repo_root)
    ledger = repo_root / ".beads" / "issues.jsonl"
    with lock_factory(repo_root):
        try:
            issues = agent_brief.load_issues(ledger)
            guard = agent_claim_guard.build_guard(
                issues,
                as_of=agent_next.parse_as_of(as_of, issues),
                stale_days=stale_days,
                activation_cap=activation_cap,
                limit=limit,
            )
        except (agent_brief.BriefError, agent_claim_guard.GuardError) as exc:
            raise ClaimError(str(exc)) from exc

        issue_id = _validate_guard(guard, expect_token, requested_issue)
        issue = issues.get(issue_id)
        if issue is None:
            raise ClaimError(f"guarded issue disappeared from the parsed graph: {issue_id}", 4)
        if issue.status != "open" or issue.assignee is not None:
            raise ClaimError(
                f"guarded issue {issue_id} is no longer an unassigned open leaf", 4
            )
        update_command, sync_command = _commands(
            br_program, issue_id, assignee, transition_comment
        )
        receipt = {
            "schema": SCHEMA,
            "version": SCHEMA_VERSION,
            "mode": "dry-run" if dry_run else "claim",
            "issue_id": issue_id,
            "assignee": assignee,
            "guard_token": expect_token,
            "before_claim_sha256": guard["claim_sha256"],
            "before_graph_sha256": guard["graph_sha256"],
            "policy": guard["policy"],
            "schemas": guard["schemas"],
            "executor_policy": {
                "command_timeout_seconds": command_timeout_seconds,
                "command_output_bytes_per_stream": MAX_COMMAND_OUTPUT_BYTES,
                "command_total_output_bytes": MAX_COMMAND_TOTAL_OUTPUT_BYTES,
            },
            "recommendation": guard["recommendation"],
            "commands": [list(update_command), list(sync_command)],
        }
        if dry_run:
            receipt["claimed"] = False
            return receipt

        _run_checked(active_runner, update_command, repo_root, "br update")
        _run_checked(active_runner, sync_command, repo_root, "br sync --flush-only")
        try:
            after = agent_brief.load_issues(ledger)
            after_graph_sha256 = agent_claim_guard.graph_digest(after)
        except (agent_brief.BriefError, agent_claim_guard.GuardError) as exc:
            raise ClaimError(f"post-claim ledger verification failed: {exc}", 5) from exc
        claimed = after.get(issue_id)
        if claimed is None:
            raise ClaimError(f"claimed issue disappeared after br sync: {issue_id}", 5)
        claim_delta = _verify_claim_only_delta(
            issues,
            after,
            issue_id,
            assignee,
            transition_comment,
        )
        receipt.update(
            {
                "claimed": True,
                "status": claimed.status,
                "after_graph_sha256": after_graph_sha256,
                "claim_delta": claim_delta,
            }
        )
        return receipt


def render_json(receipt: dict) -> str:
    try:
        text = json.dumps(
            receipt,
            ensure_ascii=False,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        ) + "\n"
    except (TypeError, ValueError) as exc:
        raise ClaimError(f"claim receipt cannot be canonically encoded: {exc}", 5) from exc
    size = len(text.encode("utf-8"))
    if size > MAX_OUTPUT_BYTES:
        raise ClaimError(
            f"claim receipt exceeds the {MAX_OUTPUT_BYTES}-byte limit ({size} bytes)", 5
        )
    return text


def make_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, default=Path("."))
    parser.add_argument("--expect-token", required=True)
    parser.add_argument("--issue")
    parser.add_argument("--assignee", default=os.environ.get("FMN_AGENT_ID"))
    parser.add_argument("--transition-comment")
    parser.add_argument("--as-of")
    parser.add_argument("--stale-days", type=int, default=2)
    parser.add_argument("--activation-cap", type=int, default=4)
    parser.add_argument("--limit", type=int, default=20)
    parser.add_argument("--br", default="br")
    parser.add_argument(
        "--command-timeout-seconds",
        type=float,
        default=DEFAULT_COMMAND_TIMEOUT_SECONDS,
        help=(
            "maximum wall-clock time for each br command; "
            f"default {DEFAULT_COMMAND_TIMEOUT_SECONDS:g}, "
            f"maximum {MAX_COMMAND_TIMEOUT_SECONDS:g}"
        ),
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="revalidate and emit the exact intended argv without invoking br",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    args = make_parser().parse_args(argv)
    try:
        receipt = execute_claim(
            args.repo,
            args.expect_token,
            args.assignee,
            requested_issue=args.issue,
            transition_comment=args.transition_comment,
            as_of=args.as_of,
            stale_days=args.stale_days,
            activation_cap=args.activation_cap,
            limit=args.limit,
            br_program=args.br,
            command_timeout_seconds=args.command_timeout_seconds,
            dry_run=args.dry_run,
        )
        output = render_json(receipt)
    except ClaimError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return exc.exit_code
    sys.stdout.write(output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
