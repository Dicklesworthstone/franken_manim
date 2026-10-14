"""Native wheel smoke tests; never substitute a fake renderer for pixels."""
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


if __name__ == "__main__":
    unittest.main()
