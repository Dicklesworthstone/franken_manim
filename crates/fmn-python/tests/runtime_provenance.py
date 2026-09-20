"""Installed-wheel runtime provenance and native output acceptance.

Uses actual installed payloads, Scene rendering and native manifest publication.
No injected inventory, monkeypatch, or renderer double satisfies these cases.
"""
from pathlib import Path
import tempfile

import manimlib as ml
from fmn_python.runtime_identity import capture_runtime


_SOURCE = {"runtime_provenance.py": Path(__file__).read_bytes()}
_ROOT = Path(tempfile.mkdtemp(prefix="fmn-runtime-provenance-native-"))


def test_loaded_runtime_has_content_identities():
    snapshot = capture_runtime(ml)
    identities = snapshot.identities
    assert set(identities) == {"cpython", "abi", "wheel", "numpy"}
    assert all("installed-payload-sha256=" in identities[name] for name in ("cpython", "wheel", "numpy"))
    assert identities["abi"].startswith("cpython-abi-sha256=")
    snapshot.verify()


def test_both_scene_front_doors_publish_real_png_and_manifest():
    class Static(ml.Scene):
        def construct(self):
            self.add(ml.Dot().shift(ml.RIGHT))

    rendered = Static().render(_ROOT / "render.png", reproducible=True,
                               sources=_SOURCE, resolution=(96, 54), fps=4, threads=1)
    scene = ml.Scene()
    with scene.render_session(_ROOT / "session.png", reproducible=True, sources=_SOURCE,
                              resolution=(96, 54), fps=4, threads=4) as session:
        scene.add(ml.Dot().shift(ml.RIGHT))
    imperative = session.result
    for result in (rendered, imperative):
        assert result.certified and result.closure_digest and result.frame_count == 1
        assert result.manifest.is_file()
        assert result.destination.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
    assert rendered.destination.read_bytes() == imperative.destination.read_bytes()
    assert "installed-payload-sha256=" in session.runtime_identities["wheel"]


def test_false_runtime_labels_refuse_without_native_publication():
    destination = _ROOT / "refused.png"
    scene = ml.Scene()
    try:
        with scene.render_session(destination, reproducible=True, sources=_SOURCE,
                                  runtime_identities={"wheel": "unverified build"}):
            raise AssertionError("runtime refusal occurred too late")
    except Exception as error:
        assert "supplied runtime identities" in str(error), error
    else:
        raise AssertionError("arbitrary runtime identity was accepted")
    assert not destination.exists()
    assert not destination.with_name(destination.name + ".manifest").exists()


assert getattr(ml, "__franken_manim__", False), "requires the installed native FrankenManim portal"
_cases = sorted((name, case) for name, case in globals().items()
                if name.startswith("test_") and callable(case))
assert len(_cases) == 3, "runtime provenance native acceptance inventory changed"
for _name, _case in _cases:
    _case()
    print("runtime provenance native acceptance passed:", _name)
print("retaining native runtime provenance outputs:", _ROOT)
