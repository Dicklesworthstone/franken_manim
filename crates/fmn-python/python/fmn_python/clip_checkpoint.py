"""Replay a verified clip prefix without opening render generations for it.

Python code and Choreo still execute, in order, on their ordinary clock. Only
native raster/encode publication is omitted for previously completed clips.
The caller's input key authorizes reuse; this is not arbitrary program replay
certification, an object pickle, or a second scene/interpolation engine.
"""
from __future__ import annotations

from contextlib import contextmanager
import json
import os
from pathlib import Path

from .batch_checkpoint import (
    _CheckpointFile, _MAX_FILES, _constructor_values, _inventory, _json, _scene_identity,
)
from .subdivision import RenderSegment, SubdividedRecordingSession


def _integer(value):
    if type(value) is not int or not 0 <= value < 1 << 63:
        raise ValueError("clip checkpoint requires nonnegative integer clock indices")
    return value


def _clip_path(root, index, format):
    stem = f"segment-{index:06d}"
    return root / (stem if format == "png_sequence" else stem + "." + format)


def _restore_segment(row, root, options):
    from .batch_rendering import _restored_outcome

    if type(row) is not dict or set(row) != {"play_index", "kind", "start_frame", "end_frame", "render"}:
        raise ValueError("invalid checkpoint clip record")
    index, start, end = (_integer(row[name]) for name in ("play_index", "start_frame", "end_frame"))
    if row["kind"] not in {"play", "wait"} or end < start:
        raise ValueError("invalid checkpoint clip interval")
    format = options["format"]
    path = _clip_path(root, index, format)
    data = row["render"]
    if (type(data) is not dict or data.get("destination") != str(path)
            or data.get("format") != format or data.get("certified") is not False
            or any(name in data for name in ("schema", "manifest", "closure_digest", "artifact_digest"))):
        raise ValueError("checkpoint clip publication does not match its planned destination")
    receipt = _restored_outcome({"name": str(index), "result": data}).result
    for name in ("resolution", "fps", "seed"):
        if _json(getattr(receipt, name)) != _json(options[name]):
            raise ValueError("checkpoint clip does not match " + name)
    if receipt.bytes <= 0 or receipt.animation_range is not None:
        raise ValueError("invalid checkpoint clip publication")
    if format == "wav":
        if (receipt.frame_count is not None or receipt.sample_frames is None
                or type(data.get("sample_rate")) is not int or data["sample_rate"] != 48000
                or type(data.get("channels")) is not int or data["channels"] != 2):
            raise ValueError("invalid checkpoint WAV clip")
    elif (receipt.sample_frames is not None or receipt.frame_count is None
          or receipt.frame_count <= 0):
        raise ValueError("invalid checkpoint video clip")
    return RenderSegment(index, row["kind"], start, end, receipt)


def _clip_inventory(segment):
    receipt = segment.render
    files = _inventory(receipt.destination)
    if receipt.format == "png_sequence":
        # Native PNG sequences are flat directories. Do not let empty added
        # directories disappear from a files-only inventory.
        if not receipt.destination.is_dir():
            raise ValueError("checkpoint PNG clip must be a directory")
        with os.scandir(receipt.destination) as entries:
            for entry in entries:
                if not entry.is_file(follow_symlinks=False):
                    raise ValueError("checkpoint PNG clips cannot contain subdirectories or links")
        if receipt.frame_count >= _MAX_FILES:
            raise ValueError("checkpoint PNG clip exceeds its file budget")
        expected = {f"frame_{index:06d}.png" for index in range(receipt.frame_count)}
        expected.add("FMN_COMPLETE")  # Native atomic-directory marker is not a frame.
        if ({item["path"] for item in files} != expected
                or sum(item["bytes"] for item in files if item["path"] != "FMN_COMPLETE") != receipt.bytes):
            raise ValueError("checkpoint PNG clip inventory disagrees with its native receipt")
    elif (len(files) != 1 or files[0]["path"] != ""
          or files[0]["bytes"] != receipt.bytes or files[0]["sha256"] != receipt.digest):
        raise ValueError("checkpoint clip artifact disagrees with its native receipt")
    return files


