"""Compact capture admission and recovery over the real batch/journal owners.

The native boundary fixture publishes marker bytes, not FMTL. Native codec,
pixel and PCM acceptance remains in the embedded native export suites.
"""
from copy import deepcopy
import json
import unittest

import test_bundle_batch_protocol as fixtures
from fmn_python import render_scenes
from fmn_python.bundle_export import _COMPACT_DEFAULTS


class CompactBundleBatchProtocol(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.BundleBatchProtocol()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.root = self.fixture.root
        self.directory = self.root / "compact"
        self.journal = self.root / "compact.json"
        self.jobs = {"First": fixtures.existing.First, "Second": fixtures.existing.Second}
        self.requests = []
        self.silent = False

        def begin(scene, path, width, height, fps, seed, max_frames, max_output_bytes,
                  camera, audio, retained, snapshot, work, rate, channels, workers, audio_bytes,
                  audio_ffmpeg):
            self.assertIs(audio_ffmpeg, False)
            self.requests.append((type(scene).__name__, max_frames, max_output_bytes,
                                  camera, audio, retained, snapshot, work))
            self.fixture.begin(camera, scene, path, width, height, fps, seed,
                               max_frames, retained, max_output_bytes)
            scene.compact_audio = (rate, channels, 1)

        self.fixture.native._portal_begin_compact_bundle = begin
        self.fixture.native._portal_finish_soundtrack_bundle = lambda scene: (
            self.fixture.finish(scene), None if self.silent else scene.compact_audio)

    def render(self, **changes):
        options = dict(format="fmtl", bundle_storage="compact", bundle_camera=True,
                       resolution=(96, 54), fps=8)
        options.update(changes)
        return render_scenes(self.jobs, self.directory, **options)

    def recovery(self, **changes):
        options = dict(checkpoint=self.journal, resume_key="compact-authored-v1")
        options.update(changes)
        return self.render(**options)

    def clear_execution(self):
        self.requests.clear()
        self.fixture.starts.clear()
        fixtures.existing.Scene.instances.clear()

    def assert_no_execution(self):
        self.assertEqual(self.requests, [])
        self.assertEqual(self.fixture.starts, [])
        self.assertEqual(fixtures.existing.Scene.instances, [])

    def test_limits_are_frozen_before_job_iteration_and_observer_callbacks(self):
        limits = {"max_frames": 10, "max_output_bytes": 4096}
        compact = {"max_retained_bytes": 123456, "max_snapshot_bytes": 3456,
                   "max_snapshot_work_bytes": 9_000_000_000}
        def jobs():
            limits["max_frames"] = 1
            compact["max_retained_bytes"] = 1
            yield fixtures.existing.First
            yield fixtures.existing.Second
        self.jobs = jobs()
        def observer(_):
            compact["max_snapshot_work_bytes"] = 1
        result = self.render(bundle_limits=limits, bundle_compact_limits=compact, on_result=observer)
        self.assertTrue(result.ok)
        self.assertEqual([row[1:] for row in self.requests],
                         [(10, 4096, True, False, 123456, 3456, 9_000_000_000)] * 2)
        self.assertTrue(all(row.result.storage == "compact" for row in result.outcomes))

    def test_same_normalized_plan_resumes_picture_camera_and_soundtrack_receipts(self):
        for camera, audio, silent in ((False, False, False), (True, False, False),
                                     (False, True, False), (True, True, False),
                                     (True, True, True)):
            with self.subTest(camera=camera, audio=audio, silent=silent):
                self.directory = self.root / f"out-{camera}-{audio}-{silent}"
                self.journal = self.root / f"journal-{camera}-{audio}-{silent}.json"
                self.silent = silent
                common = dict(bundle_camera=camera, bundle_audio=audio)
                first = self.recovery(**common)
                self.clear_execution()
                second = self.recovery(**common, resume=True,
                    bundle_compact_limits=dict(reversed(list(_COMPACT_DEFAULTS.items()))),
                    bundle_limits={"max_frames": 1_000_000, "max_capture_bytes": 256 * 1024 * 1024,
                                   "max_output_bytes": 256 * 1024 * 1024})
                self.assertEqual(first.as_dict(), second.as_dict())
                data = second.outcomes[0].result.as_dict()
                self.assertEqual(data["storage"], "compact")
                self.assertEqual(data["fmtl_minor"], 2 if audio and not silent else 3)
                self.assertEqual("picture_fmtl_minor" in data, audio and not silent)
                if audio and not silent:
                    self.assertEqual(data["picture_fmtl_minor"], 3)
                self.assert_no_execution()

    def test_storage_and_every_compact_budget_bind_resume_before_callbacks(self):
        self.recovery()
        original = self.journal.read_bytes()
        self.clear_execution()
        changes = [{"bundle_storage": "recorded"},
                   {"bundle_limits": {"max_frames": 10}},
                   {"bundle_limits": {"max_output_bytes": 4096}}]
        changes.extend({"bundle_compact_limits": {key: value // 2}}
                       for key, value in _COMPACT_DEFAULTS.items())
        for change in changes:
            with self.subTest(change=change), self.assertRaisesRegex(ValueError, "plan|options"):
                self.recovery(resume=True, on_result=lambda _: self.fail("observer ran"), **change)
            self.assert_no_execution()
            self.assertEqual(self.journal.read_bytes(), original)

    def test_malformed_or_downgraded_compact_receipts_cannot_be_reused(self):
        for audio in (False, True):
            self.directory = self.root / f"mode-{audio}"
            self.journal = self.root / f"mode-{audio}.json"
            self.recovery(bundle_audio=audio)
            original = json.loads(self.journal.read_text())
            self.clear_execution()
            mutations = [lambda row: row.update(storage="recorded"),
                         lambda row: row.update(storage=True),
                         lambda row: row.update(fmtl_minor=True),
                         lambda row: row.pop("fmtl_minor"),
                         lambda row: row.update(unexpected=True)]
            if audio:
                mutations += [lambda row: row.pop("picture_fmtl_minor"),
                              lambda row: row.update(picture_fmtl_minor=1),
                              lambda row: row.update(picture_fmtl_minor=True)]
            else:
                mutations += [lambda row: row.update(fmtl_minor=1),
                              lambda row: row.update(picture_fmtl_minor=3)]
            def downgrade(row):
                row.pop("storage")
                row.pop("picture_fmtl_minor", None)
                row["fmtl_minor"] = 2 if audio else 1
            mutations.append(downgrade)
            for mutate in mutations:
                document = deepcopy(original)
                mutate(document["outcomes"][0]["result"])
                self.journal.write_text(json.dumps(document))
                before = self.journal.read_bytes()
                with self.assertRaises(ValueError):
                    self.recovery(bundle_audio=audio, resume=True)
                self.assertEqual(self.journal.read_bytes(), before)
                self.assert_no_execution()

    def test_invalid_compact_admission_precedes_job_callbacks_and_journal(self):
        def jobs():
            self.fail("job iterator ran before admission")
            yield fixtures.existing.First
        invalid = ({"bundle_storage": "other"},
                   {"bundle_storage": "recorded", "bundle_compact_limits": {}},
                   {"bundle_compact_limits": []},
                   {"bundle_compact_limits": {"max_retained_bytes": 0}},
                   {"bundle_compact_limits": {"max_snapshot_bytes": 67108865}},
                   {"bundle_compact_limits": {"max_snapshot_work_bytes": 17179869185}},
                   {"bundle_limits": {"max_capture_bytes": 10}})
        for change in invalid:
            self.jobs = jobs()
            with self.subTest(change=change), self.assertRaises((ValueError, TypeError)):
                self.recovery(**change)
            self.assertFalse(self.journal.exists())
        self.assert_no_execution()

    def test_recorded_plan_and_receipt_do_not_gain_storage_fields(self):
        first = self.recovery(bundle_storage="recorded")
        original = self.journal.read_bytes()
        plan = json.loads(original)["plan"]["options"]
        self.assertNotIn("bundle_storage", plan)
        self.assertNotIn("bundle_compact_limits", plan)
        self.assertNotIn("storage", first.outcomes[0].result.as_dict())
        self.clear_execution()
        self.assertEqual(first.as_dict(), self.recovery(bundle_storage="recorded", resume=True).as_dict())
        self.assertEqual(self.journal.read_bytes(), original)
        self.assert_no_execution()


if __name__ == "__main__":
    unittest.main()
