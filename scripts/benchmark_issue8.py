#!/usr/bin/env python3
"""Run the supplied issue #8 workload through installed, independently built wheels.

The caller owns downloads, extraction, dependency installation and a clean source
wheel build with a declared Cargo profile. This entry point verifies identities before calling
benchmark_python_portal.py. It checks work and output correctness, never a noisy
host-time threshold or a native PG parity claim. All evidence stays in a new
directory, including failed runs; no input or existing output is overwritten.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import tomllib
import wave
import zipfile

import benchmark_python_portal as portal


ARCHIVE_SHA256 = "78e62e32f9e1d3a274c1c599e09aa06b1b682823c658d36c228476f0dd0ce16e"
RELEASED_NATIVE_SHA256 = "889f2ad148fdfdc593e401cfb24c9266a532b400583977b77be7315c28e4d296"
WAV_SHA256 = "e0adff5a80fa8e757d3d3b63b737a065d84464ad06d2e38e1fd3307163e80bfb"
INPUT_SHA256 = {
    "fmn_part2.py": "29ccd5f4e8e815aa6027a5fcfcd505f6f46159bd9f69c2eadb45ec2880ba9567",
    "manim_part2.py": "f63597d436b0ac235a7c1287847798ae2dfa222d1731ec5386e91124ac4901fb",
    "stock_valuation.py": "bf4f388525be1c3a443f021b20a67e306dc6ff9ffc5fe74731948f2401376b39",
    "beginner_stock_valuation.py": "54b636757b048437a4659a7eff1164244a1eb8e3b35a19e01dac247ddaddf683",
    "audio_beginner/timed_manifest.json": "8807e73b51a2fbd2c4f9eec8d5c204d83d33c5ef4340e7e77770353dbf7e8a64",
    "audio_beginner/normalized/02_part2_where_value_comes_from.wav":
        "29fa7b0b4eb00fcf7899f60c07e3d397d27200522f44d3cccedf67259ac8ad4b",
}
# The unchanged scene has seven waits totaling 602 frames. Conservatively
# permit one changed first frame per wait. The first real source run reused
# 598 frames; that observation is not the regression threshold.
MINIMUM_STATIC_REUSES = 602 - 7
# All eight play intervals visibly change the scene. This coarse floor rejects
# a fully frozen movie; the installed retained-conversion fixture supplies the
# independent moving-pixel oracles, not this accounting check alone.
MINIMUM_ANIMATED_CONVERSIONS = 8
FRAMES = 716


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def verify_inputs(archive, directory):
    require(sha256(archive) == ARCHIVE_SHA256, "issue #8 archive digest differs")
    actual = {name: sha256(directory / name) for name in INPUT_SHA256}
    for name, expected in INPUT_SHA256.items():
        require(actual[name] == expected, f"original workload input changed: {name}")
    return actual


def receipt(log):
    text = Path(log).read_text()
    decoder, found, offset = json.JSONDecoder(), [], 0
    while (start := text.find("{", offset)) >= 0:
        try:
            value, consumed = decoder.raw_decode(text[start:])
        except json.JSONDecodeError:
            offset = start + 1
            continue
        offset = start + consumed
        if isinstance(value, dict) and value.get("schema") == "fmn-python.cli":
            found.append(value)
    require(len(found) == 1, f"expected one FMN robot receipt in {log}")
    require(found[0].get("exit", {}).get("code") == 0, "FMN receipt reports failure")
    return found[0]


def verify_source_work(stats):
    names = ("frames", "planar_frames", "camera_frames", "raw_frame_reuses",
             "converted_frames", "conversion_reuses", "conversion_cache_bytes")
    require(all(type(stats.get(name)) is int and stats[name] >= 0 for name in names),
            "source receipt lacks valid native frame/reuse counters")
    require(stats["frames"] == FRAMES and stats["planar_frames"] == FRAMES
            and stats["camera_frames"] == 0, "original planar frame route/count changed")
    require(stats["raw_frame_reuses"] == stats["conversion_reuses"],
            "unchanged raw frames were converted again")
    require(stats["converted_frames"] + stats["conversion_reuses"] == FRAMES,
            "native conversion accounting lost frames")
    require(stats["conversion_reuses"] >= MINIMUM_STATIC_REUSES,
            "static-wait reuse regressed below the 602-minus-seven-frame bound")
    require(stats["converted_frames"] >= MINIMUM_ANIMATED_CONVERSIONS,
            "too few changed frames for the eight visibly animated play intervals")
    require(stats["conversion_cache_bytes"] > 0, "retained conversion cache is absent")


def verify_repeatability(rows):
    labels = {row["engine"] for row in rows}
    for label in labels:
        samples = [row for row in rows if row["engine"] == label]
        require(len(samples) >= 2, f"repeatability needs multiple outputs for {label}")
        for stream in ("video", "audio"):
            require(len({row["decoded"][stream]["sha256"] for row in samples}) == 1,
                    f"decoded {stream} changed between repeated {label} outputs")


def verify_media_format(probe):
    """Preserve the original encoding work and audio layout for all engines."""
    videos = [stream for stream in probe.get("streams", []) if stream.get("codec_type") == "video"]
    audios = [stream for stream in probe.get("streams", []) if stream.get("codec_type") == "audio"]
    require(len(videos) == len(audios) == 1, "issue #8 requires one video and one audio stream")
    require(videos[0].get("codec_name") == "h264" and videos[0].get("pix_fmt") == "yuv420p",
            "issue #8 video must remain H.264 yuv420p")
    audio = audios[0]
    require(audio.get("codec_name") == "aac" and str(audio.get("sample_rate")) == "48000"
            and audio.get("channels") == 2 and audio.get("channel_layout") == "stereo",
            "issue #8 audio must remain AAC 48000 Hz stereo")


def make_config(args):
    common = {"width": 640, "height": 360, "fps": "15/1", "audio": True}
    ce = {"label": "ce-0.21.0", "python": str(args.ce_python.absolute()),
          "modules": ["manim"], "command": ["{python}", "-m", "manim", "--renderer", "cairo",
          "--disable_caching", "--resolution", "640,360", "--fps", "15", "--media_dir",
          "{run_dir}/media", "--progress_bar", "none", "manim_part2.py", "Part2Benchmark"],
          "artifact": "{run_dir}/media/videos/manim_part2/360p15/Part2Benchmark.mp4",
          "expected": dict(common, frames=714, audio_duration=47.04)}
    engines = [ce]
    for label, python, output_flag in (
        ("wheel-0.5.0", args.released_python, "--video_dir"),
        ("source-current", args.source_python, "--output"),
    ):
        engines.append({"label": label, "python": str(python.absolute()),
            "command": ["{python}", "-m", "fmn_python", "--robot", "fmn_part2.py",
                        "GetPaidThenSell", "--format", "mp4", "--resolution", "640x360",
                        "--fps", "15", "--threads", "4", output_flag, "{output}"],
            "expected": dict(common, frames=FRAMES, audio_duration=47.733333)})
    engines[-1]["build_provenance"] = {
        "source_commit": args.source_commit,
        "cargo_profile_declared": args.source_profile,
        "profile_verified_by_manifest": False,
    }
    return {"cwd": str(args.input_dir.resolve()),
            "sources": [name for name in INPUT_SHA256 if not name.endswith(".wav")],
            "audio": [name for name in INPUT_SHA256 if name.endswith(".wav")],
            "warmups": 1, "repetitions": 3, "timeout_seconds": 900, "engines": engines}


def identity(python, modules, cwd):
    return json.loads(portal.capture([str(python.absolute()), "-c", portal.IDENTITY, *modules], cwd))


def verify_source_wheel(path, installed):
    # Check the actual installed payload, including Python helpers, against the
    # preserved wheel. A checkout SHA alone does not identify installed code.
    roots = {name: Path(installed["modules"][module]["path"]).parent
             for name, module in (("fmn_python", "fmn_python"), ("manimlib", "manimlib.manimlib"))}
    required_modules = {Path(installed["modules"][name]["path"]).resolve()
                        for name in ("fmn_python", "manimlib.manimlib")}
    checked, covered_modules = 0, set()
    with zipfile.ZipFile(path) as wheel_file:
        for member in wheel_file.infolist():
            parts = Path(member.filename).parts
            if member.is_dir() or not parts or parts[0] not in roots:
                continue
            require(".." not in parts, "wheel payload contains parent traversal")
            installed_path = roots[parts[0]].joinpath(*parts[1:])
            with wheel_file.open(member) as source:
                expected = hashlib.file_digest(source, "sha256").hexdigest()
            require(sha256(installed_path) == expected,
                    f"installed source payload differs from wheel: {member.filename}")
            covered_modules.add(installed_path.resolve())
            checked += 1
    require(checked > 1, "source wheel contains no complete portal payload")
    require(required_modules <= covered_modules,
            "source wheel omits the installed native extension or portal module")
    return {"file": str(path.resolve()), "sha256": sha256(path), "payload_files_checked": checked}


def run_checked(command, cwd, directory, timeout=900):
    directory.mkdir()
    status = portal.run_process(command, cwd, directory / "run.log", timeout)
    (directory / "run.json").write_text(json.dumps(status, indent=2) + "\n")
    require(status["exit_code"] == 0, f"correctness command failed; see {directory / 'run.log'}")
    return status


def verify_build(args, destination):
    directory = destination / "build-identity"
    directory.mkdir()
    source = directory / "identity_probe.py"
    source.write_text("from manimlib import Scene, Square\n"
                      "class BuildIdentityProbe(Scene):\n"
                      "    def construct(self):\n"
                      "        self.add(Square())\n")
    output = directory / "identity.png"
    command = [str(args.source_python.absolute()), "-m", "fmn_python", "--robot", str(source),
               "BuildIdentityProbe", "--format", "png", "--resolution", "64x36", "--fps", "15",
               "--threads", "1", "--reproducible", "--output", str(output)]
    run = run_checked(command, args.input_dir, directory / "process", 120)
    manifest = directory / "identity.png.manifest/manifest.txt"
    document = tomllib.loads(manifest.read_text())
    require(document["build_id"] == "git:" + args.source_commit,
            "compiled source build identity differs from the requested clean commit")
    return {"build_id": document["build_id"], "manifest_sha256": sha256(manifest), "run": run}


def decode_hash(artifact, stream, cwd, directory):
    codec = (["-map", "0:v:0", "-c:v", "rawvideo", "-pix_fmt", "rgba"] if stream == "video"
             else ["-map", "0:a:0", "-c:a", "pcm_s16le"])
    command = ["ffmpeg", "-nostdin", "-v", "error", "-xerror", "-i", artifact,
               *codec, "-f", "hash", "-hash", "sha256", "-"]
    run = run_checked(command, cwd, directory)
    line = Path(run["log"]).read_text().strip()
    require(re.fullmatch(r"SHA256=[0-9a-f]{64}", line) is not None,
            f"invalid complete decoded {stream} digest")
    return {"sha256": line.split("=", 1)[1], "run": run}


def native_wav(engine, cwd, directory):
    output = directory / "output.wav"
    command = portal.expand(engine["command"], {"python": engine["python"], "output": output})
    command[command.index("--format") + 1] = "wav"
    run = run_checked(command, cwd, directory)
    digest = sha256(output)
    require(digest == WAV_SHA256, "complete native WAV differs from the published issue #8 oracle")
    with wave.open(str(output), "rb") as audio:
        facts = {"sample_frames": audio.getnframes(), "sample_rate": audio.getframerate(),
                 "channels": audio.getnchannels(), "sample_width": audio.getsampwidth()}
    require(facts == {"sample_frames": 2291200, "sample_rate": 48000,
                      "channels": 2, "sample_width": 2}, "native WAV timeline/layout changed")
    return {"sha256": digest, "wave": facts, "run": run, "render_receipt": receipt(run["log"])}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("archive", "input-dir", "source-python", "released-python", "ce-python",
                 "source-wheel", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    parser.add_argument("--source-commit", required=True, help="full clean commit embedded in the source wheel")
    parser.add_argument("--source-profile", choices=("release", "dev"), required=True,
                        help="declared Cargo profile; preserve the build log because the manifest proves commit, not optimization level")
    parser.add_argument("--profile", action="store_true", help="separate instrumented runs, excluded from means")
    args = parser.parse_args()
    require(re.fullmatch("[0-9a-f]{40}", args.source_commit) is not None, "source commit must be a full SHA")
    args.input_dir = args.input_dir.resolve()
    destination = args.output.resolve()
    destination.mkdir(parents=True, exist_ok=False)
    verification = {"status": "incomplete", "source_commit": args.source_commit,
                    "source_profile_declared": args.source_profile,
                    "profile_verification": "Caller declaration; the embedded manifest verifies the source commit, not the Cargo optimization profile. Preserve the build log.",
                    "measurement_scope": ("Source release comparison" if args.source_profile == "release"
                                          else "Dev diagnostic only; source timings are not release-performance evidence"),
                    "archive_sha256": ARCHIVE_SHA256,
                    "static_reuse_floor": MINIMUM_STATIC_REUSES,
                    "static_reuse_derivation": "602 static-wait frames minus seven potentially changed first frames",
                    "animated_conversion_floor": MINIMUM_ANIMATED_CONVERSIONS,
                    "animated_conversion_scope": "A coarse frozen-movie guard, not proof of every animation; use the installed retained-conversion moving-pixel oracles too.",
                    "timing_policy": "No host-time threshold. Profile runs are excluded from timing means."
                    }
    try:
        verification["inputs"] = verify_inputs(args.archive, args.input_dir)
        source = identity(args.source_python, ["fmn_python", "manimlib.manimlib"], args.input_dir)
        released = identity(args.released_python, ["fmn_python", "manimlib.manimlib"], args.input_dir)
        ce = identity(args.ce_python, ["manim"], args.input_dir)
        require(released["modules"]["manimlib.manimlib"]["sha256"] == RELEASED_NATIVE_SHA256,
                "released interpreter does not contain the published 0.5.0 native wheel")
        require(ce["packages"].get("manim") == "0.21.0", "CE interpreter must contain Manim 0.21.0")
        verification["source_wheel"] = verify_source_wheel(args.source_wheel, source)
        verification["build_identity"] = verify_build(args, destination)
        config = make_config(args)
        config_path = destination / "config.json"
        config_path.write_text(json.dumps(config, indent=2) + "\n")
        evidence = portal.benchmark(config, config_path, destination / "timings", args.profile)
        require(evidence["inputs_unchanged"] and all(row["valid"] for row in evidence["runs"]),
                "original-workload export or full-decode validation failed")
        require(all(summary["complete"] for summary in evidence["summary"].values()),
                "comparison lacks complete warmups and measured samples")
        for index, row in enumerate(evidence["runs"]):
            verify_media_format(row["probe"])
            if row["engine"] != "ce-0.21.0":
                row["render_receipt"] = receipt(row["log"])
            if row["engine"] == "source-current":
                verify_source_work(row["render_receipt"].get("performance", {}))
            row["decoded"] = {stream: decode_hash(row["artifact"], stream, args.input_dir,
                               destination / f"decode-{index}-{stream}") for stream in ("video", "audio")}
        verify_repeatability(evidence["runs"])
        verification["native_wav"] = {engine["label"]: native_wav(engine, args.input_dir,
                                      destination / (engine["label"] + "-wav"))
                                      for engine in config["engines"] if engine["label"] != "ce-0.21.0"}
        require(verification["inputs"] == verify_inputs(args.archive, args.input_dir),
                "workload inputs changed during correctness checks")
        verification["status"] = "passed"
        evidence["issue8_verification"] = verification
        (destination / "results.json").write_text(json.dumps(evidence, indent=2) + "\n")
        print(json.dumps({"summary": evidence["summary"], "verification": "passed",
                          "source_profile_declared": args.source_profile,
                          "measurement_scope": verification["measurement_scope"]}, indent=2))
        return 0
    except (OSError, ValueError, KeyError, zipfile.BadZipFile) as error:
        verification.update(status="failed", error=str(error))
        raise
    finally:
        (destination / "verification.json").write_text(json.dumps(verification, indent=2) + "\n")


if __name__ == "__main__":
    raise SystemExit(main())
