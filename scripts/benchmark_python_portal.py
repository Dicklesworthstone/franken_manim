#!/usr/bin/env python3
"""Measure real Python portal exports, including the external issue #8 scene.

Usage: python scripts/benchmark_python_portal.py config.json --output results/run1

The JSON config owns exact argv (no shell), separate interpreters, and per-engine
output expectations. Paths are relative to the config, except input paths and
commands run relative to ``cwd``. Example engine entries:

  {"label": "wheel-0.5.0", "python": "/env/wheel/bin/python",
   "command": ["{python}", "-m", "fmn_python", "fmn_part2.py",
               "GetPaidThenSell", "--format", "mp4", "--resolution", "640x360",
               "--fps", "15", "--threads", "4", "--video_dir", "{output}"],
   "expected": {"width": 640, "height": 360, "fps": "15/1", "frames": 716,
                "audio": true, "audio_duration": 47.733333}}

Current source may use ``--output`` instead of ``--video_dir``; record the exact
flag in that engine's command. CE can use ``--media_dir {run_dir}/media`` and
``artifact": "{run_dir}/media/videos/manim_part2/360p15/Part2Benchmark.mp4".
Specify its own expected frames (714 for the supplied issue #8 CE scene).

Top-level keys: cwd, sources (scene/helper paths), audio (asset paths), engines.
Optional keys: warmups (1), repetitions (2), timeout_seconds (900), ffmpeg,
ffprobe. An engine may declare repository (source checkout, NOT proof of wheel
origin), modules (default fmn_python and manimlib.manimlib), and expected.

Every run gets a unique directory. Warmups precede measured rounds, whose
engine order alternates. Installation, identity collection, ffprobe, and decode
validation are outside timings. --profile adds one separate cProfile run per
engine; profile commands are recorded and never included in timing summaries.
The original external scenes/assets are hashed, not copied or reconstructed.
Results describe this host and workload; they are not the native PG-1 gate.
"""
from __future__ import annotations

import argparse
from fractions import Fraction
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import signal
import statistics
import subprocess
import sys
import time

try:
    import resource
except ImportError:  # RUSAGE_CHILDREN is unavailable on Windows.
    resource = None


IDENTITY = r"""
import hashlib, importlib.metadata, importlib.util, json, platform, sys
result = dict(python=sys.version, executable=sys.executable,
              platform=platform.platform(), packages={}, modules={})
for dist in importlib.metadata.distributions():
    result['packages'][dist.metadata['Name']] = dist.version
for name in sys.argv[1:]:
    try:
        spec = importlib.util.find_spec(name)
        path = spec.origin if spec is not None else None
        info = {'path': path}
        if path and path not in ('built-in', 'frozen'):
            with open(path, 'rb') as source:
                info['sha256'] = hashlib.file_digest(source, 'sha256').hexdigest()
        result['modules'][name] = info
    except Exception as error:
        result['modules'][name] = {'error': str(error)}
print(json.dumps(result, sort_keys=True))
"""


def fingerprint(paths, cwd):
    result = {}
    for name in paths:
        path = (cwd / name).resolve()
        with path.open("rb") as source:
            digest = hashlib.file_digest(source, "sha256").hexdigest()
        result[str(path)] = {"sha256": digest, "bytes": path.stat().st_size}
    return result


def cpu_seconds():
    if resource is None:
        return None
    usage = resource.getrusage(resource.RUSAGE_CHILDREN)
    return usage.ru_utime + usage.ru_stime


