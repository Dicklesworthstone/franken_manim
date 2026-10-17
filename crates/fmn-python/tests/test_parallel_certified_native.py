"""Real native certified outputs in fresh workers; no rendering doubles.

Run with the installed wheel whose Python payload is this checkout. A reduced
source-only package is not enough: runtime provenance requires real installed
payload inventories. These are same-host invariance checks, not matrix proof.
"""
from __future__ import annotations

import hashlib
import importlib.util
from importlib.machinery import ExtensionFileLoader, PathFinder
import json
import multiprocessing as mp
import os
from pathlib import Path
import tempfile
import tomllib
import unittest
import wave


def native_available():
    spec = importlib.util.find_spec("manimlib")
    if spec is None or spec.submodule_search_locations is None:
        return False
    ext = PathFinder.find_spec("manimlib.manimlib", spec.submodule_search_locations)
    return ext is not None and isinstance(ext.loader, ExtensionFileLoader)


SCENES = '''from pathlib import Path
from manimlib import Scene, Square, Dot, RIGHT, LEFT, linear
class Alpha(Scene):
    def construct(self):
        self.add(Square().shift(LEFT))
class Beta(Scene):
    def construct(self):
        self.add(Dot().shift(RIGHT))
class Moving(Scene):
    def construct(self):
        mob=Square().shift(LEFT)
        self.add(mob)
        self.play(mob.animate.shift(2 * RIGHT), run_time=0.5, rate_func=linear)
class Audio(Scene):
    def construct(self):
        self.add_sound(str(Path(__file__).with_name("tone.wav")))
        self.wait(0.25)
class Broken(Scene):
    def construct(self):
        raise RuntimeError("authored failure")
'''


