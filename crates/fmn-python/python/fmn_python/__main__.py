"""Console composition for ``fmn-python`` and ``python -m fmn_python``."""

import json
import sys

from . import _ensure_exclusive_manimlib_namespace, _ManimlibNamespaceCollision


def _emit_namespace_collision(error):
    message = str(error)[:1024]
    if "--robot" in sys.argv[1:]:
        payload = {
            "schema": "fmn-python.cli",
            "version": 1,
            "kind": "namespace-collision",
            "status": "error",
            "exit": {"code": 4, "identity": "capability"},
            "message": message,
            "providers": list(error.providers),
        }
        print(json.dumps(payload, sort_keys=True, separators=(",", ":")))
    else:
        print(
            f"fmn-python: capability/namespace-collision: {message}",
            file=sys.stderr,
        )
    return 4


def main():
    try:
        _ensure_exclusive_manimlib_namespace()
    except _ManimlibNamespaceCollision as error:
        return _emit_namespace_collision(error)

    from manimlib import _native

    return _native._console_main()


if __name__ == "__main__":
    raise SystemExit(main())
