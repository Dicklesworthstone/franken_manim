#!/usr/bin/env python3
"""Behavioral witnesses for the Parity Ledger (fm-5wq.12).

docs/api/ledger.tsv marks about 2,000 manimlib symbols `same` or `improved`, and most of those
rows cite one file (bridge.py) as their test. This script measures what actually exercises each
symbol. It runs the installed-wheel gate's suites (the ones scripts/check_portal_runtime.sh runs),
each in its own interpreter under sys.monitoring, against the installed wheel, and records which
ledger symbols every passing test invokes.

Each suite runs the way the gate runs it: the loop through runpy, direct scripts as
`__main__`, and discovery suites imported by their module name. A member row counts when a member of that
name runs on an instance of the row's class, whichever override or installer wrapper runs: that
is what calling the symbol means. The receiver is the first argument of method-like code (code
defined inside a class or another function), or the bound instance of a native method. A class row counts when construction of exactly that class returns normally.
A function row counts when its code returns normally, and so does a staticmethod member row,
which has no receiver. A test that fails, or a script suite that
raises, witnesses nothing. A call made outside any `test*` function belongs to the suite's
top-level statement that made it (`<module>:L<line>`): bridge.py and the other script suites are
sequences of top-level asserts and calls. Constants, attributes and leaked imports are read, not
called, so only the import kind applies to them.

Witness kinds per symbol, each with its own denominator:
- import: the symbol resolves in the installed portal, and not to a schema placeholder;
- semantic: a passing test invoked it and the call returned normally;
- refusal_only: a passing test invoked it, but every such call raised. An abstract class
  (inspect.isabstract) is exempt from the check below: refusing to construct is what the
  Reference's abstract class does too, so its refusal is the semantics, not an overclaim;
- rendered: a passing test that also captured a frame (one of RENDER_SYMBOLS) invoked it;
- corpus: the pinned videos tree names it (`.name` for a member, `Name(` otherwise).

The harness imports the installed portal before each suite, to resolve the ledger. Fake-based
protocol suites can depend on the real portal never having initialized in their process,
which `unittest discover` happens to guarantee because its native tests skip. Suites can
also depend on discover's import order. So a suite that fails traced is checked the way the
gate runs it, untraced and without the portal import. A script or loop suite is rerun alone.
The discovery suites are checked against one plain `unittest discover` of the whole
directory, per module. Only failures that reproduce there are the suite's. The rest are
reported as harness-only, and those tests witness nothing (fm-6l60).

Mutation controls: `check --mutants N` replaces the implementation of N deterministically sampled
witnessed members (Python implementations; a native one cannot be patched) with a trivial body
(returns None), and of N members whose name says they return positions with a plausible-but-wrong
one (a float array result shifted by 0.1 scene units). It then reruns, untraced, up to
MUTANT_SUITES of the suites that witness each member. A mutant is killed when a witness test fails
that passed without it; a surviving mutant marks witnesses that invoke a symbol without checking
what it does. A mutant that changed no result in any witness run (a shifted mutant of a member
that returned no float array) is reported as inapplicable, not as a survivor.

    portal_witnesses.py build [--jobs N]              write docs/api/portal_witnesses.tsv
    portal_witnesses.py check [--jobs N] [--mutants N]
    portal_witnesses.py trace SUITE [--mode runpy|main|discover] [--mutate ROW:KIND] [--plain]   internal

`check` exits 1 when a suite fails, a symbol the committed registry records as witnessed has lost every
semantic witness, when a same/improved row has only refusal witnesses, when a ledger symbol resolves to a placeholder or does not resolve, when a suite
that defines unittest cases executes none of them, or when an applied sampled mutant (trivial or
shifted) survives every witness test it reran.
"""
from __future__ import annotations

import argparse
import collections
import concurrent.futures
import contextlib
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
import types
import unittest
import weakref

ROOT = pathlib.Path(__file__).resolve().parents[1]
LEDGER = ROOT / "docs" / "api" / "ledger.tsv"
REGISTRY = ROOT / "docs" / "api" / "portal_witnesses.tsv"
GATE = ROOT / "scripts" / "check_portal_runtime.sh"
TESTS = ROOT / "crates" / "fmn-python" / "tests"
VIDEOS = pathlib.Path(os.environ.get("FMN_VIDEOS_REF", ROOT / "scripts" / "videos_ref"))
SCHEMA = "fmn.portal-witnesses"
VERSION = 2
WITNESSED_STATUSES = ("same", "improved")
COLUMNS = ("module", "symbol", "kind", "status", "import", "semantic", "refusal_only",
           "rendered", "corpus", "example")
# A test that calls one of these produced a frame, so what it invoked is rendered state.
RENDER_SYMBOLS = frozenset({"Scene.get_image", "Camera.get_image", "Camera.get_raw_fbo_data",
                            "Camera.capture", "Scene.update_frame", "Scene.emit_frame"})
SUITE_TIMEOUT_S = 900
MUTANT_SUITES = 8  # witness suites rerun per mutant


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


