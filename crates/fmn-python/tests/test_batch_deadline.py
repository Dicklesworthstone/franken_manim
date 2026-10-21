"""Shared deadline semantics and actual process shutdown, without a renderer."""
import multiprocessing as mp
import os
from pathlib import Path
import signal
import tempfile
from threading import Thread
import time
import unittest
from unittest.mock import patch

from test_caller_cancellation import cancellable_worker
from fmn_python import _process_batch as scheduler
from fmn_python.cancellation import BatchCancelled, BatchDeadlineExceeded, CancellationToken
from fmn_python.parallel_cli_options import take_parallel_options, start_batch_budget


class BatchDeadlineTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name).resolve()
        self.children = {p.pid for p in mp.active_children()}
        self.handler = signal.getsignal(signal.SIGINT)

    def tearDown(self):
        self.assertEqual({p.pid for p in mp.active_children()}, self.children)
        self.assertIs(signal.getsignal(signal.SIGINT), self.handler)

    def job(self, mode):
        return dict(root=str(self.root), name=mode, mode=mode)

    def test_invalid_durations_refuse_without_acquiring_work(self):
        for duration in (0, -1, float('nan'), float('inf'), -float('inf'), 10**1000, 'bad', object()):
            with self.subTest(duration=type(duration)), self.assertRaises(ValueError):
                CancellationToken(timeout=duration)
        with self.assertRaises(TypeError):
            CancellationToken(timeout=True)

    def test_shared_clock_is_not_reset_at_observation_and_expiry_stays_latched(self):
        with patch('fmn_python.cancellation.monotonic', return_value=100) as clock:
            token = CancellationToken(timeout=5)
            clock.return_value = 104.999
            token.raise_if_cancelled()
            self.assertFalse(token.cancelled)
            clock.return_value = 105
            with self.assertRaises(BatchDeadlineExceeded) as caught:
                token.raise_if_cancelled()
            self.assertEqual(caught.exception.timeout, 5)
            clock.return_value = 1  # An observation cannot reset an expired token.
            self.assertTrue(token.cancelled)
            with self.assertRaises(BatchDeadlineExceeded):
                token.raise_if_cancelled()

    def test_manual_request_before_deadline_remains_manual(self):
        with patch('fmn_python.cancellation.monotonic', return_value=10) as clock:
            token = CancellationToken(timeout=1)
            token.cancel()
            clock.return_value = 20
            with self.assertRaises(BatchCancelled) as caught:
                token.raise_if_cancelled()
            self.assertNotIsInstance(caught.exception, BatchDeadlineExceeded)

    def test_manual_request_after_deadline_cannot_disguise_expiry(self):
        with patch('fmn_python.cancellation.monotonic', return_value=10) as clock:
            token = CancellationToken(timeout=1)
            clock.return_value = 11
            threads = [Thread(target=token.cancel) for _ in range(8)]
            for thread in threads:
                thread.start()
            for thread in threads:
                thread.join()
            with self.assertRaises(BatchDeadlineExceeded):
                token.raise_if_cancelled()

    def test_expired_budget_prevents_first_process_acquisition(self):
        with patch('fmn_python.cancellation.monotonic', return_value=0) as clock:
            token = CancellationToken(timeout=1)
            clock.return_value = 1
            with patch.object(scheduler.mp, 'get_context', side_effect=AssertionError('no launch')):
                with self.assertRaises(BatchDeadlineExceeded) as caught:
                    scheduler.run_process_jobs([self.job('immediate')], cancellable_worker,
                                               workers=1, cancellation=token)
        self.assertEqual(caught.exception.process_batch_outcomes[0]['status'], 'not_run')
        self.assertEqual(list(self.root.iterdir()), [])

    @unittest.skipUnless(os.name == 'posix', 'cooperative SIGINT receipt requires POSIX')
    def test_real_deadline_retains_partial_receipt_and_stops_later_admission(self):
        token = CancellationToken(timeout=1.5)
        with self.assertRaises(BatchDeadlineExceeded) as caught:
            scheduler.run_process_jobs([self.job('cooperate'), self.job('immediate')],
                cancellable_worker, workers=1, cancellation=token, continue_on_error=True)
        rows = caught.exception.process_batch_outcomes
        self.assertEqual([row['status'] for row in rows], ['cancelled', 'not_run'])
        self.assertEqual(rows[0]['result']['partial'], ['already-published-clip'])
        self.assertTrue((self.root / 'cooperate.cleaned').exists())

    @unittest.skipUnless(os.name == 'posix', 'forced stop uses POSIX signals')
    def test_uncooperative_workers_escalate_together_with_deadline_identity(self):
        jobs = [{**self.job('ignore'), 'name': name} for name in ('first', 'second')]
        before = time.monotonic()
        with self.assertRaises(BatchDeadlineExceeded) as caught:
            scheduler.run_process_jobs(jobs, cancellable_worker, workers=2,
                                       cancellation=CancellationToken(timeout=1.5))
        self.assertLess(time.monotonic() - before, 8)
        rows = caught.exception.process_batch_outcomes
        self.assertEqual([r['status'] for r in rows], ['cancelled', 'cancelled'])
        self.assertEqual([r['error']['type'] for r in rows], ['BatchDeadlineExceeded'] * 2)

    @unittest.skipUnless(os.name == 'posix', 'blocked teardown uses POSIX signals')
    def test_published_success_survives_deadline_during_interpreter_teardown(self):
        with self.assertRaises(BatchDeadlineExceeded) as caught:
            scheduler.run_process_jobs([self.job('success_teardown')], cancellable_worker,
                workers=1, cancellation=CancellationToken(timeout=1.5))
        self.assertEqual(caught.exception.process_batch_outcomes[0]['status'], 'succeeded')
        self.assertTrue((self.root / 'success_teardown.teardown').exists())

    @unittest.skipUnless(os.name == 'posix', 'blocked teardown uses POSIX signals')
    def test_reported_failure_is_not_relabelled_as_deadline(self):
        with self.assertRaises(BatchDeadlineExceeded) as caught:
            scheduler.run_process_jobs([self.job('failure_teardown')], cancellable_worker,
                workers=1, continue_on_error=True, cancellation=CancellationToken(timeout=1.5))
        row = caught.exception.process_batch_outcomes[0]
        self.assertEqual(row['status'], 'failed')
        self.assertEqual(row['error']['type'], 'ValueError')

    def test_final_observer_overrun_retains_completed_success(self):
        with patch('fmn_python.cancellation.monotonic', return_value=0) as clock:
            token = CancellationToken(timeout=1)
            def observer(*_):
                clock.return_value = 1
            with self.assertRaises(BatchDeadlineExceeded) as caught:
                scheduler.run_process_jobs([self.job('immediate')], cancellable_worker,
                    workers=1, cancellation=token, on_result=observer)
        self.assertEqual(caught.exception.process_batch_outcomes[0]['status'], 'succeeded')

    def test_deadline_lexer_keeps_value_tokens_and_requires_workers(self):
        untouched, options = take_parallel_options(
            ['--video_dir', '--batch-timeout', '--workers=2', '--batch-timeout', '1.25',
             '--job-timeout=2'], {'--video_dir'})
        self.assertEqual(untouched, ['--video_dir', '--batch-timeout'])
        self.assertEqual(options, dict(workers=2, batch_timeout=1.25, job_timeout=2))
        token = start_batch_budget(options)
        self.assertIsInstance(token, CancellationToken)
        self.assertEqual(options, dict(workers=2, job_timeout=2))
        for args in (['--batch-timeout=1'], ['--workers=2','--batch-timeout'],
                     ['--workers=2','--batch-timeout=1','--batch-timeout=2'],
                     ['--workers=2','--batch-timeout=nan']):
            with self.subTest(args=args), self.assertRaises(ValueError):
                take_parallel_options(args)

    def test_absent_budget_does_not_create_token_or_mutate_worker_policy(self):
        options = {'workers': 4, 'job_timeout': 3}
        self.assertIsNone(start_batch_budget(options))
        self.assertEqual(options, {'workers': 4, 'job_timeout': 3})


if __name__ == '__main__':
    unittest.main()
