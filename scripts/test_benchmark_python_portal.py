"""Benchmark evidence checks; no native renderer or wall-time performance gate."""
import copy
import json
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
import venv
from unittest.mock import Mock, call, patch

import benchmark_python_portal as bench


def probe(frames=716):
    return {"streams": [
        {"codec_type": "video", "width": 640, "height": 360,
         "r_frame_rate": "15/1", "avg_frame_rate": "15/1", "time_base": "1/15360",
         "duration": str(frames / 15), "start_time": "0", "nb_read_frames": str(frames)},
        {"codec_type": "audio", "duration": "47.733333", "start_time": "0"},
    ]}


EXPECTED = {"width": 640, "height": 360, "fps": "15/1", "frames": 716,
            "audio": True, "audio_duration": 47.733333}


class ProbeChecks(unittest.TestCase):
    def test_original_fmn_and_ce_container_timing(self):
        self.assertEqual(bench.validate_probe(probe(), EXPECTED)["frames"], 716)
        ce = probe(714)
        ce["streams"][0].update(avg_frame_rate="5483520/365563", duration="47.599349")
        ce["streams"][1]["duration"] = "47.040000"
        expected = dict(EXPECTED, frames=714, audio_duration=47.04)
        self.assertEqual(bench.validate_probe(ce, expected)["fps"], "15")

    def test_dropped_frame_wrong_rate_missing_or_shifted_audio_fail(self):
        cases = []
        wrong_count = probe(715)
        cases.append(wrong_count)
        wrong_rate = probe()
        wrong_rate["streams"][0]["r_frame_rate"] = "30/1"
        cases.append(wrong_rate)
        cases.append({"streams": [probe()["streams"][0]]})
        shifted = probe()
        shifted["streams"][1]["start_time"] = "0.5"
        cases.append(shifted)
        truncated = probe()
        truncated["streams"][1]["duration"] = "40"
        cases.append(truncated)
        for value in cases:
            with self.subTest(probe=value):
                with self.assertRaises(ValueError):
                    bench.validate_probe(value, EXPECTED)

    def test_full_decode_count_is_required_and_nonfinite_duration_refused(self):
        value = probe()
        del value["streams"][0]["nb_read_frames"]
        value["streams"][0]["nb_frames"] = "716"
        with self.assertRaises(KeyError):
            bench.validate_probe(value, EXPECTED)
        value = probe()
        value["streams"][0]["duration"] = "nan"
        with self.assertRaises(ValueError):
            bench.validate_probe(value, EXPECTED)