def run_process(command, cwd, log, timeout):
    """Time only the child, retaining failures and terminating its process group."""
    before = cpu_seconds()
    started = time.perf_counter()
    status = {"command": command, "cwd": str(cwd), "log": str(log)}
    with log.open("xb") as stream:
        try:
            process = subprocess.Popen(command, cwd=cwd, stdout=stream,
                                       stderr=subprocess.STDOUT,
                                       start_new_session=os.name == "posix")
            try:
                status["exit_code"] = process.wait(timeout=timeout)
            except (subprocess.TimeoutExpired, KeyboardInterrupt) as error:
                # The child owns a new session, so an interrupt of this driver
                # does not stop its renderer/encoder. Reap it before returning
                # the original interrupt to the caller.
                try:
                    if os.name == "posix":
                        os.killpg(process.pid, signal.SIGKILL)
                    else:
                        process.kill()
                except ProcessLookupError:
                    pass  # It exited between wait timing out and the kill.
                status["exit_code"] = process.wait()
                if isinstance(error, KeyboardInterrupt):
                    raise
                status["timed_out"] = True
        except OSError as error:
            status.update(exit_code=None, error=str(error))
    status["wall_seconds"] = time.perf_counter() - started
    after = cpu_seconds()
    status["cpu_seconds"] = None if before is None else after - before
    return status


def capture(command, cwd, timeout=60):
    completed = subprocess.run(command, cwd=cwd, capture_output=True, text=True,
                               timeout=timeout, check=False)
    if completed.returncode:
        raise ValueError(f"{command[0]} exited {completed.returncode}: {completed.stderr.strip()}")
    return completed.stdout


def validate_probe(probe, expected):
    """Check actual decoded count, stream timing and caller-declared invariants."""
    videos = [s for s in probe.get("streams", []) if s.get("codec_type") == "video"]
    audios = [s for s in probe.get("streams", []) if s.get("codec_type") == "audio"]
    if len(videos) != 1:
        raise ValueError(f"expected one video stream, found {len(videos)}")
    video = videos[0]
    frames = int(video["nb_read_frames"])
    fps = Fraction(video["r_frame_rate"])
    average_fps = Fraction(video["avg_frame_rate"])
    duration = float(video["duration"])
    if frames <= 0 or fps <= 0 or average_fps <= 0 or not math.isfinite(duration) or duration <= 0:
        raise ValueError("empty or invalid video timeline")
    # CE concatenates animation clips and its issue #8 output has a slightly
    # different average rate (15.0002) despite a nominal rate of 15. Preserve
    # that distinction and allow explicit container rounding, not lost frames.
    tick = float(Fraction(video.get("time_base", "1/1000000")))
    if abs(duration - frames / float(average_fps)) > tick + 0.000001:
        raise ValueError("video duration does not match decoded frames and fps")
    if abs(duration - frames / float(fps)) > expected.get("duration_tolerance", 0.001):
        raise ValueError("video duration differs from its nominal frame timeline")
    if abs(float(video.get("start_time", 0))) > tick + 0.000001:
        raise ValueError("video does not start at time zero")
    for name in ("width", "height"):
        if name in expected and video[name] != expected[name]:
            raise ValueError(f"{name}: expected {expected[name]}, got {video[name]}")
    if "fps" in expected and fps != Fraction(str(expected["fps"])):
        raise ValueError(f"fps: expected {expected['fps']}, got {fps}")
    if "frames" in expected and frames != expected["frames"]:
        raise ValueError(f"frames: expected {expected['frames']}, got {frames}")
    if "audio" in expected and bool(audios) != expected["audio"]:
        raise ValueError("audio stream presence differs from expectation")
    if audios:
        if len(audios) != 1:
            raise ValueError("expected one audio stream")
        audio = audios[0]
        audio_duration = float(audio["duration"])
        if not math.isfinite(audio_duration) or audio_duration <= 0:
            raise ValueError("empty or invalid audio timeline")
        # AAC encoder delay/padding can occupy one packet. Keep tolerance
        # explicit in evidence; this is not a claim of PCM bit identity.
        tolerance = expected.get("audio_tolerance", 0.05)
        if abs(float(audio.get("start_time", 0))) > tolerance:
            raise ValueError("audio does not start at time zero")
        if ("audio_duration" in expected
                and abs(audio_duration - expected["audio_duration"]) > tolerance):
            raise ValueError("audio duration differs from expectation")
    elif "audio_duration" in expected:
        raise ValueError("audio duration requested but audio stream is missing")
    return {"frames": frames, "fps": str(fps), "average_fps": str(average_fps), "video_duration": duration,
            "audio_duration": float(audios[0]["duration"]) if audios else None}


