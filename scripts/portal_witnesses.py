#!/usr/bin/env python3
"""Behavioral witnesses for the Parity Ledger (fm-5wq.12).

docs/api/ledger.tsv marks about 2,000 manimlib symbols `same` or `improved`, and most of those
rows cite one file (bridge.py) as their test. This script measures what actually exercises each
symbol. It runs the installed-wheel gate's suites (the ones scripts/check_portal_runtime.sh runs),
each in its own interpreter under sys.setprofile, against the installed wheel, and records which
ledger symbols every passing test invokes.

A symbol resolves the way a call does, through its class's MRO (inspect.getattr_static) to the
outermost implementation: a code object for Python, a builtin name for native code. A call counts
for a row when it runs that implementation on an instance of the row's class. A class row counts
when an instance of exactly that class is initialized. A test that fails, or a script suite that
raises, witnesses nothing. A call made outside any `test*` function belongs to the suite's
top-level statement that made it (`<module>:L<line>`): bridge.py and the other script suites are
sequences of top-level asserts and calls.

Witness kinds per symbol, each with its own denominator:
- import: the symbol resolves in the installed portal, and not to a schema placeholder;
- semantic: a passing test invoked it;
- rendered: a passing test that also captured a frame (one of RENDER_SYMBOLS) invoked it;
- corpus: the pinned videos tree names it (`.name` for a member, `Name(` otherwise).

Mutation controls: `check --mutants N` replaces the implementation of N deterministically sampled
witnessed members (Python implementations; a native one cannot be patched) with a trivial body
(returns None) and with a plausible-but-wrong one (a numeric array result shifted by 0.01). It
then reruns the suites that witness each member. A mutant is killed when a witness test fails that
passed without it. A surviving mutant marks witnesses that invoke a symbol without checking what
it does.

    portal_witnesses.py build [--jobs N]              write docs/api/portal_witnesses.tsv
    portal_witnesses.py check [--jobs N] [--mutants N]
    portal_witnesses.py trace SUITE [--mutate ROW:KIND]   one suite, NDJSON on stdout (internal)

`check` exits 1 when a symbol the committed registry records as witnessed has lost every
semantic witness, when a suite executes zero test cases, or when a sampled mutant survives every
witness test. It exits 2 on malformed input.
"""
from __future__ import annotations

import argparse
import collections
import concurrent.futures
import hashlib
import importlib
import inspect
import json
import os
import pathlib
import re
import runpy
import subprocess
import sys
import tomllib
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[1]
LEDGER = ROOT / "docs" / "api" / "ledger.tsv"
REGISTRY = ROOT / "docs" / "api" / "portal_witnesses.tsv"
GATE = ROOT / "scripts" / "check_portal_runtime.sh"
TESTS = ROOT / "crates" / "fmn-python" / "tests"
VIDEOS = pathlib.Path(os.environ.get("FMN_VIDEOS_REF", ROOT / "scripts" / "videos_ref"))
SCHEMA = "fmn.portal-witnesses"
VERSION = 1
WITNESSED_STATUSES = ("same", "improved")
COLUMNS = ("module", "symbol", "kind", "status", "import", "semantic", "rendered", "corpus",
           "example")
# A test that calls one of these produced a frame, so what it invoked is rendered state.
RENDER_SYMBOLS = frozenset({"Scene.get_image", "Camera.get_image", "Camera.get_raw_fbo_data",
                            "Camera.capture", "Scene.update_frame", "Scene.emit_frame"})
SUITE_TIMEOUT_S = 900


def ledger_rows(text: str) -> list[dict]:
    """The manimlib rows of the ledger, in ledger order, with their status."""
    columns = None
    rows = []
    for line in text.splitlines():
        if line.startswith("# module\t"):
            columns = line[2:].split("\t")
            continue
        if not line or line.startswith("#"):
            continue
        if columns is None:
            raise ValueError("ledger.tsv has no '# module' header")
        row = dict(zip(columns, line.split("\t")))
        if row["module"].startswith("manimlib"):
            rows.append(row)
    return rows