class EvidenceChecks(unittest.TestCase):
    def test_symlinked_venv_interpreter_keeps_its_environment(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = root / "env"
            venv.create(environment, with_pip=False, symlinks=True)
            executable = bench.interpreter_path(root, "env/bin/python")
            prefix = subprocess.check_output(
                [executable, "-c", "import sys; print(sys.prefix)"], text=True, timeout=30).strip()
            self.assertEqual(Path(prefix), environment)

    def test_warmups_profiles_and_failed_samples_do_not_improve_the_mean(self):
        samples = [dict(engine="wheel", kind="measured", valid=True, wall_seconds=n) for n in (10, 12)]
        samples += [dict(engine="wheel", kind=k, valid=True, wall_seconds=999) for k in ("warmup", "profile")]
        result = bench.summarize(samples, ["wheel"], 2, 1)["wheel"]
        self.assertEqual(result["mean_seconds"], 11)
        self.assertEqual(result["median_absolute_deviation"], 1)
        samples[1]["valid"] = False
        result = bench.summarize(samples, ["wheel"], 2, 1)["wheel"]
        self.assertFalse(result["complete"])
        self.assertNotIn("mean_seconds", result)

    def test_failed_or_missing_warmup_invalidates_the_timing_summary(self):
        samples = [dict(engine="wheel", kind="measured", valid=True, wall_seconds=n) for n in (10, 12)]
        for warmup in ([], [dict(engine="wheel", kind="warmup", valid=False, wall_seconds=1)]):
            with self.subTest(warmup=warmup):
                result = bench.summarize(samples + warmup, ["wheel"], 2, 1)["wheel"]
                self.assertFalse(result["warmups_complete"])
                self.assertFalse(result["complete"])
                self.assertNotIn("mean_seconds", result)
        self.assertTrue(bench.summarize(samples, ["wheel"], 2, 0)["wheel"]["complete"])

    def test_commands_preserve_literals_and_use_no_shell_syntax(self):
        command = ["{python}", "script.py", "--output", "{output}", "{x}", "$(not-a-shell)"]
        self.assertEqual(bench.expand(command, {"python": "/some env/python", "output": "/tmp/movie.mp4"}),
                         ["/some env/python", "script.py", "--output", "/tmp/movie.mp4", "{x}", "$(not-a-shell)"])
        with self.assertRaises(ValueError):
            bench.expand("python script.py", {})

    def test_child_failure_and_timeout_keep_exact_logs_and_status(self):
        with tempfile.TemporaryDirectory() as directory:
            cwd = Path(directory)
            row = bench.run_process([sys.executable, "-c", "print('failed scene'); raise SystemExit(7)"],
                                    cwd, cwd / "failed.log", 10)
            self.assertEqual(row["exit_code"], 7)
            self.assertEqual((cwd / "failed.log").read_text().strip(), "failed scene")
            self.assertGreater(row["wall_seconds"], 0)
            row = bench.run_process([sys.executable, "-c", "import time; time.sleep(10)"],
                                    cwd, cwd / "timeout.log", 0.05)
            self.assertTrue(row["timed_out"])
            self.assertNotEqual(row["exit_code"], 0)

    def test_interrupt_kills_and_reaps_child_before_propagating(self):
        child = Mock(pid=12345)
        child.wait.side_effect = [KeyboardInterrupt(), -9]
        with tempfile.TemporaryDirectory() as directory:
            cwd = Path(directory)
            with patch.object(bench.subprocess, "Popen", return_value=child), \
                    patch.object(bench.os, "killpg", create=True) as killpg:
                with self.assertRaises(KeyboardInterrupt):
                    bench.run_process(["fake-renderer"], cwd, cwd / "interrupt.log", 10)
            if bench.os.name == "posix":
                killpg.assert_called_once_with(child.pid, bench.signal.SIGKILL)
            else:
                child.kill.assert_called_once_with()
            self.assertEqual(child.wait.call_args_list, [call(timeout=10), call()])

    def test_sequential_counterbalanced_rounds_and_separate_profiles(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scene.py").write_text("# external workload\n")
            (root / "audio.wav").write_bytes(b"audio identity")
            config = {"cwd": ".", "sources": ["scene.py"], "audio": ["audio.wav"],
                      "engines": [{"label": label, "python": sys.executable,
                                   "command": ["{python}", "-m", "fmn_python", "scene.py", "--output", "{output}"],
                                   "expected": EXPECTED} for label in ("wheel", "main")],
                      "warmups": 1, "repetitions": 2}
            events = []

            def capture(command, cwd, timeout=60):
                if command[0] == "ffmpeg":
                    return "ffmpeg test identity"
                if command[0] == "ffprobe":
                    return json.dumps(probe())
                return json.dumps({"python": "test interpreter", "modules": {}})

            def process(command, cwd, log, timeout):
                if command[0] != "ffmpeg":
                    events.append(log.parent.name)
                    Path(command[-1]).write_bytes(b"validated by independent probe/decode test doubles")
                log.write_text("test process\n")
                return {"command": command, "exit_code": 0, "wall_seconds": 1, "cpu_seconds": 0.5}

            with patch.object(bench, "capture", side_effect=capture), patch.object(bench, "run_process", side_effect=process):
                evidence = bench.benchmark(config, root / "config.json", root / "results", profile=True)
            self.assertEqual(events, ["warmup-1-wheel", "warmup-1-main", "measured-1-wheel", "measured-1-main",
                                      "measured-2-main", "measured-2-wheel", "profile-1-wheel", "profile-1-main"])
            self.assertTrue(evidence["inputs_unchanged"])
            self.assertEqual(evidence["summary"]["wheel"]["samples"], 2)
            self.assertEqual(evidence["summary"]["wheel"]["mean_seconds"], 1)
            profiles = [r for r in evidence["runs"] if r["kind"] == "profile"]
            self.assertTrue(all("cProfile" in r["command"] for r in profiles))
            self.assertTrue(all(r["decode"]["exit_code"] == 0 for r in evidence["runs"]))
            self.assertTrue((root / "results/results.json").is_file())
            with self.assertRaises(FileExistsError):
                bench.benchmark(copy.deepcopy(config), root / "config.json", root / "results")


if __name__ == "__main__":
    unittest.main()