def expand(command, values):
    if not isinstance(command, list) or not command or any(not isinstance(a, str) for a in command):
        raise ValueError("command must be a nonempty argv array")
    # Only named placeholders are substituted: braces in authored argv are
    # literal, and neither shell expansion nor Python format syntax runs.
    return [replace_tokens(arg, values) for arg in command]


def replace_tokens(value, values):
    for name, replacement in values.items():
        value = value.replace("{" + name + "}", str(replacement))
    return value


def interpreter_path(base, value):
    # A venv's Python is commonly a symlink to its base interpreter. Resolving
    # that symlink changes sys.prefix and silently benchmarks the wrong env.
    return str((base / value).absolute())


def summarize(rows, labels, repetitions, warmups):
    summary = {}
    for label in labels:
        samples = [r for r in rows if r["engine"] == label and r["kind"] == "measured"]
        preparation = [r for r in rows if r["engine"] == label and r["kind"] == "warmup"]
        warmups_complete = len(preparation) == warmups and all(r.get("valid", False) for r in preparation)
        complete = warmups_complete and len(samples) == repetitions and all(r.get("valid", False) for r in samples)
        summary[label] = {"complete": complete, "samples": len(samples),
                          "warmups": len(preparation), "warmups_complete": warmups_complete}
        if complete:
            values = [r["wall_seconds"] for r in samples]
            median = statistics.median(values)
            summary[label].update(mean_seconds=statistics.mean(values),
                                  median_seconds=median, min_seconds=min(values),
                                  max_seconds=max(values),
                                  median_absolute_deviation=statistics.median(abs(v - median) for v in values))
    return summary


