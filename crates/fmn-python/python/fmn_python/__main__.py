"""Console composition for ``fmn-python`` and ``python -m fmn_python``."""


def main():
    from manimlib import _native

    return _native._console_main()


if __name__ == "__main__":
    raise SystemExit(main())
