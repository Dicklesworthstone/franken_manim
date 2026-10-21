"""Shared CLI budgets over production wrappers, real spawn and checkpoints.

Native parsing/rendering and discovery inventory are explicit protocol fixtures.
NOT-MEDIA/NOT-FMTL output is not native rendering or installed-wheel acceptance.
"""
from contextlib import contextmanager
import json
import multiprocessing as mp
from pathlib import Path
import sys
import time
import types
import unittest
from unittest.mock import patch

import test_media_job_file_protocol as media_protocol
from test_media_job_file_protocol import execute_media_variant
from test_parallel_bundle_protocol import publish_bundle_job
from fmn_python import _process_batch as scheduler
from fmn_python.cancellation import BatchCancelled, CancellationToken


def delayed_media_job(job):
    if job['name'] == 'seed11':
        (Path(job['source']).parent / 'second-started').write_text('started')
        time.sleep(30)
    return execute_media_variant(job)


def blocked_discovery(job):
    Path(job['marker']).write_text('started')
    time.sleep(30)
    return {'status': 'succeeded', 'result': None, 'error': None}


class BatchDeadlineCliTests(unittest.TestCase):
    setUp = media_protocol.MediaJobFileTests.setUp
    write_jobs = media_protocol.MediaJobFileTests.write_jobs
    emit = media_protocol.MediaJobFileTests.emit
    discover = media_protocol.MediaJobFileTests.discover
    behavior = media_protocol.MediaJobFileTests.behavior
    parse = media_protocol.MediaJobFileTests.parse
    invoke = media_protocol.MediaJobFileTests.invoke
    assert_not_started = media_protocol.MediaJobFileTests.assert_not_started

    def test_media_discovery_and_render_share_exactly_one_token(self):
        self.assertEqual(self.invoke('--batch-timeout=30', '--job-timeout=10'), 0, self.records)
        token = self.discoveries[0][1]['cancellation']
        self.assertIsInstance(token, CancellationToken)
        self.assertIs(self.process_calls[0][1]['cancellation'], token)
        self.assertFalse(token.cancelled)
        self.assertNotIn('batch_timeout', self.process_calls[0][1])
        self.assertEqual(self.process_calls[0][1]['job_timeout'], 10)
        self.assertTrue(self.records[-1]['batch']['ok'])

    def test_fmtl_uses_the_same_budget_without_sending_it_to_capture(self):
        self.h.coordinator._render_source_job = publish_bundle_job
        self.assertEqual(self.invoke('--batch-timeout', '30', '--bundle-audio', format='fmtl'), 0,
                         self.records)
        token = self.discoveries[0][1]['cancellation']
        self.assertIs(self.process_calls[0][1]['cancellation'], token)
        for name in ('seed7', 'seed11'):
            job = json.loads((self.root / (name + '.job.json')).read_text())
            self.assertNotIn('cancellation', job)
            self.assertNotIn('batch_timeout', job['options'])
        self.assertTrue(self.records[-1]['batch']['ok'])

    def test_whole_media_budget_preserves_progress_and_resume_changes_only_scheduling(self):
        self.jobs.append({'name': 'seed20', 'scene': 'Lesson', 'scene_kwargs': {'value': 20}})
        self.write_jobs()
        self.h.coordinator._render_source_job = delayed_media_job
        flags = ('--workers=1', '--keep-going', '--checkpoint', str(self.checkpoint),
                 '--resume-key', 'stable-inputs')
        children = {p.pid for p in mp.active_children()}
        self.assertEqual(self.invoke(*flags, '--batch-timeout=2', workers=False), 6, self.records)
        receipt = self.records[-1]
        self.assertEqual(receipt['kind'], 'batch-deadline-exceeded')
        self.assertEqual(receipt['timeout_seconds'], 2)
        self.assertEqual([o['status'] for o in receipt['batch']['outcomes']],
                         ['succeeded', 'cancelled', 'not_run'])
        self.assertEqual({p.pid for p in mp.active_children()}, children)
        self.assertTrue((self.root / 'second-started').exists())
        path = self.output / 'seed7.png'
        saved = path.read_bytes(), path.stat().st_mtime_ns
        journal = json.loads(self.checkpoint.read_text())
        self.assertEqual(journal['outcomes'], receipt['batch']['outcomes'])
        self.assertNotIn('batch_timeout', journal['plan']['options'])
        self.h.coordinator._render_source_job = execute_media_variant
        self.assertEqual(self.invoke(*flags, '--batch-timeout=30', '--resume', workers=False), 0,
                         self.records)
        self.assertEqual(self.records[-1]['batch']['reused'], ['seed7'])
        self.assertEqual((path.read_bytes(), path.stat().st_mtime_ns), saved)
        self.assertTrue(self.records[-1]['batch']['ok'])

    def test_fmtl_deadline_is_not_reported_as_user_interrupt_or_success(self):
        self.h.coordinator._render_source_job = publish_bundle_job
        self.behavior(seed11='sleep')
        flags = ('--workers=1', '--batch-timeout=2', '--keep-going', '--bundle-audio')
        self.assertEqual(self.invoke(*flags, workers=False, format='fmtl'), 6, self.records)
        receipt = self.records[-1]
        self.assertEqual(receipt['identity'], 'render')
        self.assertEqual(receipt['kind'], 'batch-deadline-exceeded')
        self.assertEqual([o['status'] for o in receipt['batch']['outcomes']], ['succeeded', 'cancelled'])
        self.assertTrue((self.output / 'seed7.fmtl').is_file())
        self.assertFalse((self.output / 'seed11.fmtl').exists())

    def test_discovery_deadline_reaps_its_worker_and_never_starts_a_render(self):
        @contextmanager
        def discover(source, **options):
            self.discoveries.append((source, options))
            scheduler.run_process_jobs([{'marker': str(self.root / 'discovery-started')}],
                blocked_discovery, workers=1, cancellation=options['cancellation'])
            yield types.SimpleNamespace(scenes=self.names, source_digest='unreachable')
        with patch.object(sys.modules['fmn_python.parallel_discovery'], 'discovery_source', discover):
            children = {p.pid for p in mp.active_children()}
            self.assertEqual(self.invoke('--batch-timeout=1.5'), 6, self.records)
        self.assertEqual(self.records[-1]['kind'], 'batch-deadline-exceeded')
        self.assertEqual(self.records[-1]['phase'], 'load')
        self.assertNotIn('batch', self.records[-1])
        self.assertEqual(self.process_calls, [])
        self.assertFalse(self.output.exists())
        self.assertTrue((self.root / 'discovery-started').exists())
        self.assertEqual({p.pid for p in mp.active_children()}, children)

    def test_discovery_consumes_budget_instead_of_restarting_it_for_render(self):
        original = self.discover
        with patch('fmn_python.cancellation.monotonic', return_value=100) as clock:
            @contextmanager
            def discover(source, **options):
                with original(source, **options) as loaded:
                    clock.return_value = 110
                    yield loaded
            with patch.object(sys.modules['fmn_python.parallel_discovery'], 'discovery_source', discover):
                self.assertEqual(self.invoke('--batch-timeout=10'), 6, self.records)
        self.assertEqual(self.records[-1]['kind'], 'batch-deadline-exceeded')
        self.assertEqual(self.records[-1]['phase'], 'batch')
        self.assertIs(self.discoveries[0][1]['cancellation'], self.process_calls[0][1]['cancellation'])
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.root.glob('*.values')), [])

    def test_manual_cancellation_with_budget_keeps_interrupt_exit(self):
        self.discovery_error = BatchCancelled('host stopped discovery')
        self.assertEqual(self.invoke('--batch-timeout=30'), 130, self.records)
        self.assertNotEqual(self.records[-1]['kind'], 'batch-deadline-exceeded')
        self.assertEqual(self.process_calls, [])

    def test_invalid_budgets_refuse_before_discovery_in_both_front_doors(self):
        for format in ('png', 'fmtl'):
            for args in (('--batch-timeout=nan',), ('--batch-timeout=0',),
                         ('--batch-timeout=-1',), ('--batch-timeout=inf',),
                         ('--batch-timeout',), ('--batch-timeout=1','--batch-timeout=2')):
                with self.subTest(format=format,args=args):
                    self.assertEqual(self.invoke(*args, format=format), 2, self.records)
                    self.assertTrue(self.records[-1]['robot'])
                    self.assert_not_started()

    def test_budget_requires_explicit_process_ownership(self):
        for format in ('png', 'fmtl'):
            with self.subTest(format=format):
                self.assertEqual(self.invoke('--batch-timeout=1', format=format, workers=False), 2,
                                 self.records)
                self.assert_not_started()

    def test_all_completed_resume_does_not_bind_a_different_budget_into_checkpoint(self):
        flags = ('--checkpoint', str(self.checkpoint), '--resume-key', 'same-inputs')
        self.assertEqual(self.invoke(*flags, '--batch-timeout=30'), 0, self.records)
        files = {p.name: (p.read_bytes(),p.stat().st_mtime_ns) for p in self.output.iterdir()}
        with patch.object(self.h.coordinator, 'run_process_jobs', side_effect=AssertionError('no rerender')):
            self.assertEqual(self.invoke(*flags, '--batch-timeout=40', '--resume'), 0, self.records)
        self.assertEqual(self.records[-1]['batch']['reused'], ['seed7','seed11'])
        self.assertEqual({p.name:(p.read_bytes(),p.stat().st_mtime_ns) for p in self.output.iterdir()}, files)


if __name__ == '__main__':
    unittest.main()
