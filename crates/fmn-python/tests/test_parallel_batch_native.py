"""Native wheel smoke tests; never substitute a fake renderer for pixels."""
import hashlib
import importlib.util
from importlib.machinery import ExtensionFileLoader, PathFinder
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))


def native_wheel_available():
    package = importlib.util.find_spec("manimlib")
    if package is None or package.submodule_search_locations is None:
        return False
    # Do not import a source-only package just to discover that its private
    # extension has not been built; do not mistake Reference Manim for FMN.
    extension = PathFinder.find_spec("manimlib.manimlib", package.submodule_search_locations)
    return extension is not None and isinstance(extension.loader, ExtensionFileLoader)


@unittest.skipUnless(native_wheel_available(),
                     "requires the built FrankenManim native wheel")
class NativeParallelBatchTests(unittest.TestCase):
    def test_fresh_scene_state_and_real_png_receipts(self):
        from fmn_python import render_source_scenes
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "scenes.py"
            source.write_text('''from manimlib import Scene, Square

generation = 0

def populate(scene):
    global generation
    generation += 1
    assert generation == 1, "interpreter state leaked between scene jobs"
    scene.add(Square())
    scene.wait(0.1)

class First(Scene):
    def construct(self):
        populate(self)

class Second(Scene):
    def construct(self):
        populate(self)

class Broken(Scene):
    def construct(self):
        raise RuntimeError("authored scene failed")
''', encoding="utf-8")
            report = render_source_scenes(source, root / "output", ["Second", "Broken", "First"],
                workers=2, format="png", resolution=(64, 48), fps=10, continue_on_error=True)
            self.assertEqual([row.name for row in report.outcomes], ["Second", "Broken", "First"])
            self.assertEqual([row.status for row in report.outcomes], ["succeeded", "failed", "succeeded"])
            self.assertFalse(report.ok)
            self.assertEqual(report.as_dict()["execution"], "spawn")
            for row in (report.outcomes[0], report.outcomes[2]):
                self.assertEqual(row.destination.read_bytes()[:8], b"\x89PNG\r\n\x1a\n")
                self.assertEqual(row.result.destination, row.destination)
            with self.assertRaises(FileExistsError):
                render_source_scenes(source, root / "output", ["First"], workers=1, format="png")

    def test_checkpoint_resume_reuses_native_png_and_retries_only_failed_scene(self):
        from fmn_python import render_source_scenes
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            source = root / "scenes.py"
            source.write_text('''from pathlib import Path
from manimlib import Scene, Square

root = Path(__file__).parent

def attempt(name):
    with (root / (name + ".attempts")).open("a") as stream:
        stream.write("constructed\\n")

class First(Scene):
    def __init__(self, *args, **kwargs):
        attempt("First")
        super().__init__(*args, **kwargs)
    def construct(self):
        self.add(Square())
        self.wait(0.1)

class Transient(Scene):
    def __init__(self, *args, **kwargs):
        attempt("Transient")
        super().__init__(*args, **kwargs)
    def construct(self):
        if not (root / "allow-retry").exists():
            raise RuntimeError("transient scene failure")
        self.add(Square().shift((1, 0, 0)))
        self.wait(0.1)
''', encoding="utf-8")
            options = dict(format="png", resolution=(64, 48), fps=10,
                           checkpoint=root / "progress.json", resume_key="stable-scene-assets-v1",
                           continue_on_error=True)
            names = ["First", "Transient"]

            def counts():
                return [len((root / (name + ".attempts")).read_text().splitlines()) for name in names]

            initial = render_source_scenes(source, root / "output", names, workers=2, **options)
            self.assertEqual([row.status for row in initial.outcomes], ["succeeded", "failed"])
            first_path = initial.outcomes[0].destination
            first_bytes, first_mtime = first_path.read_bytes(), first_path.stat().st_mtime_ns
            self.assertEqual(counts(), [1, 1])
            # Resolve the transient failure without changing the entry source
            # or any input of the already-completed First scene.
            (root / "allow-retry").write_text("ready", encoding="utf-8")
            resumed = render_source_scenes(source, root / "output", names, workers=1,
                                           resume=True, **options)
            self.assertTrue(resumed.ok)
            self.assertEqual(resumed.reused, ("First",))
            self.assertEqual(resumed.as_dict()["reused"], ["First"])
            self.assertEqual(counts(), [1, 2])
            self.assertEqual(first_path.read_bytes(), first_bytes)
            self.assertEqual(first_path.stat().st_mtime_ns, first_mtime)
            for row in resumed.outcomes:
                data = row.destination.read_bytes()
                self.assertEqual(data[:8], b"\x89PNG\r\n\x1a\n")
                self.assertEqual(row.result.bytes, len(data))
                self.assertEqual(row.result.digest, hashlib.sha256(data).hexdigest())
            complete = render_source_scenes(source, root / "output", names, workers=3,
                                            resume=True, **options)
            self.assertEqual(complete.reused, tuple(names))
            self.assertEqual(counts(), [1, 2])
            first_path.write_bytes(first_bytes + b"tampered")
            with self.assertRaisesRegex(ValueError, "missing or modified"):
                render_source_scenes(source, root / "output", names, workers=2,
                                     resume=True, **options)
            self.assertEqual(counts(), [1, 2])
            self.assertEqual(first_path.read_bytes(), first_bytes + b"tampered")


if __name__ == "__main__":
    unittest.main()