def gate_suites(script: str, tests: pathlib.Path = TESTS) -> list[tuple[str, str]]:
    """Gate suites with their execution mode: `runpy` for the loop, `main` for direct
    scripts, and `discover` for normally imported unittest discovery modules."""
    loop = re.search(r"^for suite in (.+?); do$", script, re.M)
    if loop is None:
        raise ValueError("check_portal_runtime.sh has no suite loop")
    suites = [(name, "runpy") for name in loop.group(1).split()]
    # A direct run may carry environment assignments (`FMN_REQUIRE_FFMPEG=1 python3 ...`).
    direct = [(name, "main") for name in re.findall(
        r"^(?:[A-Z_][A-Z0-9_]*=\S* )*python3 crates/fmn-python/tests/(\w+)\.py", script, re.M)]
    if re.search(r"unittest discover -s crates/fmn-python/tests -p 'test_\*\.py'", script):
        direct += [(path.stem, "discover") for path in sorted(tests.glob("test_*.py"))]
    seen = {name for name, _ in suites}
    for name, mode in direct:
        if name not in seen:
            suites.append((name, mode))
            seen.add(name)
    return suites


class Probes:
    """Where each ledger row's implementation runs, for the profiler."""

    def __init__(self, rows: list[dict]):
        self.code = collections.defaultdict(list)     # function code object -> [row]
        self.native = collections.defaultdict(list)   # native function name -> [row]
        self.members = collections.defaultdict(list)  # member name -> [(row, owner class)]
        self.native_members = set()                   # member names with a native implementation
        self.classes = collections.defaultdict(list)  # class -> [row]
        # code object -> member names it answers for, when its own name differs: the portal's
        # installers rename __name__ and __qualname__ but not co_name (Animation.copy runs code
        # named animation_copy).
        self.renamed = collections.defaultdict(set)
        self.status = {}                              # row -> ok | placeholder | error: ...
        self.implementation = {}                      # row -> (owner, attribute name, value)
        # member name -> {id(type): (weak reference, rows)}: the suites assert that the classes
        # they define are collected, so the memo must never keep one alive.
        self._matches = collections.defaultdict(dict)
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
            if row["kind"] == "class" and isinstance(value, type):
                self.classes[value].append(index)
            elif isinstance(owner, type) and not isinstance(value, staticmethod):
                # A member is called through its name on an instance, whichever override
                # (or installer wrapper) of that name then runs.
                self.members[name].append((index, owner))
                code = getattr(function, "__code__", None)
                if code is None:
                    self.native_members.add(name)
                elif code.co_name != name:
                    self.renamed[code].add(name)
            elif getattr(function, "__code__", None) is not None:
                # A function, or a staticmethod member, which runs with no instance receiver
                # (StringMobject.span_contains, the Mobject.affects_data decorator): calling
                # it means running its code.
                self.code[function.__code__].append(index)
            elif callable(function):
                self.native[getattr(function, "__name__", name)].append(index)
        # A code object installed under several names (a getter factory, a decorator's shared
        # wrapper) cannot say which member ran, so it answers for none of them.
        self.renamed = {code: tuple(names) for code, names in self.renamed.items()
                        if len(names) == 1}

    def matches(self, name: str, cls: type) -> tuple[int, ...]:
        """Member rows answered by this receiver, memoized without owning its class.
        Class rows instead require a completed construction: a helper returning normally
        inside an ultimately refusing constructor cannot witness that constructor."""
        memo = self._matches[name]
        entry = memo.get(id(cls))
        if entry is not None and entry[0]() is cls:
            return entry[1]
        found = tuple(row for row, owner in self.members.get(name, ())
                      if issubclass(cls, owner))
        try:
            memo[id(cls)] = (weakref.ref(cls), found)
        except TypeError:  # a type that refuses weak references is not memoized
            pass
        return found


def resolve(module: str, symbol: str):
    """(owner, attribute name, static value) for `module`'s `symbol`, dotted through classes."""
    owner, value = None, importlib.import_module(module)
    name = symbol
    for name in symbol.split("."):
        owner, value = value, inspect.getattr_static(value, name)
    return owner, name, value


def callable_of(value):
    """The function a call or attribute read runs: unwrap property, classmethod, staticmethod,
    and a code-less caching wrapper (functools.lru_cache), whose wrapped function runs on a
    miss."""
    if isinstance(value, property):
        return value.fget
    function = getattr(value, "__func__", value)
    wrapped = getattr(function, "__wrapped__", None)
    if getattr(function, "__code__", None) is None and getattr(wrapped, "__code__", None) is not None:
        return wrapped
    return function


MONITOR = sys.monitoring
TOOL = MONITOR.PROFILER_ID
NATIVE_CALLABLES = (types.BuiltinFunctionType, types.MethodDescriptorType,
                    types.WrapperDescriptorType, types.MethodWrapperType,
                    types.ClassMethodDescriptorType)


