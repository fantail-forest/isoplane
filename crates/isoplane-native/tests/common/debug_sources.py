"""Helpers the debugger scenarios share: source comparison across filesystem aliases and the
stop event they wait for."""
from pathlib import Path


def same_source(actual, expected):
    return bool(actual) and Path(actual).samefile(expected)


def stop_event(item):
    """A debugger stop. The driver fails a wait for one when the request that should pause completes."""
    return item.get("event") == "stopped"