def benchmark(config, config_path, destination, profile=False):
    base = config_path.parent
    cwd = (base / config["cwd"]).resolve()
    engines = config["engines"]
    labels = [e["label"] for e in engines]
    if not labels or len(set(labels)) != len(labels):
        raise ValueError("engines must have unique labels")
    if any(not label or any(c not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_.-" for c in label)
           or label in (".", "..") for label in labels):
        raise ValueError("engine labels must be simple filenames")
    if not config.get("sources"):
        raise ValueError("sources must list the external scene and helper inputs")
    warmups, repetitions = config.get("warmups", 1), config.get("repetitions", 2)
    if type(warmups) is not int or warmups < 0 or type(repetitions) is not int or repetitions < 1:
        raise ValueError("warmups must be nonnegative and repetitions must be positive integers")
    timeout = config.get("timeout_seconds", 900)
    if not math.isfinite(timeout) or timeout <= 0:
        raise ValueError("timeout_seconds must be positive and finite")
    destination.mkdir(parents=True, exist_ok=False)
    inputs = {kind: fingerprint(config.get(kind, []), cwd) for kind in ("sources", "audio")}
    evidence = {"schema_version": 1, "config": config, "config_path": str(config_path),
                "inputs_before": inputs, "host": {"platform": platform.platform(),
                "cpu_count": os.cpu_count(), "python": sys.version}, "engines": {}, "runs": []}
    if hasattr(os, "getloadavg"):
        evidence["host"]["load_before"] = list(os.getloadavg())
    ffmpeg, ffprobe = config.get("ffmpeg", "ffmpeg"), config.get("ffprobe", "ffprobe")
    evidence["host"]["ffmpeg"] = capture([ffmpeg, "-version"], cwd)
    for engine in engines:
        python = interpreter_path(base, engine["python"])
        identity = json.loads(capture([python, "-c", IDENTITY,
            *engine.get("modules", ["fmn_python", "manimlib.manimlib"])], cwd))
        if "repository" in engine:
            repository = (base / engine["repository"]).resolve()
            identity["source_checkout"] = {"path": str(repository),
                "commit": capture(["git", "rev-parse", "HEAD"], repository).strip(),
                "status": capture(["git", "status", "--porcelain"], repository),
                "diff_sha256": hashlib.sha256(capture(["git", "diff", "HEAD"], repository).encode()).hexdigest()}
        evidence["engines"][engine["label"]] = identity

    def save():
        evidence["summary"] = summarize(evidence["runs"], labels, repetitions, warmups)
        (destination / "results.json").write_text(json.dumps(evidence, indent=2) + "\n")

    save()
    rounds = [("warmup", i) for i in range(warmups)] + [("measured", i) for i in range(repetitions)]
    if profile:
        rounds.append(("profile", 0))
    for kind, number in rounds:
        for engine in (engines if number % 2 == 0 else list(reversed(engines))):
            run_dir = destination / f"{kind}-{number + 1}-{engine['label']}"
            run_dir.mkdir()
            values = {"python": interpreter_path(base, engine["python"]),
                      "run_dir": run_dir, "output": run_dir / "output.mp4",
                      "profile": run_dir / "profile.pstats"}
            command = expand(engine["command"], values)
            if kind == "profile":
                if "profile_command" in engine:
                    command = expand(engine["profile_command"], values)
                else:
                    arguments = command[1:] if command[0] == values["python"] else command
                    command = [values["python"], "-m", "cProfile", "-o", str(values["profile"]), *arguments]
            print(f"{kind} {number + 1}: {engine['label']}", flush=True)
            row = run_process(command, cwd, run_dir / "render.log", timeout)
            row.update(engine=engine["label"], kind=kind, round=number + 1, valid=False)
            artifact = Path(replace_tokens(engine.get("artifact", "{output}"), values))
            if not artifact.is_absolute():
                artifact = cwd / artifact
            row["artifact"] = str(artifact)
            evidence["runs"].append(row)
            save()  # A failed validation or interrupted next run retains timings.
            if row["exit_code"] == 0:
                try:
                    probe_command = [ffprobe, "-v", "error", "-count_frames", "-show_streams",
                                     "-show_format", "-of", "json", str(artifact)]
                    row["probe_command"] = probe_command
                    row["probe"] = json.loads(capture(probe_command, cwd, timeout))
                    row["validation"] = validate_probe(row["probe"], engine.get("expected", {}))
                    decode = run_process([ffmpeg, "-v", "error", "-xerror", "-i", str(artifact),
                                          "-f", "null", "-"], cwd, run_dir / "decode.log", timeout)
                    row["decode"] = decode
                    if decode["exit_code"] != 0:
                        raise ValueError("full video/audio decode failed")
                    row["artifact_sha256"] = fingerprint([str(artifact)], cwd)[str(artifact.resolve())]["sha256"]
                    row["valid"] = True
                except (OSError, ValueError, KeyError, ZeroDivisionError, subprocess.TimeoutExpired) as error:
                    row["validation_error"] = str(error)
            save()
            print(f"  {row['wall_seconds']:.3f}s, exit={row['exit_code']}, valid={row['valid']}", flush=True)
    evidence["inputs_after"] = {kind: fingerprint(config.get(kind, []), cwd) for kind in ("sources", "audio")}
    evidence["inputs_unchanged"] = evidence["inputs_after"] == inputs
    if not evidence["inputs_unchanged"]:
        for row in evidence["runs"]:
            row.update(valid=False, validation_error="workload inputs changed during benchmark")
    save()
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("config", type=Path)
    parser.add_argument("--output", type=Path, required=True, help="new evidence directory; never reused")
    parser.add_argument("--profile", action="store_true", help="separate cProfile export after all measured runs")
    args = parser.parse_args()
    config_path = args.config.resolve()
    evidence = benchmark(json.loads(config_path.read_text()), config_path, args.output.resolve(), args.profile)
    print(json.dumps(evidence["summary"], indent=2))
    return 0 if evidence["inputs_unchanged"] and all(r["valid"] for r in evidence["runs"]) else 1


if __name__ == "__main__":
    raise SystemExit(main())
