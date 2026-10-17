"""Real-runtime worker admission and post-publication generation changes."""
from __future__ import annotations

import hashlib
from pathlib import Path
import tempfile
import unittest

from test_parallel_certified_native import native_available


@unittest.skipUnless(native_available(), "requires the installed native FrankenManim wheel")
class CertifiedWorkerGenerationTests(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="fmn-certified-generation-"))
        self.source = self.root / "scenes.py"
        self.source.write_text('''from manimlib import Scene, Square
class First(Scene):
    def construct(self):
        self.add(Square())
class Second(First):
    pass
''')

    def test_runtime_mismatch_refuses_before_authored_source_import(self):
        import manimlib
        from fmn_python._process_batch import run_process_jobs
        from fmn_python.parallel_batch import _render_source_job
        from fmn_python.runtime_identity import capture_runtime

        marker = self.root / "imported"
        self.source.write_text(f'from pathlib import Path\nPath({str(marker)!r}).write_text("yes")\n' + self.source.read_text())
        identities = dict(capture_runtime(manimlib).identities, wheel="wrong installed payload")
        job = {"source": str(self.source), "source_digest": hashlib.sha256(self.source.read_bytes()).hexdigest(),
               "working_directory": str(self.root), "directory": str(self.root / "output"),
               "name": "First", "runtime_identities": identities,
               "options": {"reproducible": True, "format": "png", "resolution": [64, 48],
                           "fps": 4, "threads": 1}}
        row = run_process_jobs([job], _render_source_job, workers=1, job_timeout=30)[0]
        self.assertEqual(row["status"], "failed")
        self.assertIn("runtime disagrees", row["error"]["message"])
        self.assertFalse(marker.exists())
        self.assertFalse((self.root / "output").exists())

    def test_observer_runtime_edit_stops_admission_without_retracting_verified_publication(self):
        from fmn_python import parallel_cli_options, render_source_scenes
        from fmn_python.runtime_identity import RuntimeIdentityError

        path = Path(parallel_cli_options.__file__)
        original = path.read_bytes()
        observed = []

        def observer(row):
            observed.append(row)
            # Edit a real installed runtime file after this scene's certificate
            # was verified. The next worker must not run with a new generation.
            path.write_bytes(original + b"\n# generation-change regression witness\n")

        try:
            with self.assertRaises(RuntimeIdentityError) as caught:
                render_source_scenes(self.source, self.root / "output", ["First", "Second"],
                                     workers=1, format="png", resolution=(64, 48), fps=4,
                                     reproducible=True, on_result=observer, job_timeout=30)
        finally:
            # This test's own byte edit is restored even on an assertion failure;
            # no published scene artifact is modified, removed or re-created.
            path.write_bytes(original)
        self.assertEqual(len(observed), 1)
        report = caught.exception.render_batch_result
        self.assertEqual([row.status for row in report.outcomes], ["succeeded", "not_run"])
        self.assertEqual(report.outcomes[0], observed[0])
        self.assertTrue(report.outcomes[0].result.certified)
        self.assertFalse(report.all_scenes_certified)
        self.assertFalse((self.root / "output" / "Second.png").exists())
        receipt = report.outcomes[0].result
        self.assertEqual(hashlib.sha256(receipt.destination.read_bytes()).hexdigest(), receipt.digest)
        self.assertTrue(receipt.manifest.read_bytes().startswith(b"FMNP"))


if __name__ == "__main__":
    unittest.main()
