"""Cold/warm portal code loading with real source files and CPython caches.

These tests need no native extension. The source fixtures, caches and failure
inputs are retained beside the printed evidence directory.
"""
from __future__ import annotations

import importlib.util
import json
import marshal
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

_PATH = Path(__file__).resolve().parents[1] / "python/fmn_python/bootstrap_loader.py"
_SPEC = importlib.util.spec_from_file_location("bootstrap_loader_under_test", _PATH)
loader = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(loader)
_ROOT = Path(tempfile.mkdtemp(prefix="fmn-bootstrap-loader-tests-"))
_SKIN = b'''from __future__ import annotations
ROOT = _FMN_MODULE
class Example:
    def value(self, x: MissingType = 3):
        return (x + 4, ROOT, __name__)
    def selection(self, x):
        return x[1:5:2]
MEMBERS = frozenset({"alpha", "beta", "gamma"})
'''


class BootstrapLoaderTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(dir=_ROOT))
        self.path = self.root / "_embedded_bootstrap.py"
        self.path.write_bytes(_SKIN)
        self.cache = Path(importlib.util.cache_from_source(str(self.path)))
        self.addCleanup(patch.stopall)
        patch.object(loader, "_SOURCE", self.path).start()
        patch.object(sys, "dont_write_bytecode", False).start()

    def load(self, source=_SKIN):
        return loader.load_bootstrap(source)

    def test_warm_cache_avoids_compilation_and_preserves_native_globals(self):
        cold, cold_identity = self.load()
        self.assertTrue(self.cache.is_file())
        self.assertEqual(self.cache.read_bytes()[4:8], b"\x03\x00\x00\x00")
        with patch.object(loader._VerifiedSourceLoader, "source_to_code",
                          side_effect=AssertionError("warm bootstrap was recompiled")):
            warm, warm_identity = self.load()
        self.assertEqual(cold, warm)
        self.assertEqual(cold_identity, warm_identity)
        self.assertEqual(warm.co_filename, "<string>")
        for code in (cold, warm):
            native = types.ModuleType("manimlib")
            native._FMN_MODULE = native
            exec(code, vars(native), vars(native))  # ubs:ignore[py.security.eval-exec-usage,py.eval-exec] -- fixed trusted source fixture, exercising native-global execution.
            self.assertEqual(native.Example().value()[::2], (7, "manimlib"))
            self.assertIs(native.Example().value()[1], native)
            self.assertEqual(native.Example().selection(range(8)), range(1, 5, 2))
            self.assertEqual(native.Example.value.__code__.co_filename, "<string>")

    def test_slice_constants_do_not_require_a_new_marshal_format(self):
        # CPython 3.14 folds constant slices into co_consts; older hosts can
        # exercise that exact code-object shape without pretending to run 3.14.
        child = compile("answer = 1", "<string>", "exec")
        child = child.replace(co_consts=child.co_consts + (slice(1, 5, 2),))
        code = compile("pass", "<string>", "exec")
        code = code.replace(co_consts=(None, child, (slice(None, 3),)))
        with self.assertRaisesRegex(ValueError, "unmarshallable"):
            marshal.dumps(code, 2)
        with patch.object(loader, "_SOURCE", self.root / "missing.py"), \
                patch.object(loader, "compile", return_value=code, create=True):
            loaded, identity = self.load()
        self.assertEqual(loaded, code)
        self.assertEqual(identity["code_sha256"], loader._code_digest(code))

    def test_code_identity_preserves_constant_types_and_float_bits(self):
        code = compile("pass", "<string>", "exec")
        constants = (None, Ellipsis, False, 0, 0.0, -0.0, 0j, b"x", "x",
                     (1, 2), frozenset((1, 2)), slice(1, 2), slice(1, 2, 1))
        identities = {loader._code_digest(code.replace(co_consts=(constant,)))
                      for constant in constants}
        self.assertEqual(len(identities), len(constants))
        self.assertEqual(
            loader._code_digest(code.replace(co_consts=(frozenset(("a", "b", "c")),))),
            loader._code_digest(code.replace(co_consts=(frozenset(("c", "b", "a")),))),
        )

    def test_code_identity_rejects_unknown_constant_types(self):
        code = compile("pass", "<string>", "exec").replace(co_consts=({},))
        with self.assertRaisesRegex(TypeError, "unsupported bootstrap code constant type: dict"):
            loader._code_digest(code)

    def test_exception_handlers_participate_in_executed_identity(self):
        source = b'''def guarded(value):
    try:
        return 10 // value
    except ZeroDivisionError:
        return 42
'''
        compiled = compile(source, "<string>", "exec")
        code = next(value for value in compiled.co_consts if isinstance(value, types.CodeType))
        unguarded = code.replace(co_exceptiontable=b"")
        self.assertEqual(code.co_code, unguarded.co_code)
        self.assertEqual(types.FunctionType(code, {})(0), 42)
        with self.assertRaises(ZeroDivisionError):
            types.FunctionType(unguarded, {})(0)
        self.assertNotEqual(loader._code_digest(code), loader._code_digest(unguarded))

    def test_optimization_changes_the_executed_identity(self):
        source = "def example():\n    'retained docstring'\n    assert __debug__\n    return __debug__\n"
        identities = {
            loader._code_digest(compile(source, "<string>", "exec", optimize=level))
            for level in (0, 1, 2)
        }
        self.assertEqual(len(identities), 3)

    def test_full_production_bootstrap_matches_embedded_compilation(self):
        source = _PATH.parent.parent.joinpath("manimlib_bootstrap.py").read_bytes()
        self.path.write_bytes(source)
        cold, identity = self.load(source)
        with patch.object(loader._VerifiedSourceLoader, "source_to_code",
                          side_effect=AssertionError("production bootstrap was recompiled")):
            warm, cached_identity = self.load(source)
        self.assertEqual(cold, compile(source, "<string>", "exec", dont_inherit=True))
        self.assertEqual(cold, warm)
        self.assertEqual(identity, cached_identity)

    def test_missing_generated_source_uses_only_embedded_source(self):
        path = self.root / "direct_extension.py"
        with patch.object(loader, "_SOURCE", path):
            code, identity = self.load()
        self.assertEqual(code, compile(_SKIN, "<string>", "exec", dont_inherit=True))
        self.assertIsNone(identity["source_path"])
        self.assertFalse(path.exists())
        self.assertFalse(Path(importlib.util.cache_from_source(str(path))).exists())

    def test_foreign_source_is_rejected_even_with_equal_size_mtime_and_valid_cache(self):
        self.load()
        stamp = self.path.stat()
        self.path.write_bytes(_SKIN.replace(b"x + 4", b"x + 9"))
        os.utime(self.path, ns=(stamp.st_atime_ns, stamp.st_mtime_ns))
        with self.assertRaisesRegex(ImportError, "differs from the native extension"):
            self.load()

    def test_stale_foreign_and_malformed_bytecode_recompile(self):
        _, expected = self.load()
        good = self.cache.read_bytes()
        damaged = (
            b"", b"bad!" + good[4:], good[:4] + bytes(4) + good[8:],
            good[:8] + bytes(8) + good[16:], good[:16] + b"!",
            good[:16] + marshal.dumps(42), good[:16],
        )
        for payload in damaged:
            with self.subTest(payload=payload[:24]):
                self.cache.write_bytes(payload)
                code, actual = self.load()
                self.assertEqual(expected, actual)
                self.assertEqual(code, compile(_SKIN, "<string>", "exec", dont_inherit=True))
                self.assertEqual(self.cache.read_bytes()[:16], good[:16])

    def test_unsupported_cached_constant_recompiles_and_repairs_cache(self):
        original, expected = self.load()
        header = self.cache.read_bytes()[:16]
        # Marshal accepts mutable containers inside code constants, even
        # though the compiler never puts them there. Header validation and
        # get_code accept this damaged body; identity validation must reject
        # it and retry from the source already verified above.
        damaged = original.replace(co_consts=original.co_consts + ({},))
        self.cache.write_bytes(header + marshal.dumps(damaged))
        compilations = []
        compile_source = loader._VerifiedSourceLoader.source_to_code

        def verified_compile(instance, source, path, **kwargs):
            compilations.append(source)
            return compile_source(instance, source, path, **kwargs)

        with patch.object(loader._VerifiedSourceLoader, "source_to_code", verified_compile):
            repaired, observed = self.load()
        self.assertEqual(compilations, [_SKIN])
        self.assertEqual(repaired, original)
        self.assertEqual(observed, expected)
        self.assertEqual(self.cache.read_bytes()[:16], header)
        with patch.object(loader._VerifiedSourceLoader, "source_to_code",
                          side_effect=AssertionError("repaired bootstrap was recompiled")):
            warm, cached_identity = self.load()
        self.assertEqual(warm, original)
        self.assertEqual(cached_identity, expected)

    def test_cache_recovery_preserves_source_compilation_errors(self):
        original, _ = self.load()
        source = b"def broken(:\n"
        self.path.write_bytes(source)
        header = (importlib.util.MAGIC_NUMBER + b"\x03\x00\x00\x00"
                  + importlib.util.source_hash(source))
        damaged = original.replace(co_consts=original.co_consts + ({},))
        self.cache.write_bytes(header + marshal.dumps(damaged))
        with self.assertRaises(SyntaxError):
            self.load(source)

    def test_content_hash_is_checked_even_if_host_disables_hash_pyc_checks(self):
        import _imp
        self.load()
        data = self.cache.read_bytes()
        alien = compile(_SKIN.replace(b"x + 4", b"x + 9"), str(self.path), "exec")
        self.cache.write_bytes(data[:8] + bytes(8) + marshal.dumps(alien))
        with patch.object(_imp, "check_hash_based_pycs", "never"):
            code, _ = self.load()
        self.assertEqual(code, compile(_SKIN, "<string>", "exec", dont_inherit=True))

    def test_cache_is_optional_under_no_bytecode_or_unwritable_installation(self):
        with patch.object(sys, "dont_write_bytecode", True):
            code, _ = self.load()
        self.assertFalse(self.cache.exists())
        with patch.object(importlib.machinery.SourceFileLoader, "set_data",
                          side_effect=PermissionError("read-only install")):
            uncached, _ = self.load()
        self.assertEqual(code, uncached)
        self.assertFalse(self.cache.exists())

    def test_oversized_cache_is_a_miss_and_oversized_source_is_a_refusal(self):
        _, expected = self.load()
        with patch.object(loader, "_MAX_BYTECODE_BYTES", 16):
            _, actual = self.load()
        self.assertEqual(expected, actual)
        with patch.object(loader, "_MAX_SOURCE_BYTES", len(_SKIN) - 1):
            with self.assertRaisesRegex(ImportError, "bounded source"):
                self.load()
        self.path.write_bytes(_SKIN + b"# too large\n")
        with patch.object(loader, "_MAX_SOURCE_BYTES", len(_SKIN)):
            with self.assertRaisesRegex(ImportError, "cannot verify"):
                self.load()

    @unittest.skipUnless(hasattr(os, "mkfifo"), "POSIX FIFO witness")
    def test_fifo_cache_does_not_block_startup(self):
        self.cache.parent.mkdir()
        os.mkfifo(self.cache)
        code, _ = self.load()
        self.assertEqual(code, compile(_SKIN, "<string>", "exec", dont_inherit=True))

    def test_executed_bytecode_body_participates_in_runtime_identity(self):
        _, expected = self.load()
        header = self.cache.read_bytes()[:16]
        altered = compile(_SKIN.replace(b"x + 4", b"x + 9"), str(self.path), "exec")
        self.cache.write_bytes(header + marshal.dumps(altered))
        code, observed = self.load()
        self.assertNotEqual(code, compile(_SKIN, "<string>", "exec", dont_inherit=True))
        self.assertEqual(expected["source_sha256"], observed["source_sha256"])
        self.assertNotEqual(expected["code_sha256"], observed["code_sha256"])

    def test_relocation_and_fresh_processes_preserve_executed_identity(self):
        # Run real independent interpreters with different hash seeds; the
        # second one is forbidden to compile and must consume the first cache.
        source = _PATH.parent.parent.joinpath("manimlib_bootstrap.py").read_bytes()
        self.path.write_bytes(source)
        probe = '''import importlib.util, json, pathlib, sys
spec = importlib.util.spec_from_file_location("bootstrap_probe", sys.argv[1])
loader = importlib.util.module_from_spec(spec)
spec.loader.exec_module(loader)
loader._SOURCE = pathlib.Path(sys.argv[2])
source = loader._SOURCE.read_bytes()
if sys.argv[3] == "embedded":
    loader._SOURCE = loader._SOURCE.with_name("missing_bootstrap.py")
if sys.argv[3] == "warm":
    def fail(*args, **kwargs):
        raise AssertionError("fresh process recompiled a warm bootstrap")
    loader._VerifiedSourceLoader.source_to_code = fail
code, identity = loader.load_bootstrap(source)
identity.pop("source_path")
print(json.dumps(identity, sort_keys=True))
'''
        observed = []
        optimization = ["-" + "O" * sys.flags.optimize] if sys.flags.optimize else []
        for mode, seed in (("cold", "1"), ("warm", "2"), ("embedded", "3")):
            env = dict(os.environ, PYTHONHASHSEED=seed)
            env.pop("PYTHONDONTWRITEBYTECODE", None)
            result = subprocess.run(
                [sys.executable, *optimization, "-c", probe, str(_PATH), str(self.path), mode],
                env=env, capture_output=True, text=True, timeout=20, check=True,
            )
            observed.append(json.loads(result.stdout))
        self.assertEqual(observed[0], observed[1])
        self.assertEqual(observed[0], observed[2])
        other = self.root / "another_install.py"
        other.write_bytes(source)
        with patch.object(loader, "_SOURCE", other):
            _, relocated = self.load(source)
        relocated.pop("source_path")
        self.assertEqual(observed[0], relocated)


if __name__ == "__main__":
    try:
        unittest.main()
    finally:
        print("bootstrap loader evidence:", _ROOT)