def gate_suites(script: str) -> list[str]:
    """The suites check_portal_runtime.sh runs, in its order: the runpy loop, then the direct runs."""
    loop = re.search(r"^for suite in (.+?); do$", script, re.M)
    if loop is None:
        raise ValueError("check_portal_runtime.sh has no suite loop")
    suites = loop.group(1).split()
    for name in re.findall(r"^python3 crates/fmn-python/tests/(\w+)\.py", script, re.M):
        if name not in suites:
            suites.append(name)
    return suites


class Probes:
    """Where each ledger row's implementation runs, for the profiler."""

    def __init__(self, rows: list[dict]):
        self.code = collections.defaultdict(list)     # code object -> [(row, owner class)]
        self.native = collections.defaultdict(list)   # builtin name -> [(row, owner class)]
        self.classes = collections.defaultdict(list)  # class -> [row]
        self.status = {}                              # row -> ok | placeholder | error: ...
        self.implementation = {}                      # row -> (owner, attribute name, value)
        for index, row in enumerate(rows):
            if row["status"] not in WITNESSED_STATUSES:
                continue
            try:
                owner, name, value = resolve(row["module"], row["symbol"])
            except Exception as error:  # noqa: BLE001 — any import or lookup failure is a finding
                self.status[index] = f"error: {type(error).__name__}"
                continue
            self.implementation[index] = (owner, name, value)
            function = callable_of(value)
            if getattr(function, "_fmn_schema_placeholder", False) is True:
                self.status[index] = "placeholder"
                continue
            self.status[index] = "ok"
            cls = owner if isinstance(owner, type) else None
            if row["kind"] == "class" and isinstance(value, type):
                self.classes[value].append(index)
            elif getattr(function, "__code__", None) is not None:
                self.code[function.__code__].append((index, cls))
            elif callable(function):
                self.native[getattr(function, "__name__", name)].append((index, cls))


def resolve(module: str, symbol: str):
    """(owner, attribute name, static value) for `module`'s `symbol`, dotted through classes."""
    owner, value = None, importlib.import_module(module)
    name = symbol
    for name in symbol.split("."):
        owner, value = value, inspect.getattr_static(value, name)
    return owner, name, value


def callable_of(value):
    """The function a call or attribute read runs: unwrap property, classmethod, staticmethod."""
    if isinstance(value, property):
        return value.fget
    return getattr(value, "__func__", value)


class Recorder:
    """A sys.setprofile hook: which rows each test identity invokes, and whether it renders."""

    def __init__(self, probes: Probes, suite_file: str, render_rows: set[int]):
        self.probes = probes
        self.suite_file = suite_file
        self.render_rows = render_rows
        self.stack = []                                # active test frames
        self.hits = collections.defaultdict(set)      # identity -> rows
        self.renders = set()                          # identities that captured a frame
        self.module_identity = "<module>"

    def identity(self, frame) -> str:
        if self.stack:
            return self.stack[-1][1]
        while frame is not None:
            if frame.f_code.co_filename == self.suite_file and frame.f_code.co_name == "<module>":
                return f"{self.module_identity}:L{frame.f_lineno}"
            frame = frame.f_back
        return self.module_identity

    def _hit(self, row: int, frame) -> None:
        identity = self.identity(frame)
        self.hits[identity].add(row)
        if row in self.render_rows:
            self.renders.add(identity)

    def __call__(self, frame, event, arg):
        if event == "call":
            code = frame.f_code
            if code.co_filename == self.suite_file and code.co_name.startswith("test"):
                self.stack.append((frame, frame_identity(frame)))
            entries = self.probes.code.get(code)
            if entries:
                instance = frame.f_locals.get("self")
                for row, cls in entries:
                    if cls is None or instance is None or isinstance(instance, cls):
                        self._hit(row, frame)
            if code.co_name == "__init__":
                instance = frame.f_locals.get("self")
                for row in self.probes.classes.get(type(instance), ()):
                    self._hit(row, frame)
        elif event == "return":
            if self.stack and self.stack[-1][0] is frame:
                self.stack.pop()
        elif event == "c_call":
            entries = self.probes.native.get(getattr(arg, "__name__", None))
            if entries:
                instance = getattr(arg, "__self__", None)
                for row, cls in entries:
                    if cls is None or (instance is not None and isinstance(instance, cls)):
                        self._hit(row, frame)


