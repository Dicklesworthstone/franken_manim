"""Per-scene prefix journals inside the existing spawned batch supervisor.

The parent owns a locked invocation plan, not a second render journal. Each
worker delegates clip verification, replay and publication to the existing
ResumableSubdivisionSession. Only ordinary JSON jobs and receipts cross the
process boundary; no scene state, lock or live native object is transferred.
"""
from __future__ import annotations

import json
import os
from pathlib import Path

from .batch_checkpoint import _CheckpointFile, _MAX_FILES, _json, _read_document


def validate_clip_recovery(*, checkpoint, subdivide, save_last_frame, reproducible, format):
    if not subdivide or format not in {"gif", "y4m", "png_sequence", "wav", "mp4", "mov"}:
        raise ValueError("clip_checkpoint_dir requires subdivide=True with an animated clip format")
    if reproducible:
        raise RuntimeError("CAPABILITY: clip-prefix replay is standard-only, not certified execution")


def _temporary(name, stems):
    # Atomic journal writers may be killed before replacing their destination.
    # Keep their inert temporary files; they never authorize output adoption.
    stem, separator, tail = name.rpartition(".json.")
    return bool(separator and stem + ".json" in stems and tail.endswith(".tmp")
                and tail[:-4] and all(char in "abcdefghijklmnopqrstuvwxyz0123456789_" for char in tail[:-4]))


class ParallelClipPlan(_CheckpointFile):
    """One parent lock binds source, variants and options across partial retries.

    A missing child journal denotes an unstarted scene ONLY when its output
    destination is absent. A present journal is never permission to adopt an
    unrecorded clip; the worker's native-session owner verifies the full prefix
    before replay and again before opening any new render generation.
    """
    schema = "fmn-python.parallel-clip-plan"
    version = 1

    def __init__(self, directory, *, resume, key):
        text = os.fspath(directory)
        if not isinstance(text, str) or not text or "\0" in text:
            raise ValueError("clip_checkpoint_dir must be a nonempty text path without NUL")
        original = Path(text).absolute()
        self.directory = original.parent.resolve() / original.name
        self.jobs_directory = self.directory / "jobs"
        self._check_directories()
        super().__init__(self.directory / "plan.json", resume=resume, key=key)
        self.existing_destinations = set()
        self.recovery = {}

    def _check_directories(self):
        for path in (self.directory, self.jobs_directory):
            if path.is_symlink() or (os.path.lexists(path) and not path.is_dir()):
                raise ValueError("clip checkpoint directories must be real directories, not links")

    def validate_destinations(self, destinations):
        for destination in destinations:
            if (destination == self.directory or destination in self.directory.parents
                    or self.directory in destination.parents):
                raise ValueError("clip_checkpoint_dir must be outside every published clip collection")
        super().validate_destinations(destinations)

    def __enter__(self):
        self._check_directories()
        return super().__enter__()

    def prepare(self, jobs, destinations, *, completion_checkpoint=None):
        self._check_directories()
        self.plan = json.loads(_json({"jobs": jobs, "destinations": [str(path) for path in destinations]}))
        if completion_checkpoint is not None:
            # Both owners bind the other journal's location. Switching modes or
            # borrowing a completion journal from another run must not make
            # authored side effects disappear (or unexpectedly execute again).
            self.plan["completion_checkpoint"] = str(completion_checkpoint)
        if self.document is not None:
            if (set(self.document) != {"schema", "version", "key", "plan"}
                    or _json(self.document["plan"]) != _json(self.plan)):
                raise ValueError("clip batch scene plan, source inputs or render options do not match")
        expected = {"plan.json", "plan.json.lock", "jobs"}
        with os.scandir(self.directory) as entries:
            for index, entry in enumerate(entries):
                if index >= _MAX_FILES:
                    raise ValueError("clip checkpoint directory exceeds its file budget")
                if entry.name not in expected and not (
                        _temporary(entry.name, {"plan.json"}) and entry.is_file(follow_symlinks=False)):
                    raise ValueError("clip checkpoint directory contains unrecorded state")
        stems = {job["name"] + ".json" for job in jobs}
        stems.update(job["name"] + ".still.json" for job in jobs
                     if job["options"].get("save_last_frame"))
        allowed = stems | {name + ".lock" for name in stems}
        if self.jobs_directory.exists():
            with os.scandir(self.jobs_directory) as entries:
                for index, entry in enumerate(entries):
                    if index >= _MAX_FILES:
                        raise ValueError("clip checkpoint jobs directory exceeds its file budget")
                    if (entry.name not in allowed and not _temporary(entry.name, stems)
                            or not entry.is_file(follow_symlinks=False)):
                        raise ValueError("clip checkpoint jobs directory contains unexpected files or links")
        for job, destination in zip(jobs, destinations):
            path = self.jobs_directory / (job["name"] + ".json")
            saved = os.path.lexists(path)
            if saved:
                if not self.resume:
                    raise FileExistsError("clip journal already exists; use resume=True: " + str(path))
                document = _read_document(path)
                if (type(document) is not dict or document.get("schema") != "fmn-python.clip-checkpoint"
                        or type(document.get("version")) is not int or document["version"] != 1
                        or document.get("key") != self.key
                        or type(document.get("plan")) is not dict
                        or document["plan"].get("destination") != str(destination)):
                    raise ValueError("clip journal does not identify its planned scene destination and input key")
                self.existing_destinations.add(destination)
            elif os.path.lexists(destination):
                raise FileExistsError("unrecorded clip destination already exists: " + str(destination))
            recovery = {"checkpoint": str(path), "resume": saved, "resume_key": self.key}
            if job["options"].get("save_last_frame"):
                from .paired_clip_recovery import _SCHEMA, final_checkpoint_path
                from .paired_output import companion_png_path

                final_path = final_checkpoint_path(path)
                still = companion_png_path(destination, job["options"]["format"], subdivide=True)
                final_saved = os.path.lexists(final_path)
                paired = False
                if final_saved:
                    if not self.resume or not saved:
                        raise ValueError("final-image journal requires its saved clip journal and resume=True")
                    final = _read_document(final_path)
                    if (type(final) is not dict or final.get("schema") != _SCHEMA
                            or type(final.get("version")) is not int or final["version"] != 1
                            or final.get("key") != self.key or type(final.get("plan")) is not dict
                            or type(final["plan"].get("clip_plan")) is not dict
                            or final["plan"]["clip_plan"].get("destination") != str(destination)
                            or final["plan"].get("still_destination") != str(still)
                            or set(final) != {"schema", "version", "key", "plan", "pair", "artifacts"}):
                        raise ValueError("final-image journal does not identify its planned output and input key")
                    paired = final["pair"] is not None
                    if paired and (type(final["pair"]) is not dict
                            or final["pair"].get("destination") != str(destination)
                            or final["pair"].get("still_destination") != str(still)
                            or final["pair"].get("completed") is not True):
                        raise ValueError("invalid completed final-image pair receipt")
                    if not paired and final["artifacts"] is not None:
                        raise ValueError("final-image journal has artifacts without a pair receipt")
                if paired:
                    # Permission for preflight only: the locked worker owner
                    # validates the entire plan, receipts and both artifacts.
                    self.existing_destinations.add(still)
                elif os.path.lexists(still):
                    raise FileExistsError("unrecorded final PNG already exists: " + str(still))
                recovery.update(final_checkpoint=str(final_path), resume_final=final_saved)
            self.recovery[job["name"]] = recovery

    def admit(self):
        if self.document is None:
            self._publish(_json({"schema": self.schema, "version": self.version,
                                 "key": self.key, "plan": self.plan}))


