"""Public clip-assembly argument and receipt contracts without an encoder.

The native double never reads media or publishes a movie. The native acceptance
suite separately exercises the actual Rust boundary and child executable.
"""
import copy
import itertools
import ntpath
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from fmn_python import VideoClipResult, VideoConcatResult, concatenate_video
from fmn_python.video_composition import _path


class CompositionProtocol(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.calls = []
        self.receipt = None
        self.error = None

        def assemble(inputs, destination, executable, policy, workdir):
            self.calls.append((inputs, destination, executable, dict(policy), workdir))
            if self.error is not None:
                raise self.error
            self.receipt = {
                "schema": "fmn.video_concat.v1", "destination": destination,
                "inputs": [{"source": path, "bytes": index + 1,
                            "digest": str(index) * 64} for index, path in enumerate(inputs)],
                "total_input_bytes": sum(range(1, len(inputs) + 1)),
                "bytes": 91, "digest": "a" * 64, "certified": False,
                "ffmpeg_invocations": [{"argv": ["-map", "0", "-c", "copy"]}],
                "ffmpeg_limits": {"max_input_bytes": policy["max_input_bytes"],
                                  "timeout_seconds": policy["timeout"]},
                "source_limits": {key: policy[key] for key in
                                  ("max_clips", "max_clip_bytes", "max_input_bytes")},
            }
            return self.receipt

        self.native = types.SimpleNamespace(_portal_concatenate_video=assemble)
        importing = patch.dict(sys.modules, {"manimlib": self.native})
        importing.start()
        self.addCleanup(importing.stop)

    def compose(self, inputs=None, destination=None, **options):
        return concatenate_video(
            [self.root / "A.mp4"] if inputs is None else inputs,
            self.root / "joined.mp4" if destination is None else destination,
            **options,
        )

    def test_order_repetition_and_native_receipt_are_preserved(self):
        paths = [self.root / "A.mp4", self.root / "it's\nB.mov", self.root / "A.mp4"]
        result = self.compose(iter(paths), max_clips=3, max_clip_bytes=11, max_input_bytes=30,
                              timeout=7, max_log_bytes=100, max_output_bytes=250)
        self.assertIsInstance(result, VideoConcatResult)
        self.assertTrue(all(isinstance(entry, VideoClipResult) for entry in result.inputs))
        self.assertEqual([entry.source for entry in result.inputs], paths)
        self.assertEqual(self.calls[0][0], tuple(str(path) for path in paths))
        self.assertEqual(self.calls[0][3], {
            "max_clips": 3, "max_clip_bytes": 11, "max_input_bytes": 30,
            "timeout": 7, "max_log_bytes": 100, "max_output_bytes": 250,
        })
        self.assertEqual(result.total_input_bytes, 6)
        self.assertEqual((result.bytes, result.digest), (91, "a" * 64))
        self.assertFalse(result.certified)
        self.assertNotIn("frame_count", result.as_dict())
        self.assertNotIn("duration", result.as_dict())
        self.assertFalse(result.destination.exists(), "this double does not publish movies")

    def test_generator_admission_is_bounded_without_consuming_its_tail(self):
        consumed = []
        def inputs():
            for index in itertools.count():
                consumed.append(index)
                yield self.root / f"{index}.mp4"
        with self.assertRaisesRegex(ValueError, "max_clips"):
            self.compose(inputs(), max_clips=3)
        self.assertEqual(consumed, [0, 1, 2, 3])
        self.assertEqual(self.calls, [])

    def test_empty_or_single_path_is_not_a_clip_sequence(self):
        with self.assertRaises(ValueError):
            self.compose([])
        for value in ("one.mp4", b"one.mp4", Path("one.mp4")):
            with self.subTest(value=value), self.assertRaises(TypeError):
                self.compose(value)
        self.assertEqual(self.calls, [])

    def test_policy_refusals_precede_iterator_work(self):
        def unvisited():
            raise AssertionError("invalid policy must not inspect inputs")
            yield
        for name in ("max_clips", "max_clip_bytes", "max_input_bytes", "timeout",
                     "max_log_bytes", "max_output_bytes"):
            for value in (0, -1, True, 1.25, 1 << 64):
                with self.subTest(name=name, value=value), self.assertRaises((TypeError, ValueError)):
                    self.compose(unvisited(), **{name: value})
        with self.assertRaises(ValueError):
            self.compose(unvisited(), max_clips=4094)
        self.assertEqual(self.calls, [])

    def test_invalid_paths_or_container_do_not_call_native(self):
        for value in (b"clip.mp4", "", "clip\0.mp4"):
            with self.subTest(value=value), self.assertRaises((TypeError, ValueError)):
                self.compose([value])
            with self.subTest(destination=value), self.assertRaises((TypeError, ValueError)):
                self.compose(destination=value)
            with self.subTest(executable=value), self.assertRaises((TypeError, ValueError)):
                self.compose(ffmpeg_bin=value)
        for value in ("movie.gif", "movie", "movie.y4m"):
            with self.assertRaisesRegex(ValueError, "MP4 or MOV"):
                self.compose(destination=value)
        self.assertEqual(self.calls, [])

    def test_relative_paths_use_entry_cwd_even_across_authored_callbacks(self):
        entry, other = self.root / "entry", self.root / "other"
        entry.mkdir()
        other.mkdir()
        previous = os.getcwd()
        self.addCleanup(os.chdir, previous)
        os.chdir(entry)

        class Destination:
            def __fspath__(self):
                os.chdir(other)
                return "joined.MOV"

        def inputs():
            yield "A.mp4"
            os.chdir(previous)
            yield "B.mp4"

        result = self.compose(inputs(), Destination(), ffmpeg_bin="tools/ffmpeg", workdir="jobs")
        paths, output, executable, _, workdir = self.calls[0]
        self.assertEqual(paths, (str(entry / "A.mp4"), str(entry / "B.mp4")))
        self.assertEqual(output, str(entry / "joined.MOV"))
        self.assertEqual(executable, str(entry / "tools/ffmpeg"))
        self.assertEqual(workdir, str(entry / "jobs"))
        self.assertEqual(result.destination, entry / "joined.MOV")

    def test_parent_components_follow_symlinks_for_every_native_path(self):
        entry, actual = self.root / "entry", self.root / "actual"
        entry.mkdir()
        (actual / "sub").mkdir(parents=True)
        try:
            (entry / "link").symlink_to(actual / "sub", target_is_directory=True)
        except (OSError, NotImplementedError) as error:
            self.skipTest(f"directory symlinks unavailable: {error}")
        (entry / "clip.mp4").write_bytes(b"wrong local clip")
        (actual / "clip.mp4").write_bytes(b"authored clip")
        (entry / "ffmpeg").write_bytes(b"wrong local executable")
        (actual / "ffmpeg").write_bytes(b"authored executable")
        (entry / "jobs").mkdir()
        (actual / "jobs").mkdir()
        previous = os.getcwd()
        self.addCleanup(os.chdir, previous)
        os.chdir(entry)

        result = self.compose(["link/../clip.mp4"], "link/../joined.mp4",
                              ffmpeg_bin="link/../ffmpeg", workdir="link/../jobs")
        paths, output, executable, _, workdir = self.calls[0]
        self.assertEqual(Path(paths[0]).read_bytes(), b"authored clip")
        self.assertTrue(Path(output).parent.samefile(actual))
        self.assertEqual(Path(executable).read_bytes(), b"authored executable")
        self.assertTrue(Path(workdir).samefile(actual / "jobs"))
        self.assertEqual(result.destination, entry / "link" / ".." / "joined.mp4")

    def test_windows_drive_relative_paths_are_refused_without_normalizing_parents(self):
        # Exercise the standard Windows path rules on every test platform.
        # Only this pure helper runs while os.path is replaced.
        with patch.object(os, "path", ntpath):
            for value in ("C:clip.mp4", "D:clip.mp4", "C:"):
                with self.subTest(value=value), self.assertRaisesRegex(ValueError, "drive-relative"):
                    _path(value, "input", "C:\\entry")
            for value, expected in (
                (r"link\..\clip.mp4", r"C:\entry\link\..\clip.mp4"),
                (r"\clips\clip.mp4", r"C:\clips\clip.mp4"),
                (r"D:\link\..\clip.mp4", r"D:\link\..\clip.mp4"),
                (r"\\server\share\link\..\clip.mp4", r"\\server\share\link\..\clip.mp4"),
            ):
                with self.subTest(value=value):
                    self.assertEqual(_path(value, "input", "C:\\entry"), expected)

    def test_native_refusal_is_propagated_without_a_second_attempt(self):
        self.error = FileExistsError("another writer won")
        with self.assertRaises(FileExistsError) as caught:
            self.compose()
        self.assertIs(caught.exception, self.error)
        self.assertEqual(len(self.calls), 1)

    def test_missing_native_surface_names_the_matching_wheel(self):
        del self.native._portal_concatenate_video
        with self.assertRaisesRegex(RuntimeError, "matching native.*wheel"):
            self.compose()

    def test_receipts_do_not_share_mutable_metadata_with_native_or_serializations(self):
        result = self.compose()
        expected = copy.deepcopy(result.as_dict())
        self.receipt["ffmpeg_invocations"][0]["argv"].clear()
        self.receipt["source_limits"].clear()
        self.receipt["ffmpeg_limits"].clear()
        exported = result.as_dict()
        exported["ffmpeg_invocations"][0]["argv"].clear()
        exported["ffmpeg_limits"].clear()
        exported["source_limits"].clear()
        self.assertEqual(result.as_dict(), expected)


if __name__ == "__main__":
    unittest.main()