@unittest.skipUnless(native_available(), "requires the built native FrankenManim wheel")
class CertifiedParallelNativeTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-parallel-certified-"))
        self.source = self.root / "scenes.py"
        self.source.write_text(SCENES, encoding="utf-8")
        self.counter = 0
        from fmn_python import render_source_scenes
        self.render = render_source_scenes

    def run_scenes(self, names=("Alpha", "Beta"), **options):
        self.counter += 1
        settings = dict(workers=2, format="png", resolution=(64, 48), fps=4,
                        threads=1, reproducible=True, job_timeout=30)
        settings.update(options)
        return self.render(self.source, self.root / f"output-{self.counter}", names, **settings)

    def manifest(self, receipt):
        self.assertTrue(receipt.certified)
        self.assertEqual(receipt.manifest, Path(str(receipt.destination) + ".manifest") / "manifest.fmnp")
        self.assertTrue(receipt.manifest.read_bytes().startswith(b"FMNP"))
        text = tomllib.loads(receipt.manifest.with_name("manifest.txt").read_text())
        self.assertEqual(text["mode"], "certified")
        self.assertEqual(text["closure_digest"], receipt.closure_digest)
        self.assertTrue(any(item.get("virtual_path") == "__fmn_parallel__/invocation.json"
                            for item in text["items"]))
        return text

    def check(self, report):
        self.assertTrue(report.ok)
        self.assertTrue(report.all_scenes_certified)
        data = json.loads(json.dumps(report.as_dict()))
        self.assertTrue(data["reproducible_requested"])
        self.assertTrue(data["all_scenes_certified"])
        self.assertFalse(data["certified"], "the batch is not a new certified artifact")
        self.assertEqual(data["execution"], "spawn")
        for row in report.outcomes:
            self.manifest(row.result)
            if row.result.format != "png_sequence":
                self.assertEqual(hashlib.sha256(row.destination.read_bytes()).hexdigest(), row.result.digest)
        return data

    def test_stills_are_identical_across_worker_counts_and_bind_selected_class(self):
        reports = [self.run_scenes(workers=count) for count in (1, 2)]
        for report in reports:
            self.check(report)
            self.assertNotEqual(report.outcomes[0].result.closure_digest,
                                report.outcomes[1].result.closure_digest)
        for a, b in zip(reports[0].outcomes, reports[1].outcomes):
            self.assertEqual(a.destination.read_bytes(), b.destination.read_bytes())
            self.assertEqual(a.result.closure_digest, b.result.closure_digest)
            self.assertEqual(a.result.manifest.read_bytes(), b.result.manifest.read_bytes())

    def test_moving_frames_are_bit_identical_at_one_four_and_sixteen_threads(self):
        frames = []
        for threads in (1, 4, 16):
            report = self.run_scenes(("Moving", "Beta"), format="png_sequence", threads=threads)
            self.check(report)
            outputs = [[p.read_bytes() for p in sorted(row.destination.glob("*.png"))]
                       for row in report.outcomes]
            self.assertEqual(len(outputs[0]), 2)
            self.assertEqual(len(set(outputs[0])), 2, "real motion must change the emitted frames")
            frames.append(outputs)
        self.assertEqual(frames[0], frames[1])
        self.assertEqual(frames[0], frames[2])

    def test_native_wav_is_identical_and_captures_cue_bytes(self):
        cue = self.root / "tone.wav"
        with wave.open(str(cue), "wb") as stream:
            stream.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
            stream.writeframes((b"\x10\x00\xf0\xff") * 6000)
        reports = [self.run_scenes(("Audio",), format="wav", workers=n) for n in (1, 2)]
        for report in reports:
            self.check(report)
            row = report.outcomes[0]
            with wave.open(str(row.destination), "rb") as stream:
                self.assertEqual((stream.getnchannels(), stream.getframerate(), stream.getnframes()),
                                 (2, 48000, 12000))
            text = row.result.manifest.with_name("manifest.txt").read_text()
            self.assertIn(hashlib.sha256(cue.read_bytes()).hexdigest(), text)
        self.assertEqual(reports[0].outcomes[0].destination.read_bytes(),
                         reports[1].outcomes[0].destination.read_bytes())

    def test_import_time_asset_and_lazy_helper_change_closure(self):
        asset = self.root / "offset.txt"
        asset.write_text("1.0")
        helper = self.root / "helper.py"
        helper.write_text("VALUE = 0.25\n")
        self.source.write_text('''from pathlib import Path
from manimlib import Scene, Dot, RIGHT
offset = float(Path(__file__).with_name("offset.txt").read_text())
class Read(Scene):
    def construct(self):
        import helper
        self.add(Dot().shift((offset + helper.VALUE) * RIGHT))
''')
        receipts = []
        for value in ("1.0", "2.0"):
            asset.write_text(value)
            report = self.run_scenes(("Read",))
            self.check(report)
            receipt = report.outcomes[0].result
            text = receipt.manifest.with_name("manifest.txt").read_text()
            for data in (asset.read_bytes(), helper.read_bytes()):
                self.assertIn(hashlib.sha256(data).hexdigest(), text)
            self.assertIn('virtual_path = "helper.py"', text)
            receipts.append(receipt)
        self.assertNotEqual(receipts[0].closure_digest, receipts[1].closure_digest)
        self.assertNotEqual(receipts[0].digest, receipts[1].digest)

    def test_uncapturable_import_effect_is_refused_before_publication(self):
        self.source.write_text('''from pathlib import Path
from manimlib import Scene
list(Path(__file__).parent.iterdir())
class Read(Scene):
    def __init__(self):
        Path(__file__).with_name("constructed").write_text("yes")
        super().__init__()
''')
        report = self.run_scenes(("Read",), continue_on_error=True)
        self.assertFalse(report.ok)
        self.assertFalse(report.all_scenes_certified)
        self.assertEqual(report.outcomes[0].status, "failed")
        self.assertIn("directory listing", report.outcomes[0].message)
        self.assertFalse(report.outcomes[0].destination.exists())
        # The ordinary guard rejects before native output starts. Source and
        # constructor effects are not sandboxed or rolled back by certification.

    def test_asset_changed_after_read_refuses_publication(self):
        asset = self.root / "mutable.txt"
        asset.write_text("old")
        self.source.write_text('''from pathlib import Path
from manimlib import Scene, Dot
class Read(Scene):
    def construct(self):
        path = Path(__file__).with_name("mutable.txt")
        path.read_text()
        path.write_text("new")
        self.add(Dot())
''')
        report = self.run_scenes(("Read",), continue_on_error=True)
        self.assertEqual(report.outcomes[0].status, "failed")
        self.assertFalse(report.all_scenes_certified)
        self.assertFalse(report.outcomes[0].destination.exists())
        self.assertEqual(asset.read_text(), "new", "authored effects are not rolled back")

    def test_failure_preserves_other_certified_outputs_but_not_all_certified(self):
        report = self.run_scenes(("Alpha", "Broken", "Beta"), continue_on_error=True)
        self.assertEqual([r.status for r in report.outcomes], ["succeeded", "failed", "succeeded"])
        self.assertFalse(report.all_scenes_certified)
        self.assertTrue(report.reproducible)
        for row in (report.outcomes[0], report.outcomes[2]):
            self.manifest(row.result)

    def test_sidecar_collision_prevents_any_worker_source_execution(self):
        marker = self.root / "imported"
        self.source.write_text(f'from pathlib import Path\nPath({str(marker)!r}).write_text("yes")\n' + SCENES)
        destination = self.root / "output-1"
        destination.mkdir()
        sidecar = destination / "Beta.png.manifest"
        sidecar.write_bytes(b"preserve")
        with self.assertRaises(FileExistsError):
            self.run_scenes()
        self.assertFalse(marker.exists())
        self.assertFalse((destination / "Alpha.png").exists())
        self.assertEqual(sidecar.read_bytes(), b"preserve")

    def test_observer_error_retains_verified_partial_receipts_and_reaps_workers(self):
        before = {child.pid for child in mp.active_children()}
        def observer(row):
            raise LookupError("observer stopped")
        with self.assertRaises(LookupError) as caught:
            self.run_scenes(on_result=observer)
        report = caught.exception.render_batch_result
        self.assertTrue(report.reproducible)
        successes = [r for r in report.outcomes if r.status == "succeeded"]
        self.assertTrue(successes)
        for row in successes:
            self.manifest(row.result)
        self.assertEqual({child.pid for child in mp.active_children()}, before)


if __name__ == "__main__":
    unittest.main()
