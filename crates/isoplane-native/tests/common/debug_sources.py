"""Compare debugger source locations across filesystem aliases."""
from pathlib import Path


def same_source(actual, expected):
    return bool(actual) and Path(actual).samefile(expected)