class Recorder:
    """Which rows each test identity invokes, and whether it renders, through sys.monitoring
    (PEP 669). A code object or call site that can never matter is disabled after its first
    event, which keeps bridge.py's 900 KB of top-level calls affordable.

    The identity is the running unittest test (`current`, set by _Result), else the innermost
    running `test*` function of the suite file, else the suite's top-level statement."""

    def __init__(self, probes: Probes, suite_file: str, render_rows: set[int]):
        self.probes = probes
        self.suite_file = suite_file
        self.render_rows = render_rows
        self.current = None                           # the running unittest test, if any
        self.stack = []                               # running test* frames of the suite file
        self.hits = collections.defaultdict(set)      # identity -> rows
        self.successes = collections.defaultdict(set)  # calls that returned normally
        self.refusals = collections.defaultdict(set)    # calls that unwound with an exception
        self.renders = set()                          # identities that captured a frame
        # Store frame identities, never frames/receivers: the suites test collection of
        # authored classes and objects. A new PY_START replaces any abandoned generator's
        # entry if its frame identity is recycled; only PY_RETURN earns success.
        self.python_calls = {}
        self.native_calls = {}
        self.local_codes = set()
        self.module_line = None
        self.module_identity = "<module>"

    def __enter__(self):
        MONITOR.use_tool_id(TOOL, "fmn-portal-witnesses")
        MONITOR.restart_events()  # what an earlier session disabled may matter to this one
        MONITOR.register_callback(TOOL, MONITOR.events.PY_START, self._start)
        MONITOR.register_callback(TOOL, MONITOR.events.PY_RETURN, self._return)
        MONITOR.register_callback(TOOL, MONITOR.events.PY_UNWIND, self._unwind)
        MONITOR.register_callback(TOOL, MONITOR.events.CALL, self._call)
        MONITOR.register_callback(TOOL, MONITOR.events.C_RETURN, self._native_return)
        MONITOR.register_callback(TOOL, MONITOR.events.C_RAISE, self._native_raise)
        MONITOR.register_callback(TOOL, MONITOR.events.LINE, self._line)
        MONITOR.set_events(TOOL, MONITOR.events.PY_START | MONITOR.events.PY_UNWIND
                           | MONITOR.events.CALL | MONITOR.events.C_RETURN | MONITOR.events.C_RAISE)
        return self

    def __exit__(self, *exc):
        MONITOR.set_events(TOOL, 0)
        for code in self.local_codes:
            MONITOR.set_local_events(TOOL, code, 0)
        for event in (MONITOR.events.PY_START, MONITOR.events.PY_RETURN, MONITOR.events.CALL,
                      MONITOR.events.PY_UNWIND, MONITOR.events.C_RETURN, MONITOR.events.C_RAISE,
                      MONITOR.events.LINE):
            MONITOR.register_callback(TOOL, event, None)
        MONITOR.free_tool_id(TOOL)
        self.python_calls.clear()
        self.native_calls.clear()
        self.local_codes.clear()
        self.stack.clear()
        return False

    def _key(self):
        """The current identity, cheaply: a test name, or the suite's top-level line number
        (spelled `<module>:L<line>` only when the records are written)."""
        if self.current is not None:
            return self.current
        if self.stack:
            return self.stack[-1][1]
        if self.module_line is not None:
            return self.module_line
        return self.module_identity

    def spell(self, key) -> str:
        return f"{self.module_identity}:L{key}" if isinstance(key, int) else key

    def _hit_all(self, rows) -> None:
        if not rows:
            return
        key = self._key()
        self.hits[key].update(rows)

    def _outcome(self, call, success):
        if call is None:
            return
        key, rows = call
        (self.successes if success else self.refusals)[key].update(rows)
        if success and not self.render_rows.isdisjoint(rows):
            self.renders.add(key)

    def _python_outcome(self, frame, code, success):
        call = self.python_calls.pop(id(frame), None)
        # An abandoned generator owns no retained frame here. Its numeric identity
        # can later belong to another code object; that return earns it no witness.
        if call is not None and call[0] is code:
            self._outcome(call[1:], success)

    def _local(self, code, events):
        self.local_codes.add(code)
        MONITOR.set_local_events(TOOL, code, events)

    def _start(self, code, offset):
        rows = self.probes.code.get(code)
        names = self.probes.renamed.get(code, ())
        if code.co_name in self.probes.members and "." in code.co_qualname:
            names = (code.co_name, *names)
        in_suite = code.co_filename == self.suite_file
        test = in_suite and code.co_name.startswith("test")
        module = in_suite and code.co_name == "<module>"
        if not (rows or names or test or module):
            return MONITOR.DISABLE
        if module:
            # LINE events on the module code alone give the running top-level statement.
            # Reading the frame's f_lineno instead scans bridge.py's whole line table per hit.
            self._local(code, MONITOR.events.LINE)
            return None
        frame = sys._getframe(1)
        if test:
            self.stack.append((frame, frame_identity(frame)))
            self._local(code, MONITOR.events.PY_RETURN)
            return None
        found = set(rows or ())
        if names:
            instance = first_argument(frame)
            if instance is not None:
                for name in names:
                    found.update(self.probes.matches(name, type(instance)))
        if found:
            self._hit_all(found)
            self.python_calls[id(frame)] = (code, self._key(), tuple(found))
            self._local(code, MONITOR.events.PY_RETURN)
        return None

    def _line(self, code, line):
        self.module_line = line

    def _return(self, code, offset, value):
        frame = sys._getframe(1)
        self._python_outcome(frame, code, True)
        if self.stack and self.stack[-1][0] is frame:
            self.stack.pop()

    def _unwind(self, code, offset, exception):
        frame = sys._getframe(1)
        self._python_outcome(frame, code, False)
        if self.stack and self.stack[-1][0] is frame:
            self.stack.pop()

    def _call(self, code, offset, function, arg0):
        """Track constructions and native calls until their C_RETURN/C_RAISE outcome.
        Python functions instead complete at PY_RETURN/PY_UNWIND. Keep indirect call
        sites enabled: the next target at the same offset can be a different provider."""
        rows = ()
        if isinstance(function, type):
            rows = self.probes.classes.get(function)
            if not rows:
                return None
        elif isinstance(function, NATIVE_CALLABLES):
            name = getattr(function, "__name__", None)
            rows = self.probes.native.get(name, ())
            if name in self.probes.native_members:
                instance = getattr(function, "__self__", None)
                if instance is None or isinstance(instance, types.ModuleType):
                    instance = arg0
                if instance is not MONITOR.MISSING:
                    rows = (*rows, *self.probes.matches(name, type(instance)))
        if not rows:
            # An indirect call site may next call a probed class/native method.
            # Disabling it after one unrelated target silently loses those outcomes.
            return None
        self._hit_all(rows)
        self.native_calls[(id(sys._getframe(1)), offset)] = (self._key(), tuple(rows))
        return None

    def _native_return(self, code, offset, function, arg0):
        self._outcome(self.native_calls.pop((id(sys._getframe(1)), offset), None), True)

    def _native_raise(self, code, offset, function, arg0):
        self._outcome(self.native_calls.pop((id(sys._getframe(1)), offset), None), False)


