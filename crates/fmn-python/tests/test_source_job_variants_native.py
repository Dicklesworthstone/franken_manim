"""Installed-wheel acceptance; protocol fixtures are not a substitute."""
from pathlib import Path
import tempfile
import unittest

from test_parallel_batch_native import native_wheel_available


@unittest.skipUnless(native_wheel_available(), "requires the built native wheel")
class NativeSourceVariantTests(unittest.TestCase):
    def test_native_variants_match_sequential_constructor_exports_and_resume(self):
        from fmn_python import RenderJob, render_scenes, render_source_scenes
        from fmn_python.source_jobs import SourceRenderJob
        from fmn_python.scene_loading import SceneSource
        import manimlib

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            source = root / "scenes.py"
            source.write_text('''from manimlib import Scene, Square
class Lesson(Scene):
    def __init__(self, side=1.0, **kwargs):
        super().__init__(**kwargs)
        self.side = side
    def construct(self):
        self.add(Square(side_length=self.side, fill_opacity=1))
        self.wait(0.25)
''')
            jobs = [SourceRenderJob("small", "Lesson", {"side": 0.5, "random_seed": 7}),
                    SourceRenderJob("large", "Lesson", {"side": 2.0, "random_seed": 11})]
            options = dict(format="fmtl", bundle_camera=True, resolution=(64, 36), fps=8)
            with SceneSource(source, manimlib.Scene) as loaded:
                sequential = render_scenes([RenderJob(job.name, loaded.scenes[job.scene], job.scene_kwargs)
                                            for job in jobs], root / "sequential", **options)
            parallel = render_source_scenes(source, root / "parallel", jobs, workers=2,
                checkpoint=root / "progress.json", resume_key="native-variants-v1", **options)
            self.assertTrue(sequential.ok and parallel.ok)
            for expected, actual in zip(sequential.outcomes, parallel.outcomes):
                self.assertEqual(actual.destination.read_bytes(), expected.destination.read_bytes())
                self.assertEqual(actual.result.digest, expected.result.digest)
                self.assertEqual(actual.result.frame_count, 2)
            before = [row.destination.stat().st_mtime_ns for row in parallel.outcomes]
            recovered = render_source_scenes(source, root / "parallel", jobs, workers=1,
                checkpoint=root / "progress.json", resume_key="native-variants-v1", resume=True, **options)
            self.assertEqual(recovered.reused, ("small", "large"))
            self.assertEqual([row.destination.stat().st_mtime_ns for row in recovered.outcomes], before)


if __name__ == "__main__":
    unittest.main()