class ClipCheckpoint(_CheckpointFile):
    """A prefix journal over the batch journal's existing lock and fsync owner."""
    schema = "fmn-python.clip-checkpoint"
    version = 1

    def prepare(self, destination, options, identity, maximum):
        self.destination, self.maximum = destination, maximum
        self.plan = json.loads(_json({"destination": str(destination), "options": options,
                                      "identity": _constructor_values(identity)}))
        self.segments, self.inventories, self.complete = [], [], False
        document = self.document
        if document is not None:
            if (set(document) != {"schema", "version", "key", "plan", "segments", "inventories", "completed"}
                    or type(document["completed"]) is not bool):
                raise ValueError("invalid clip checkpoint document")
            if _json(document["plan"]) != _json(self.plan):
                raise ValueError("clip checkpoint scene inputs or render options do not match")
            rows, inventories = document["segments"], document["inventories"]
            if (type(rows) is not list or type(inventories) is not list
                    or len(rows) != len(inventories) or len(rows) > maximum):
                raise ValueError("invalid clip checkpoint inventory")
            self.segments = [_restore_segment(row, destination, options) for row in rows]
            previous_index, previous_end = -1, 0
            total = 0
            for segment, files in zip(self.segments, inventories):
                if segment.play_index <= previous_index or segment.start_frame < previous_end:
                    raise ValueError("checkpoint clips do not preserve scene order")
                if type(files) is not list:
                    raise ValueError("invalid checkpoint file inventory")
                total += len(files)
                if total > _MAX_FILES:
                    raise ValueError("clip checkpoint exceeds its combined file budget")
                previous_index, previous_end = segment.play_index, segment.end_frame
            self.inventories, self.complete = inventories, document["completed"]
            self.verify()
        elif os.path.lexists(destination):
            raise FileExistsError(f"unrecorded clip destination already exists: {destination}")

    def _check_directory(self, segments):
        expected = {segment.render.destination.name for segment in segments}
        if not os.path.lexists(self.destination):
            if expected:
                raise ValueError("checkpoint clip directory is missing")
            return
        if self.destination.is_symlink() or not self.destination.is_dir():
            raise ValueError("checkpoint clip destination must be a directory, not a link")
        found = set()
        with os.scandir(self.destination) as entries:
            for entry in entries:
                if entry.name not in expected or entry.is_symlink():
                    raise ValueError("clip directory contains an unrecorded artifact: " + entry.name)
                found.add(entry.name)
        if found != expected:
            raise ValueError("checkpoint clip directory is missing an artifact")

    def verify(self):
        self._check_directory(self.segments)
        for segment, saved in zip(self.segments, self.inventories):
            if _json(_clip_inventory(segment)) != _json(saved):
                raise ValueError("checkpoint clip artifact is missing or modified")

    def record(self, segments, *, completed=False):
        if len(segments) > self.maximum or len(segments) < len(self.segments):
            raise ValueError("clip checkpoint cannot discard publications or exceed its budget")
        if any(_json(a.as_dict()) != _json(b.as_dict())
               for a, b in zip(self.segments, segments)):
            raise ValueError("clip checkpoint cannot replace its published prefix")
        inventories = list(self.inventories)
        previous_index = self.segments[-1].play_index if self.segments else -1
        previous_end = self.segments[-1].end_frame if self.segments else 0
        for segment in segments[len(self.segments):]:
            _restore_segment(segment.as_dict(), self.destination, self.plan["options"])
            if segment.play_index <= previous_index or segment.start_frame < previous_end:
                raise ValueError("new clips do not extend the checkpoint in scene order")
            inventories.append(_clip_inventory(segment))
            previous_index, previous_end = segment.play_index, segment.end_frame
        if sum(map(len, inventories)) > _MAX_FILES:
            raise ValueError("clip checkpoint exceeds its combined file budget")
        self._check_directory(segments)
        payload = _json({"schema": self.schema, "version": self.version, "key": self.key,
                         "plan": self.plan, "segments": [item.as_dict() for item in segments],
                         "inventories": inventories, "completed": completed})
        self._publish(payload)
        # Publish memory only after the atomic journal replacement succeeds.
        self.segments, self.inventories, self.complete = list(segments), inventories, completed


class _ReplayedSegment:
    def __init__(self, expected, index, kind, start):
        self.expected, self.index, self.kind, self.start = expected, index, kind, start
        self.selected = False

    def select(self, selected):
        self.selected = bool(selected)
        if not self.selected:
            return
        expected = self.expected
        if (expected is None or (self.index, self.kind, self.start)
                != (expected.play_index, expected.kind, expected.start_frame)):
            raise ValueError("replayed clip selection, kind or start frame differs from checkpoint")

    def abort(self):
        # No native output generation was opened for a replayed segment.
        pass


