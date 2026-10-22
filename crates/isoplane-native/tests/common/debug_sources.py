"""Helpers the debugger scenarios share: source comparison across filesystem aliases and the
stop event they wait for."""
from pathlib import Path
from os.path import samestat


def same_source(actual, expected):
    expected_stat = Path(expected).stat()
    if not actual:
        return False
    try:
        actual_stat = Path(actual).stat()
    except FileNotFoundError:
        # Dependency symbols can name files on the SDK build host.
        return False
    return samestat(actual_stat, expected_stat)


def stop_event(item):
    """A debugger stop. The driver fails a wait for one when the request that should pause completes."""
    return item.get("event") == "stopped"