def first_argument(frame):
    """The receiver of a method-like call: its first positional parameter, or the first element
    of `*args` (installer wrappers are often `def wrapper(*args, **kwargs)`)."""
    code = frame.f_code
    if code.co_argcount:
        return frame.f_locals.get(code.co_varnames[0])
    if code.co_flags & inspect.CO_VARARGS:
        args = frame.f_locals.get(code.co_varnames[code.co_kwonlyargcount])
        return args[0] if args else None
    return None


def frame_identity(frame) -> str:
    instance = frame.f_locals.get("self")
    name = frame.f_code.co_name
    if isinstance(instance, unittest.TestCase):
        return f"{type(instance).__name__}.{name}"
    return name


MUTANT_CALLS = {"applied": 0}


def mutate(probes: Probes, row: int, kind: str) -> None:
    """Replace row's implementation on its owner: `trivial` returns None, `shifted` adds 0.1 to
    a numeric array result. MUTANT_CALLS counts the calls the mutation changed, so a mutant that
    never changed a result is inapplicable, not a survivor."""
    owner, name, value = probes.implementation[row]
    original = callable_of(value)

    def trivial(*args, **kwargs):
        MUTANT_CALLS["applied"] += 1
        return None

    def shifted(*args, **kwargs):
        import numpy as np
        result = original(*args, **kwargs)
        if isinstance(result, np.ndarray) and result.dtype.kind == "f":
            MUTANT_CALLS["applied"] += 1
            return result + 0.1
        return result

    replacement = trivial if kind == "trivial" else shifted
    if isinstance(value, property):
        replacement = property(replacement, value.fset, value.fdel)
    elif isinstance(value, staticmethod):
        replacement = staticmethod(replacement)
    elif isinstance(value, classmethod):
        replacement = classmethod(replacement)
    setattr(owner, name, replacement)


class _PlainRecorder:
    """No profiler and no ledger import: the suite runs as the gate runs it (fm-6l60)."""

    def __init__(self):
        self.current = None
        self.hits, self.renders = {}, set()