class ResumableSubdivisionSession(SubdividedRecordingSession):
    """Rebuild scene state on its real clock and render only the missing suffix."""
    def __init__(self, scene, destination, *, checkpoint, resume, resume_key,
                 input_identity=None, **options):
        if not isinstance(resume, bool):
            raise TypeError("resume must be bool")
        super().__init__(scene, destination, **options)
        # Normalize ancestors, but keep a symlink leaf visible for refusal.
        self.destination = self.destination.parent.resolve() / self.destination.name
        self._journal = ClipCheckpoint(checkpoint, resume=resume, key=resume_key)
        self._journal.validate_destinations([self.destination])
        self._identity = {"scene": _scene_identity(scene), "cwd": str(Path.cwd()),
                          "inputs": _constructor_values(input_identity)}
        self._saved = ()
        self._replayed = 0
        self._was_complete = False
        self._replaying_segment = False
        self._prefix_verified = False

    @property
    def reused(self):
        """Original play indices whose saved clips this run has replayed."""
        return tuple(segment.play_index for segment in self._saved[:self._replayed])

    def _preflight_directory(self):
        # prepare() validated both the whole plan and every retained artifact.
        self._journal._check_directory(self._saved)

    def _claim_directory(self):
        if not os.path.lexists(self.destination):
            super()._claim_directory()
        else:
            self._journal._check_directory(self._saved)

    def __enter__(self):
        self._check_owner()
        if self._state != "new":
            raise RuntimeError("a resumable subdivision can be entered only once")
        self._journal.__enter__()
        try:
            config = self._configuration
            writer = self.scene.file_writer
            names = ("vcodec", "pix_fmt", "ffmpeg_bin", "transparent", "video_crf",
                     "video_preset", "video_tune", "video_bitrate", "audio_bitrate",
                     "ffmpeg_timeout", "ffmpeg_max_log_bytes", "ffmpeg_max_artifact_bytes",
                     "ffmpeg_max_input_bytes")
            writer_options = {name: getattr(writer, name, None) for name in names}
            if isinstance(writer_options["ffmpeg_bin"], os.PathLike):
                writer_options["ffmpeg_bin"] = os.fspath(writer_options["ffmpeg_bin"])
            options = dict(format=config.format, resolution=config.resolution, fps=config.fps,
                           threads=config.threads, seed=config.seed, animation_range=self._selection,
                           max_segments=self.max_segments, writer=writer_options)
            self._journal.prepare(self.destination, options, self._identity, self.max_segments)
            self._saved = tuple(self._journal.segments)
            self._was_complete = self._journal.complete
            # Journal an empty namespace before creating it: a crash between
            # these steps is resumable, but an unrelated preexisting one is not.
            if self._journal.document is None:
                self._journal.record(())
            super().__enter__()
            self._segments = list(self._saved)
            return self
        except BaseException:
            self._journal.__exit__(None, None, None)
            raise

    @contextmanager
    def _segment(self, kind):
        self._check_owner()
        if self._state != "active" or self._current is not None:
            raise RuntimeError("resumable subdivision is inactive or already executing")
        try:
            if self._replayed < len(self._saved) or self._was_complete:
                fps, start = self._native._portal_scene_clock(self.scene)
                if fps != self._configuration.fps or start < self._last_frame:
                    raise RuntimeError("the replay clock was reset")
                index = _integer(self.scene.num_plays)
                expected = self._saved[self._replayed] if self._replayed < len(self._saved) else None
                segment = _ReplayedSegment(expected, index, kind, start)
                self._current, self._replaying_segment = segment, True
                try:
                    yield segment
                    fps, end = self._native._portal_scene_clock(self.scene)
                    if fps != self._configuration.fps or end < start:
                        raise ValueError("replayed clip clock differs from checkpoint")
                    if segment.selected:
                        if end != expected.end_frame:
                            raise ValueError("replayed clip end frame differs from checkpoint")
                        self._replayed += 1
                    self._last_frame = end
                finally:
                    self._current, self._replaying_segment = None, False
                return
            # Before opening a new generation, ensure replay did not alter a
            # retained output through authored filesystem effects.
            if not self._prefix_verified:
                self._journal.verify()
                self._prefix_verified = True
            with super()._segment(kind) as segment:
                yield segment
            self._journal.record(self.segments)
        except BaseException as error:
            # EndScene is the existing successful range boundary, not a failed
            # clip. finish() still checks that all saved clips were replayed.
            if not isinstance(error, self._native.EndScene):
                self._abort_preserving(error)
            raise

    def finish(self):
        self._check_owner()
        if self._state == "finished":
            return self.result
        if self._replayed != len(self._saved):
            raise ValueError("scene ended before replaying all checkpoint clips")
        self._journal.verify()
        result = super().finish()
        try:
            self._journal.record(self.segments, completed=True)
        except BaseException:
            self._state = "failed"
            raise
        finally:
            self._journal.__exit__(None, None, None)
        return result

    def abort(self):
        try:
            super().abort()
        finally:
            self._journal.__exit__(None, None, None)