def frame_identity(frame) -> str:
    instance = frame.f_locals.get("self")
    name = frame.f_code.co_name
    if isinstance(instance, unittest.TestCase):
        return f"{type(instance).__name__}.{name}"
    return name


def mutate(probes: Probes, row: int, kind: str) -> None:
    """Replace row's implementation on its owner: `trivial` returns None, `shifted` adds 0.01 to
    a numeric array result."""
    owner, name, value = probes.implementation[row]
    original = callable_of(value)

    def trivial(*args, **kwargs):
        return None

    def shifted(*args, **kwargs):
        import numpy as np
        result = original(*args, **kwargs)
        if isinstance(result, np.ndarray) and result.dtype.kind == "f":
            return result + 0.01
        return result

    replacement = trivial if kind == "trivial" else shifted
    if isinstance(value, property):
        replacement = property(replacement, value.fset, value.fdel)
    elif isinstance(value, staticmethod):
        replacement = staticmethod(replacement)
    elif isinstance(value, classmethod):
        replacement = classmethod(replacement)
    setattr(owner, name, replacement)


def trace_suite(name: str, mutation: str | None) -> list[dict]:
    """Run one suite under the profiler. One record per test identity, then a summary record."""
    rows = ledger_rows(LEDGER.read_text(encoding="utf-8"))
    probes = Probes(rows)
    render_rows = {i for i, r in enumerate(rows) if r["symbol"] in RENDER_SYMBOLS}
    if mutation:
        row, kind = mutation.split(":")
        try:
            mutate(probes, int(row), kind)
        except (TypeError, AttributeError) as error:
            return [{"suite": name, "summary": True, "cases": 0, "failed": [],
                     "error": None, "unmutable": f"{type(error).__name__}: {error}"}]
    suite = TESTS / f"{name}.py"
    recorder = Recorder(probes, str(suite), render_rows)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    passed, failed, error = set(), set(), None
    unittest_mode = "unittest.main" in suite.read_text(encoding="utf-8")
    sys.path.insert(0, str(TESTS))
    sys.setprofile(recorder)
    try:
        if unittest_mode:
            # Run as __main__, the way the gate runs it, so classes pickle and multiprocessing
            # children re-import it; unittest.main runs the module's own cases into _Result.
            result = _Result()

            def run_cases(*args, **kwargs):
                namespace = sys.modules["__main__"].__dict__
                cases = unittest.TestSuite()
                for value in list(namespace.values()):
                    if (isinstance(value, type) and issubclass(value, unittest.TestCase)
                            and value.__module__ == "__main__"):
                        cases.addTests(unittest.TestLoader().loadTestsFromTestCase(value))
                cases.run(result)

            unittest.main = run_cases
            sys.argv = [str(suite)]
            runpy.run_path(str(suite), run_name="__main__")
            passed, failed = result.passed, result.failed
        else:
            runpy.run_path(str(suite), init_globals={"_expected_package_version": version})
            passed = set(recorder.hits)
    except BaseException as caught:  # noqa: BLE001 — a raising script suite witnesses nothing
        error = f"{type(caught).__name__}: {str(caught)[:200]}"
    finally:
        sys.setprofile(None)
    records = []
    for identity, hit in sorted(recorder.hits.items()):
        ok = error is None and identity in passed and identity not in failed
        if identity.startswith(recorder.module_identity) and unittest_mode:
            ok = False  # import-time work in a unittest suite is not a test
        records.append({"suite": name, "test": identity, "passed": ok,
                        "renders": identity in recorder.renders, "rows": sorted(hit)})
    cases = len(passed) + len(failed)
    records.append({"suite": name, "summary": True, "cases": cases, "failed": sorted(failed),
                    "error": error})
    return records


class _Result(unittest.TestResult):
    """Pass/fail per test identity (Class.method), as frame_identity spells it."""

    def __init__(self):
        super().__init__()
        self.passed, self.failed = set(), set()

    @staticmethod
    def _identity(test) -> str:
        return f"{type(test).__name__}.{test._testMethodName}"

    def addSuccess(self, test):
        self.passed.add(self._identity(test))

    def addFailure(self, test, err):
        self.failed.add(self._identity(test))

    def addError(self, test, err):
        self.failed.add(self._identity(test))

    def addSkip(self, test, reason):
        pass