def trace_suite(name: str, mode: str, mutation: str | None, plain: bool = False) -> list[dict]:
    """Run one suite under the profiler the way the gate runs it (`mode`: `runpy` or `main`).
    One record per test identity, then a summary record. `plain` runs it untraced, without
    importing the portal first, and returns only the summary."""
    if plain:
        rows, probes, render_rows = [], None, set()
    else:
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
    recorder = _PlainRecorder() if plain else Recorder(probes, str(suite), render_rows)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    error = None
    # Standard unittest runners report original per-test outcomes through _Result.
    results = []
    namespace = {}

    def make_result(runner):
        result = _Result(recorder, runner.stream, runner.descriptions, runner.verbosity)
        results.append(result)
        return result

    original_factory = unittest.TextTestRunner._makeResult
    original_argv, original_path = sys.argv, list(sys.path)
    # Observe the standard runner's original execution, preserving its loader,
    # flags, output and __main__ lifetime. Replaying cases after runpy returns
    # both doubles side effects and breaks pickle's class lookup.
    unittest.TextTestRunner._makeResult = make_result
    sys.argv = [str(suite)]
    sys.path.insert(0, str(TESTS))
    try:
        # A mutant run needs only pass/fail, so it runs untraced.
        with recorder if mutation is None and not plain else contextlib.nullcontext():
            if mode == "discover":
                module = importlib.import_module(name)
                namespace = vars(module)
                unittest.TextTestRunner().run(unittest.defaultTestLoader.loadTestsFromModule(module))
            elif mode == "main":
                # `python3 suite.py`: classes pickle and multiprocessing children re-import it.
                namespace = runpy.run_path(str(suite), run_name="__main__")
            else:
                namespace = runpy.run_path(str(suite), init_globals={"_expected_package_version": version})
    except SystemExit as caught:
        # unittest.main's normal failure exit does not erase other passing
        # identities. The failed cases still make the registry check fail.
        failure_exit = caught.code == 1 and any(not result.wasSuccessful() for result in results)
        if caught.code not in (None, 0) and not failure_exit:
            error = f"SystemExit: {caught.code}"
    except BaseException as caught:  # noqa: BLE001 — a raising script suite witnesses nothing
        error = f"{type(caught).__name__}: {str(caught)[:200]}"
    finally:
        unittest.TextTestRunner._makeResult = original_factory
        sys.argv = original_argv
        sys.path[:] = original_path
    defined = any(isinstance(value, type) and issubclass(value, unittest.TestCase)
                  and value.__module__ == namespace.get("__name__")
                  for value in list(namespace.values()))
    unittest_suite = defined or bool(results)
    passed = set().union(*(result.passed for result in results))
    failed = set().union(*(result.failed for result in results))
    records = []
    for key, hit in recorder.hits.items():
        identity = recorder.spell(key)
        if unittest_suite:
            # A unittest suite's witnesses are its passing tests; import-time work is not one.
            ok = error is None and identity in passed and identity not in failed
        else:
            ok = error is None
        records.append({"suite": name, "test": identity, "passed": ok,
                        "renders": key in recorder.renders,
                        "rows": sorted(recorder.successes[key]),
                        "refusals": sorted(recorder.refusals[key] - recorder.successes[key]),
                        "trace_version": VERSION})
    records.sort(key=lambda record: record["test"])
    # A unittest suite's cases are its tests run (skips excluded); a script suite's, the
    # statements that invoked a ledger symbol.
    cases = len(passed | failed) if unittest_suite else len(recorder.hits)
    records.append({"suite": name, "summary": True, "unittest": unittest_suite, "cases": cases,
                    "failed": sorted(failed), "error": error,
                    "mutant_applied": MUTANT_CALLS["applied"]})
    return records


class _Result(unittest.TextTestResult):
    """Pass/fail per test identity (Class.method), and the running test for the recorder."""

    def __init__(self, recorder: Recorder, stream=None, descriptions=True, verbosity=0):
        if stream is None:
            stream = unittest.TextTestRunner(verbosity=0).stream
        super().__init__(stream, descriptions, verbosity)
        self.recorder = recorder
        self.passed, self.failed = set(), set()

    @staticmethod
    def _identity(test) -> str:
        if isinstance(test, unittest.FunctionTestCase):
            return test.id()
        return f"{type(test).__name__}.{test._testMethodName}"

    def startTest(self, test):
        super().startTest(test)
        self.recorder.current = self._identity(test)

    def stopTest(self, test):
        super().stopTest(test)
        self.recorder.current = None

    def addSuccess(self, test):
        super().addSuccess(test)
        self.passed.add(self._identity(test))

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self.failed.add(self._identity(test))

    def addError(self, test, err):
        super().addError(test, err)
        self.failed.add(self._identity(test))

    def addSkip(self, test, reason):
        super().addSkip(test, reason)

    def addSubTest(self, test, subtest, err):
        super().addSubTest(test, subtest, err)
        if err is not None:
            self.failed.add(self._identity(test))

    def addUnexpectedSuccess(self, test):
        super().addUnexpectedSuccess(test)
        self.failed.add(self._identity(test))


def run_suites(suites: list[tuple[str, str]], jobs: int, mutation: str | None = None) -> list[dict]:
    def one(suite):
        name, mode = suite
        command = [sys.executable, __file__, "trace", name, "--mode", mode]
        if mutation:
            command += ["--mutate", mutation]
        try:
            done = subprocess.run(command, capture_output=True, text=True, timeout=SUITE_TIMEOUT_S,
                                  cwd=ROOT, check=False)
        except subprocess.TimeoutExpired:
            return [{"suite": name, "summary": True, "cases": 0, "failed": [],
                     "error": f"timeout after {SUITE_TIMEOUT_S} s"}]
        records = []
        for line in done.stdout.splitlines():
            if line.startswith('{"suite"'):
                try:
                    records.append(json.loads(line))
                except json.JSONDecodeError:
                    continue  # a suite's own output that happened to start like a record
        if not any(r.get("summary") for r in records):
            records.append({"suite": name, "summary": True, "cases": 0, "failed": [],
                            "error": f"exit {done.returncode}: {done.stderr.strip()[-200:]}"})
        summary = records[-1] if records[-1].get("summary") else None
        if (mutation is None and mode != "discover" and summary is not None
                and (summary["failed"] or summary["error"])):
            records[-1] = reconcile(summary, plain_summary(name, mode))
        return records
    with concurrent.futures.ThreadPoolExecutor(max_workers=jobs) as pool:
        records = [record for records in pool.map(one, suites) for record in records]
    discovered = {name for name, mode in suites if mode == "discover"}
    if mutation is None and failing_discovered(records, discovered):
        records = reconcile_discovered(records, discovered, plain_discover())
    return records


