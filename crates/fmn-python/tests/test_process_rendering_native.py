"""Actual native output through the supervised worker result channel."""
import hashlib
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python._process_batch import run_process_jobs
from test_parallel_batch_native import native_wheel_available


def render_job(job):
    import manimlib as m
    from fmn_python.batch_rendering import render_scenes

    class Motion(m.Scene):
        def construct(self):
            shape = m.Square(fill_opacity=1).shift(m.LEFT)
            self.add(shape)
            self.play(shape.animate.shift(m.RIGHT * 2), run_time=0.3)

    class Broken(m.Scene):
        def construct(self):
            raise RuntimeError("native scene failed deliberately")

    scene = Broken if job.get("fail") else Motion
    report = render_scenes({job["name"]: scene}, job["directory"],
        format="png_sequence", resolution=(64, 48), fps=10, threads=1,
        continue_on_error=True)
    return report.outcomes[0].as_dict()


@unittest.skipUnless(native_wheel_available(), "requires the built FrankenManim native wheel")
class NativeProcessRenderingTests(unittest.TestCase):
    def test_real_moving_png_frames_and_receipts_survive_new_channel(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            jobs = [dict(name=name, directory=str(root / "parallel"), fail=name == "Broken")
                    for name in ("First", "Broken", "Second")]
            rows = run_process_jobs(jobs, render_job, workers=2, continue_on_error=True,
                                    job_timeout=15)
            self.assertEqual([row["status"] for row in rows], ["succeeded", "failed", "succeeded"])
            self.assertIn("native scene failed", rows[1]["error"]["message"])
            sequential = render_job(dict(name="Reference", directory=str(root / "sequential")))
            reference = sorted(Path(sequential["destination"]).glob("*.png"))
            self.assertGreaterEqual(len(reference), 3)
            expected = [path.read_bytes() for path in reference]
            self.assertGreater(len({hashlib.sha256(data).digest() for data in expected}), 1)
            for row in (rows[0], rows[2]):
                frames = sorted(Path(row["destination"]).glob("*.png"))
                self.assertEqual(row["result"]["frame_count"], len(frames))
                self.assertEqual([path.read_bytes() for path in frames], expected)
                self.assertTrue(all(data.startswith(b"\x89PNG\r\n\x1a\n") for data in expected))

    def test_observer_failure_retains_already_published_native_output(self):
        with tempfile.TemporaryDirectory() as directory:
            def stop(index, row):
                raise LookupError("observer failure after native publication")
            with self.assertRaises(LookupError) as caught:
                run_process_jobs([dict(name="First", directory=directory),
                                  dict(name="NotStarted", directory=directory)],
                                 render_job, workers=1, on_result=stop, job_timeout=15)
            rows = caught.exception.process_batch_outcomes
            self.assertEqual([row["status"] for row in rows], ["succeeded", "not_run"])
            self.assertTrue(list(Path(rows[0]["destination"]).glob("*.png")))
            self.assertFalse((Path(directory) / "NotStarted").exists())


if __name__ == "__main__":
    unittest.main()