def render_clip_job(job, scene):
    """Delegate the one admitted scene to the existing resumable clip owner."""
    from ._process_batch import failure
    from .subdivision import SubdividedRenderResult
    from .paired_output import PairedRenderResult, companion_png_path, render_scene_with_still

    name, options = job["name"], job["options"]
    destination = Path(job["directory"]) / name / "clips"
    # This identity supplements the native session's resolved camera/writer,
    # class and constructor plan. Helpers/assets still belong to resume_key.
    identity = {key: job[key] for key in ("source", "source_digest", "working_directory", "name")}
    identity.update(scene=job.get("scene_name", name), configuration=options.get("_configuration"))
    render_options = {key: options[key] for key in (
        "format", "resolution", "fps", "threads", "animation_range", "max_segments", "_output_options")}
    paired = options.get("save_last_frame", False)
    still = companion_png_path(destination, options["format"], subdivide=True) if paired else None
    try:
        if paired:
            result = render_scene_with_still(
                scene, destination, still, scene_kwargs=job.get("scene_kwargs"),
                subdivide=True, _source_identity=identity,
                _clip_recovery=job["clip_recovery"], **render_options,
            )
        else:
            from .subdivision_rendering import render_subdivided_scene

            result = render_subdivided_scene(
                scene, destination, scene_kwargs=job.get("scene_kwargs"),
                _source_identity=identity, **render_options, **job["clip_recovery"],
            )
    except BaseException as error:
        row = failure(error, "failed" if isinstance(error, Exception) else "cancelled")
        result = getattr(error, "render_pair_result" if paired else "render_subdivision_result", None)
        if not (isinstance(result, PairedRenderResult if paired else SubdividedRenderResult)
                and result.destination == destination and not result.completed
                and (not paired or result.still_destination == still)):
            result = None
        notes = getattr(error, "__notes__", ())
        if isinstance(notes, (list, tuple)):
            row["error"]["notes"] = [note[:1024] for note in notes[:8] if isinstance(note, str)]
    else:
        row = {"status": "succeeded", "error": None}
    row.update(name=name, destination=str(destination), result=None if result is None else result.as_dict())
    return row