def failing_discovered(records: list[dict], discovered: set[str]) -> list[dict]:
    return [r for r in records if r.get("summary") and r["suite"] in discovered
            and (r["failed"] or r["error"])]


def reconcile_discovered(records: list[dict], discovered: set[str],
                         verdict: dict[str, dict]) -> list[dict]:
    """Each failing discovery suite's summary, reconciled with the plain discover verdict for
    its module (a module absent from the verdict had no failures there)."""
    failing_ids = {id(record) for record in failing_discovered(records, discovered)}
    return [reconcile(record, verdict.get(record["suite"], {"failed": [], "error": None}))
            if id(record) in failing_ids else record for record in records]


def plain_summary(name: str, mode: str) -> dict:
    """The suite's summary run untraced and without the portal pre-imported (fm-6l60)."""
    command = [sys.executable, __file__, "trace", name, "--mode", mode, "--plain"]
    try:
        done = subprocess.run(command, capture_output=True, text=True, timeout=SUITE_TIMEOUT_S,
                              cwd=ROOT, check=False)
    except subprocess.TimeoutExpired:
        return {"suite": name, "summary": True, "cases": 0, "failed": [],
                "error": f"timeout after {SUITE_TIMEOUT_S} s"}
    for line in reversed(done.stdout.splitlines()):
        if line.startswith('{"suite"'):
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            if record.get("summary"):
                return record
    return {"suite": name, "summary": True, "cases": 0, "failed": [],
            "error": f"exit {done.returncode}: {done.stderr.strip()[-200:]}"}


def plain_discover() -> dict[str, dict]:
    """The gate's own `unittest discover` of the tests directory, untraced and without the
    portal pre-imported: per module, the failed test identities, or the import error."""
    command = [sys.executable, __file__, "discover-plain"]
    try:
        done = subprocess.run(command, capture_output=True, text=True, cwd=ROOT, check=False,
                              timeout=SUITE_TIMEOUT_S * 4)
    except subprocess.TimeoutExpired:
        return {}
    for line in reversed(done.stdout.splitlines()):
        if line.startswith('{"discover"'):
            try:
                return json.loads(line)["discover"]
            except (json.JSONDecodeError, KeyError):
                break
    return {}


def discover_plain() -> dict[str, dict]:
    """Run `python -m unittest discover -s TESTS -p 'test_*.py'` in this process (a fresh one)
    and record per module what failed, by the identities trace_suite uses."""
    class Collect(unittest.TextTestResult):
        def __init__(self, *args, **kwargs):
            super().__init__(*args, **kwargs)
            self.by_module = collections.defaultdict(lambda: {"failed": set(), "error": None})

        def _note(self, test):
            if isinstance(test, unittest.case._SubTest):
                test = test.test_case
            module = type(test).__module__
            if module == "unittest.loader":  # a module that failed to import
                self.by_module[test._testMethodName]["error"] = "import failed"
                return
            self.by_module[module]["failed"].add(_Result._identity(test))

        def addFailure(self, test, err):
            super().addFailure(test, err)
            self._note(test)

        def addError(self, test, err):
            super().addError(test, err)
            self._note(test)

        def addSubTest(self, test, subtest, err):
            super().addSubTest(test, subtest, err)
            if err is not None:
                self._note(test)

    sys.path.insert(0, str(TESTS))
    tests = unittest.defaultTestLoader.discover(str(TESTS), pattern="test_*.py")
    with open(os.devnull, "w", encoding="utf-8") as sink:
        result = unittest.TextTestRunner(stream=sink, resultclass=Collect).run(tests)
    return {module: {"failed": sorted(entry["failed"]), "error": entry["error"]}
            for module, entry in result.by_module.items()}