def run_suites(suites: list[str], jobs: int, mutation: str | None = None) -> list[dict]:
    def one(name):
        command = [sys.executable, __file__, "trace", name]
        if mutation:
            command += ["--mutate", mutation]
        try:
            done = subprocess.run(command, capture_output=True, text=True, timeout=SUITE_TIMEOUT_S,
                                  cwd=ROOT, check=False)
        except subprocess.TimeoutExpired:
            return [{"suite": name, "summary": True, "cases": 0, "failed": [],
                     "error": f"timeout after {SUITE_TIMEOUT_S} s"}]
        records = [json.loads(line) for line in done.stdout.splitlines()
                   if line.startswith('{"suite"')]
        if not any(r.get("summary") for r in records):
            records.append({"suite": name, "summary": True, "cases": 0, "failed": [],
                            "error": f"exit {done.returncode}: {done.stderr.strip()[-200:]}"})
        return records
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        return [record for records in pool.map(one, suites) for record in records]


def corpus_counts(rows: list[dict]) -> dict[int, int]:
    """Static mentions in the pinned videos tree: `.name` for members, `Name(` otherwise."""
    if not VIDEOS.is_dir():
        return {}
    text = "\n".join(p.read_text(encoding="utf-8", errors="replace")
                     for p in sorted(VIDEOS.rglob("*.py")))
    calls = collections.Counter(re.findall(r"\b([A-Za-z_]\w*)\(", text))
    members = collections.Counter(re.findall(r"\.([A-Za-z_]\w*)\b", text))
    counts = {}
    for index, row in enumerate(rows):
        leaf = row["symbol"].split(".")[-1]
        counts[index] = members[leaf] if "." in row["symbol"] else calls[leaf]
    return counts


def registry(rows: list[dict], probes: Probes, records: list[dict]) -> list[dict]:
    semantic, rendered = collections.defaultdict(set), collections.defaultdict(set)
    for record in records:
        if record.get("summary") or not record["passed"]:
            continue
        identity = f"{record['suite']}::{record['test']}"
        for row in record["rows"]:
            semantic[row].add(identity)
            if record["renders"]:
                rendered[row].add(identity)
    corpus = corpus_counts(rows)
    out = []
    for index, row in enumerate(rows):
        if row["status"] not in WITNESSED_STATUSES:
            continue
        out.append({
            "module": row["module"], "symbol": row["symbol"], "kind": row["kind"],
            "status": row["status"], "import": probes.status.get(index, "error"),
            "semantic": len(semantic[index]), "rendered": len(rendered[index]),
            "corpus": corpus.get(index, "-"),
            "example": min(semantic[index]) if semantic[index] else "-",
        })
    return out


def summary_lines(table: list[dict], records: list[dict]) -> list[str]:
    suites = [r for r in records if r.get("summary")]
    total = len(table)
    counts = {
        "rows": total,
        "import_ok": sum(r["import"] == "ok" for r in table),
        "placeholder": sum(r["import"] == "placeholder" for r in table),
        "semantic": sum(r["semantic"] > 0 for r in table),
        "rendered": sum(r["rendered"] > 0 for r in table),
        "corpus": sum(isinstance(r["corpus"], int) and r["corpus"] > 0 for r in table),
        "suites": len(suites),
        "suites_raising": sum(bool(s["error"]) for s in suites),
        "cases": sum(s["cases"] for s in suites),
    }
    return [f"# {SCHEMA} v{VERSION}: generated by scripts/portal_witnesses.py build; do not edit",
            "# " + json.dumps(counts, sort_keys=True),
            "\t".join(COLUMNS)]


def write_registry(table: list[dict], records: list[dict]) -> str:
    lines = summary_lines(table, records)
    lines += ["\t".join(str(r[c]) for c in COLUMNS) for r in table]
    return "\n".join(lines) + "\n"


def read_registry(text: str) -> dict[tuple[str, str], dict]:
    rows = {}
    for line in text.splitlines():
        if not line or line.startswith("#") or line.startswith("module\t"):
            continue
        row = dict(zip(COLUMNS, line.split("\t")))
        rows[(row["module"], row["symbol"])] = row
    return rows


