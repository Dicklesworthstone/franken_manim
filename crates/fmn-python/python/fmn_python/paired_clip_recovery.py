"""Durable final-image receipts alongside the existing clip-prefix journal.

A final image is reused only with its exact completed clip receipt and frozen
native-session plan. The two journals and the image are separate atomic
publications, not one filesystem transaction. Unrecorded images are refused,
never adopted or overwritten. Native capture and clip replay keep their owners.
"""
from __future__ import annotations

import os
from pathlib import Path
import sys

from .batch_checkpoint import _CheckpointFile, _json
from .paired_output import PairedRenderResult, PairedRenderSession, companion_png_path
from .subdivision import SubdividedRenderResult
from .subdivision_checkpoint import collection_inventory, collection_receipt

_SCHEMA = "fmn-python.clip-final-image"


def final_checkpoint_path(checkpoint):
    path = Path(checkpoint)
    return path.with_name(path.stem + ".still.json")


def prepare_paired_recovery(recovery, destination, still_destination):
    """Freeze both journal paths before an authored constructor can change cwd."""
    fields = {"checkpoint", "resume", "resume_key", "final_checkpoint", "resume_final"}
    if type(recovery) is not dict or set(recovery) != fields:
        raise ValueError("invalid paired clip recovery options")
    if type(recovery["resume"]) is not bool or type(recovery["resume_final"]) is not bool:
        raise TypeError("paired clip resume settings must be bool")
    owners = [_CheckpointFile(recovery[key], resume=False, key=recovery["resume_key"])
              for key in ("checkpoint", "final_checkpoint")]
    paths = [owner.path for owner in owners]
    if paths[1] != final_checkpoint_path(paths[0]):
        raise ValueError("final-image journal must be the clip journal's named companion")
    for owner in owners:
        owner.validate_destinations([destination, still_destination])
    return dict(recovery, checkpoint=paths[0], final_checkpoint=paths[1])


class FinalImageCheckpoint(_CheckpointFile):
    """Bind a native PNG receipt to one completed, verified clip collection."""
    schema = _SCHEMA
    version = 1

    def prepare(self, primary, still_destination):
        self.destination = primary.destination
        self.options = dict(primary._journal.plan["options"], subdivide=True, save_last_frame=True)
        self.plan = {"clip_plan": primary._journal.plan, "still_destination": str(still_destination)}
        self.pair, self.inventory = None, None
        document = self.document
        if document is not None:
            if (set(document) != {"schema", "version", "key", "plan", "pair", "artifacts"}
                    or _json(document["plan"]) != _json(self.plan)):
                raise ValueError("final-image checkpoint inputs or render options do not match")
            data, inventory = document["pair"], document["artifacts"]
            if data is not None:
                self.pair = collection_receipt(data, self.destination, self.options)
                self.inventory = inventory
                saved = SubdividedRenderResult(self.destination, tuple(primary._journal.segments),
                                                primary._journal.complete)
                self.verify(saved)
            elif inventory is not None:
                raise ValueError("final-image checkpoint has artifacts without a publication receipt")
        if self.pair is None and os.path.lexists(still_destination):
            raise FileExistsError("unrecorded final PNG already exists: " + str(still_destination))
        if document is None:
            self._publish(_json({"schema": self.schema, "version": self.version, "key": self.key,
                                 "plan": self.plan, "pair": None, "artifacts": None}))

    def verify(self, primary):
        if self.pair is None or _json(self.pair.primary.as_dict()) != _json(primary.as_dict()):
            raise ValueError("final PNG does not identify the completed clip collection")
        actual = collection_inventory(self.pair.as_dict(), self.destination, self.options)
        if _json(actual) != _json(self.inventory):
            raise ValueError("final-image checkpoint artifacts are missing or modified")

    def record(self, pair):
        data = pair.as_dict()
        receipt = collection_receipt(data, self.destination, self.options)
        if receipt.still_destination != Path(self.plan["still_destination"]):
            raise ValueError("final PNG changed its admitted destination")
        inventory = collection_inventory(data, self.destination, self.options)
        self._publish(_json({"schema": self.schema, "version": self.version, "key": self.key,
                             "plan": self.plan, "pair": data, "artifacts": inventory}))
        self.pair, self.inventory = receipt, inventory


class PairedClipRenderSession(PairedRenderSession):
    """Reuse the ordinary paired owner with resumable primary and final receipt."""
    def __init__(self, primary, still_destination, *, checkpoint, resume):
        super().__init__(primary, still_destination)
        if self.still_destination != companion_png_path(self.destination, self._config.format, subdivide=True):
            raise ValueError("clip recovery requires the collection's adjacent final PNG")
        self._image_journal = FinalImageCheckpoint(checkpoint, resume=resume, key=primary._journal.key)
        self._image_journal.validate_destinations([self.destination, self.still_destination])
        if self._image_journal.path != final_checkpoint_path(primary._journal.path):
            raise ValueError("final-image journal must be the clip journal's named companion")

    def _enter_primary(self):
        self.primary.destination = self.destination
        self.primary.__enter__()
        try:
            self._image_journal.__enter__()
            self._image_journal.prepare(self.primary, self.still_destination)
        except BaseException as error:
            try:
                self.primary.abort()
            except BaseException as cleanup:
                BaseException.add_note(error, "clip recovery cleanup also failed: " + type(cleanup).__name__)
            self._release()
            raise

    def _prepare_still(self):
        if self._image_journal.pair is None:
            return super()._prepare_still()
        # Defer exposing the saved still until replay and primary finish succeed.
        # Otherwise an interrupted replay would falsely look like a complete pair.
        return None

    def _commit_still(self, capture_threads):
        journal = self._image_journal
        if journal.pair is not None:
            journal.verify(self.primary.result)
            self._still = journal.pair.still
            self._still_published = True
        else:
            super()._commit_still(capture_threads)
            journal.record(PairedRenderResult(self.destination, self.still_destination,
                                              self.primary.result, self._still, True))

    def _release(self):
        super()._release()
        error = sys.exception()
        try:
            self._image_journal.__exit__(None, None, None)
        except BaseException as cleanup:
            if error is None:
                raise
            BaseException.add_note(error, "final-image checkpoint cleanup also failed: " + type(cleanup).__name__)