def reconcile(traced: dict, plain: dict) -> dict:
    """A traced summary corrected by the gate-faithful plain rerun: the suite's failures and
    error are the plain run's; what failed only traced is kept as harness_only. The traced
    values stay as traced_failed and traced_error, the baseline mutants compare against."""
    summary = dict(traced)
    summary["traced_failed"], summary["traced_error"] = traced["failed"], traced["error"]
    summary["failed"], summary["error"] = sorted(plain["failed"]), plain["error"]
    summary["harness_only"] = sorted(set(traced["failed"]) - set(plain["failed"]))
    if traced["error"] and not plain["error"]:
        summary["harness_error"] = traced["error"]
    return summary


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
    refusals = collections.defaultdict(set)
    for record in records:
        if record.get("summary") or not record["passed"]:
            continue
        identity = f"{record['suite']}::{record['test']}"
        for row in set(record.get("refusals", ())) - set(record["rows"]):
            refusals[row].add(identity)
        for row in record["rows"]:
            semantic[row].add(identity)
            if record["renders"]:
                rendered[row].add(identity)
    corpus = corpus_counts(rows)
    out = []
    for index, row in enumerate(rows):
        if row["status"] not in WITNESSED_STATUSES:
            continue
        value = probes.implementation.get(index, (None, None, None))[2]
        out.append({
            "module": row["module"], "symbol": row["symbol"], "kind": row["kind"],
            "status": row["status"], "import": probes.status.get(index, "error"),
            "semantic": len(semantic[index]), "rendered": len(rendered[index]),
            "refusal_only": len(refusals[index]),
            "corpus": corpus.get(index, "-"),
            "example": min(semantic[index]) if semantic[index] else "-",
            # Not a registry column: check's refusal-only rule exempts abstract classes.
            "abstract": isinstance(value, type) and inspect.isabstract(value),
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
        "refusal_only": sum(r["semantic"] == 0 and r["refusal_only"] > 0 for r in table),
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
    columns = None
    version = None
    # v1 recorded call entry, without an outcome. Keep its original column meanings
    # when checking the prior baseline; never shift its rendered count into refusal_only.
    prior_columns = tuple(c for c in COLUMNS if c != "refusal_only")
    for line in text.splitlines():
        match = re.match(r"# fmn\.portal-witnesses v(\d+):", line)
        if match:
            version = int(match.group(1))
            if version not in (1, VERSION):
                raise ValueError("unsupported portal witness registry version")
        if line.startswith("module\t"):
            columns = tuple(line.split("\t"))
            expected = prior_columns if version == 1 else COLUMNS
            if version is None or columns != expected:
                raise ValueError("unsupported portal witness registry columns")
            continue
        if not line or line.startswith("#"):
            continue
        fields = line.split("\t")
        if columns is None or len(fields) != len(columns):
            raise ValueError("malformed portal witness registry row")
        row = dict(zip(columns, fields))
        rows[(row["module"], row["symbol"])] = row
    return rows


def registry_problems(table: list[dict], records: list[dict],
                      committed: dict[tuple[str, str], dict]) -> list[str]:
    """What `check` fails on, before the mutants: a suite that defines tests and executes none
    (every one skipped, say), a same/improved symbol that resolves to a placeholder or does not
    resolve, a same/improved row with only refusal witnesses, and a symbol the committed
    registry records as witnessed that no passing test completes now. A script suite that
    never runs is refused by the gate's own runner."""
    problems = []
    for record in records:
        if record.get("summary"):
            if record.get("error"):
                problems.append(f"{record['suite']}: suite failed: {record['error']}")
            elif record.get("failed"):
                problems.append(f"{record['suite']}: {len(record['failed'])} test identities failed")
        if (record.get("summary") and record.get("unittest") and record["cases"] == 0
                and not record["error"]):
            problems.append(f"{record['suite']}: defines tests but executed none")
    for row in table:
        if row["import"] != "ok":
            problems.append(f"{row['module']}.{row['symbol']}: {row['status']} in the ledger, but "
                            f"it resolves to {row['import']}")
        if row["semantic"] == 0 and row["refusal_only"] > 0 and not row.get("abstract"):
            problems.append(f"{row['module']}.{row['symbol']}: {row['status']} in the ledger, but "
                            f"its {row['refusal_only']} passing witnesses only exercise refusals")
    now = {(r["module"], r["symbol"]): r for r in table}
    for key, row in committed.items():
        if int(row["semantic"]) > 0 and now.get(key, {}).get("semantic", 0) == 0:
            problems.append(f"{key[0]}.{key[1]}: lost every semantic witness")
    return problems


GEOMETRY_NAMES = re.compile(r"point|coord|center|corner|anchor|bounding_box|vector|direction|_to_")


def sample_mutants(table: list[dict], python_rows: set[tuple[str, str]], count: int,
                   geometry: bool = False) -> list[str]:
    """`count` witnessed members with a Python implementation, picked by a stable hash so every
    run picks the same ones. `geometry` keeps only members whose name says they return
    positions (where a shifted result is the plausible-but-wrong mutant)."""
    members = [r for r in table if r["semantic"] > 0 and r["kind"] in ("method", "property")
               and r["import"] == "ok" and (r["module"], r["symbol"]) in python_rows
               and (not geometry or GEOMETRY_NAMES.search(r["symbol"].split(".")[-1]))]
    members.sort(key=lambda r: hashlib.sha256(f"{r['module']}.{r['symbol']}".encode()).hexdigest())
    return [f"{r['module']}.{r['symbol']}" for r in members[:count]]


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = parser.add_subparsers(dest="command", required=True)
    trace = sub.add_parser("trace")
    trace.add_argument("suite")
    trace.add_argument("--mode", choices=("runpy", "main", "discover"), default="runpy")
    trace.add_argument("--mutate")
    trace.add_argument("--plain", action="store_true")
    sub.add_parser("discover-plain")
    for name in ("build", "check"):
        command = sub.add_parser(name)
        command.add_argument("--jobs", type=int, default=max(1, (os.cpu_count() or 2) // 4))
        command.add_argument("--suites", nargs="*", help="limit to these suites (diagnosis only)")
        if name == "check":
            command.add_argument("--mutants", type=int, default=0)
    args = parser.parse_args(argv)

    if args.command == "discover-plain":
        print(json.dumps({"discover": discover_plain()}))
        return 0

    if args.command == "trace":
        if args.plain:
            print(json.dumps(trace_suite(args.suite, args.mode, None, plain=True)[-1]))
            return 0
        for record in trace_suite(args.suite, args.mode, args.mutate):
            print(json.dumps(record))
        return 0

    rows = ledger_rows(LEDGER.read_text(encoding="utf-8"))
    probes = Probes(rows)
    suites = gate_suites(GATE.read_text(encoding="utf-8"))
    if args.suites:
        suites = [suite for suite in suites if suite[0] in args.suites]
    mode_of = dict(suites)
    records = run_suites(suites, args.jobs)
    table = registry(rows, probes, records)
    for line in summary_lines(table, records)[:2]:
        print(line)
    for record in records:
        if record.get("summary") and (record["error"] or record["failed"]):
            print(json.dumps({"schema": SCHEMA, "suite": record["suite"], "cases": record["cases"],
                              "failed": record["failed"][:5], "error": record["error"]}))
        if record.get("summary") and (record.get("harness_only") or record.get("harness_error")):
            print(json.dumps({"schema": SCHEMA, "suite": record["suite"],
                              "harness_only": record.get("harness_only", [])[:5],
                              "harness_error": record.get("harness_error")}))
    if args.command == "build":
        REGISTRY.write_text(write_registry(table, records), encoding="utf-8")
        return 0

    problems = registry_problems(table, records, read_registry(REGISTRY.read_text(encoding="utf-8")))
    if args.mutants:
        index_of = {f"{r['module']}.{r['symbol']}": i for i, r in enumerate(rows)}
        python_rows = {(rows[i]["module"], rows[i]["symbol"])
                       for i, (_, _, value) in probes.implementation.items()
                       if getattr(callable_of(value), "__code__", None) is not None}
        witnesses = collections.defaultdict(set)
        for record in records:
            if not record.get("summary") and record["passed"]:
                for row in record["rows"]:
                    witnesses[row].add((record["suite"], mode_of[record["suite"]]))
        baseline = {r["suite"]: r for r in records if r.get("summary")}
        killed = tried = 0
        # Trivial bodies over every kind of member; shifted results over members that return
        # positions, where a small offset is the plausible-but-wrong substitution.
        plan = [(name, "trivial") for name in sample_mutants(table, python_rows, args.mutants)]
        plan += [(name, "shifted")
                 for name in sample_mutants(table, python_rows, args.mutants, geometry=True)]
        for name, kind in plan:
            row = index_of[name]
            # One failing witness kills a mutant, so a bounded set of them is enough to rerun.
            suites_for_row = sorted(witnesses[row])[:MUTANT_SUITES]
            mutant = [r for r in run_suites(suites_for_row, args.jobs, f"{row}:{kind}")
                      if r.get("summary")]
            if any(r.get("unmutable") for r in mutant):
                print(json.dumps({"schema": SCHEMA, "mutant": name, "kind": kind,
                                  "unmutable": mutant[0]["unmutable"]}))
                continue
            if not any(r.get("mutant_applied") for r in mutant):
                # No witness run reached a changed result (a shifted mutant of a member that
                # returned no float array changes nothing), so there was nothing to kill.
                print(json.dumps({"schema": SCHEMA, "mutant": name, "kind": kind,
                                  "inapplicable": True}))
                continue
            # A mutant run is traced-environment (the portal is imported to patch it), so it is
            # compared against the traced baseline, harness-only failures included.
            died = any(set(r["failed"]) - set(baseline[r["suite"]].get("traced_failed",
                                                                       baseline[r["suite"]]["failed"]))
                       or (r["error"] and not baseline[r["suite"]].get("traced_error",
                                                                      baseline[r["suite"]]["error"]))
                       for r in mutant)
            tried += 1
            killed += died
            print(json.dumps({"schema": SCHEMA, "mutant": name, "kind": kind,
                              "suites": len(suites_for_row), "killed": bool(died)}))
            if not died:
                problems.append(f"{name}: a {kind} mutant survives its witnesses")
        print(json.dumps({"schema": SCHEMA, "mutants": tried, "killed": killed}))
    sys.stdout.flush()
    for problem in problems:
        print(problem, file=sys.stderr, flush=True)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