def sample_mutants(table: list[dict], python_rows: set[tuple[str, str]], count: int) -> list[str]:
    """`count` witnessed members with a Python implementation, picked by a stable hash so every
    run picks the same ones."""
    members = [r for r in table if r["semantic"] > 0 and r["kind"] in ("method", "property")
               and r["import"] == "ok" and (r["module"], r["symbol"]) in python_rows]
    members.sort(key=lambda r: hashlib.sha256(f"{r['module']}.{r['symbol']}".encode()).hexdigest())
    return [f"{r['module']}.{r['symbol']}" for r in members[:count]]


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    trace = sub.add_parser("trace")
    trace.add_argument("suite")
    trace.add_argument("--mutate")
    for name in ("build", "check"):
        command = sub.add_parser(name)
        command.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 4))
        command.add_argument("--suites", nargs="*", help="limit to these suites (diagnosis only)")
        if name == "check":
            command.add_argument("--mutants", type=int, default=0)
    args = parser.parse_args(argv)

    if args.command == "trace":
        for record in trace_suite(args.suite, args.mutate):
            print(json.dumps(record))
        return 0

    rows = ledger_rows(LEDGER.read_text(encoding="utf-8"))
    probes = Probes(rows)
    suites = args.suites or gate_suites(GATE.read_text(encoding="utf-8"))
    records = run_suites(suites, args.jobs)
    table = registry(rows, probes, records)
    for line in summary_lines(table, records)[:2]:
        print(line)
    for record in records:
        if record.get("summary") and (record["error"] or record["failed"]):
            print(json.dumps({"schema": SCHEMA, "suite": record["suite"], "cases": record["cases"],
                              "failed": record["failed"][:5], "error": record["error"]}))
    if args.command == "build":
        REGISTRY.write_text(write_registry(table, records), encoding="utf-8")
        return 0

    problems = []
    for record in records:
        if record.get("summary") and record["cases"] == 0 and not record["error"]:
            problems.append(f"{record['suite']}: zero executed test cases")
    committed = read_registry(REGISTRY.read_text(encoding="utf-8"))
    now = {(r["module"], r["symbol"]): r for r in table}
    for key, row in committed.items():
        if int(row["semantic"]) > 0 and now.get(key, {}).get("semantic", 0) == 0:
            problems.append(f"{key[0]}.{key[1]}: lost every semantic witness")
    if args.mutants:
        index_of = {f"{r['module']}.{r['symbol']}": i for i, r in enumerate(rows)}
        python_rows = {(rows[i]["module"], rows[i]["symbol"])
                       for i, (_, _, value) in probes.implementation.items()
                       if getattr(callable_of(value), "__code__", None) is not None}
        witnesses = collections.defaultdict(set)
        for record in records:
            if not record.get("summary") and record["passed"]:
                for row in record["rows"]:
                    witnesses[row].add(record["suite"])
        baseline = {r["suite"]: r for r in records if r.get("summary")}
        killed = tried = 0
        for name in sample_mutants(table, python_rows, args.mutants):
            row = index_of[name]
            for kind in ("trivial", "shifted"):
                mutant = [r for r in run_suites(sorted(witnesses[row]), args.jobs, f"{row}:{kind}")
                          if r.get("summary")]
                if any(r.get("unmutable") for r in mutant):
                    print(json.dumps({"schema": SCHEMA, "mutant": name, "kind": kind,
                                      "unmutable": mutant[0]["unmutable"]}))
                    continue
                died = any(set(r["failed"]) - set(baseline[r["suite"]]["failed"])
                           or (r["error"] and not baseline[r["suite"]]["error"]) for r in mutant)
                tried += 1
                killed += died
                print(json.dumps({"schema": SCHEMA, "mutant": name, "kind": kind,
                                  "suites": len(witnesses[row]), "killed": bool(died)}))
                if not died and kind == "trivial":
                    problems.append(f"{name}: a trivial body survives its witnesses")
        print(json.dumps({"schema": SCHEMA, "mutants": tried, "killed": killed}))
    for problem in problems:
        print(problem, file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
