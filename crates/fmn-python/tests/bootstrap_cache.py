"""Native bootstrap-cache acceptance, sharing the portal's owner-thread teardown."""
from contextlib import contextmanager
import hashlib
import importlib
import importlib.util
from pathlib import Path
import sys


@contextmanager
def bootstrap_source(source, mode):
    from fmn_python import bootstrap_loader

    source = Path(source)
    cache = Path(importlib.util.cache_from_source(str(source)))
    assert mode in {"embedded", "cold", "warm"}
    assert source.is_file() == (mode != "embedded")
    assert cache.is_file() == (mode == "warm")
    previous_source = bootstrap_loader._SOURCE
    previous_writer = sys.dont_write_bytecode
    previous_compiler = bootstrap_loader._VerifiedSourceLoader.source_to_code

    def reject_compile(*args, **kwargs):
        raise AssertionError("warm native portal initialization recompiled the bootstrap")

    try:
        bootstrap_loader._SOURCE = source
        sys.dont_write_bytecode = False
        if mode == "warm":
            bootstrap_loader._VerifiedSourceLoader.source_to_code = reject_compile
        yield
        assert cache.is_file() == (mode != "embedded")
    finally:
        bootstrap_loader._SOURCE = previous_source
        sys.dont_write_bytecode = previous_writer
        bootstrap_loader._VerifiedSourceLoader.source_to_code = previous_compiler


def render_bootstrap(destination):
    import manimlib as m

    native = getattr(m, "_native", m)
    assert "_FMN_MODULE" not in vars(native)
    assert importlib.import_module("manimlib.mobject.geometry").Square is m.Square
    assert importlib.import_module("manimlib.scene.scene").Scene is m.Scene

    class BootstrapScene(m.Scene):
        def construct(self):
            square = m.Square(side_length=1.0).set_color(m.BLUE)
            self.add(square)
            self.play(square.animate.shift(m.RIGHT), run_time=0.5, rate_func=m.linear)

    result = BootstrapScene().render(
        str(destination), format="y4m", resolution=(64, 36), fps=4, threads=1,
    )
    payload = Path(destination).read_bytes()
    assert result.frame_count >= 2
    assert payload.startswith(b"YUV4MPEG2 ")
    return (result.frame_count, len(payload), hashlib.sha256(payload).hexdigest(),
            native._FMN_BOOTSTRAP_IDENTITY["code_sha256"])
