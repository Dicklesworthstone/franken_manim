"""Installed-wheel acceptance for parallel per-scene clip-prefix recovery."""
import hashlib
from pathlib import Path
import tempfile
import unittest

from test_parallel_batch_native import native_wheel_available

SOURCE = '''from pathlib import Path
from manimlib import Scene, Square, RIGHT
class Lesson(Scene):
    def construct(self):
        root = Path(__file__).parent
        with (root / (type(self).__name__ + ".calls")).open("a") as stream:
            stream.write("construct\\n")
        square = Square()
        self.add(square)
        self.wait(0.125)
        if (root / "fail").exists():
            raise RuntimeError("interrupted after a published clip")
        self.play(square.animate.shift(RIGHT), run_time=0.25)
        self.wait(0.125)
class Other(Lesson): pass
'''


@unittest.skipUnless(native_wheel_available(), "requires a matching built native wheel")
class ParallelClipRecoveryNativeTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-native-prefix-"))
        self.source = self.root / "scenes.py"
        self.source.write_text(SOURCE)
        self.output, self.state = self.root / "out", self.root / "state"

    def render(self, **options):
        from fmn_python.parallel_batch import render_source_scenes
        return render_source_scenes(self.source, self.output, ["Lesson", "Other"],
            workers=2, format="gif", resolution=(64, 48), fps=8, threads=1,
            subdivide=True, clip_checkpoint_dir=self.state, resume_key="native-v1",
            continue_on_error=True, **options)

    def test_partial_native_collections_resume_without_reencoding_prefix(self):
        flag = self.root / "fail"
        flag.touch()
        first = self.render()
        self.assertEqual([row.status for row in first.outcomes], ["failed", "failed"])
        saved = {}
        for row in first.outcomes:
            self.assertEqual(len(row.result.segments), 1)
            receipt = row.result.segments[0].render
            data = receipt.destination.read_bytes()
            self.assertTrue(data.startswith(b"GIF8"))
            self.assertEqual(hashlib.sha256(data).hexdigest(), receipt.digest)
            saved[row.name] = (receipt.destination.stat().st_mtime_ns, data)
        flag.rename(self.root / "fixed")
        resumed = self.render(resume=True)
        self.assertTrue(resumed.ok)
        for row in resumed.outcomes:
            self.assertEqual(len(row.result.segments), 3)
            first_clip = row.result.segments[0].render.destination
            self.assertEqual((first_clip.stat().st_mtime_ns, first_clip.read_bytes()), saved[row.name])
            self.assertEqual((self.root / (row.name + ".calls")).read_text().splitlines(),
                             ["construct", "construct"])

    def test_native_prefix_tampering_refuses_before_rendering_the_suffix(self):
        (self.root / "fail").touch()
        first = self.render()
        path = first.outcomes[0].result.segments[0].render.destination
        path.write_bytes(b"tampered native clip")
        (self.root / "fail").rename(self.root / "fixed")
        resumed = self.render(resume=True)
        self.assertEqual(resumed.outcomes[0].status, "failed")
        self.assertFalse((self.output / "Lesson/clips/segment-000001.gif").exists())
        self.assertEqual(path.read_bytes(), b"tampered native clip")


if __name__ == "__main__":
    unittest.main()
