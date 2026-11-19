"""Adopt added locals in an async or protected body without losing live state."""
from pathlib import Path
import sys
from debug_sources import stop_event

ASYNC = sys.argv[-2] == "edit-and-continue-async"
EXPECTED_RESPONSE = b"35" if ASYNC else b"1035"


def configure(source, send, response):
    response(send("setExceptionBreakpoints", {"filters": ["uncaught"]}))


def exercise(source, send, response, until):
    def stop():
        event = until(stop_event)
        thread = event["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        return thread, frame

    def value(frame):
        return response(send("evaluate", {"expression": "value", "frameId": frame["id"], "context": "watch"}))["body"]["result"]

    thread, frame = stop()
    assert value(frame) == "11"
    edited = Path(source).with_suffix(".added-async.cs.in" if ASYNC else ".added-try.cs.in").read_text()
    changed = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": edited}]}))["body"]
    assert changed["appliedModules"] == 1 and not changed["diagnostics"] and not changed["restartRequired"], changed
    thread, frame = stop()
    for before, after in [("11", "23"), ("23", "35")]:
        line = next(index + 1 for index, text in enumerate(edited.splitlines()) if "HOT_RELOAD_ADDED" in text)
        targets = response(send("gotoTargets", {"source": {"path": source}, "line": line}))["body"]["targets"]
        assert len(targets) == 1, targets
        response(send("goto", {"threadId": thread, "targetId": targets[0]["id"]}))
        thread, frame = stop()
        assert value(frame) == before, "adopting new locals must preserve the existing value"
        response(send("continue", {"threadId": thread}))
        thread, frame = stop()
        assert value(frame) == after, "reentry must initialize and use the new local on each pass"
        length = response(send("evaluate", {"expression": "added.Length", "frameId": frame["id"], "context": "watch"}))["body"]["result"]
        assert length == "2", "a newly added local must be available to watches after resuming"
        response(send("setExpression", {"expression": "added", "value": '\"abcd\"', "frameId": frame["id"]}))
        length = response(send("evaluate", {"expression": "added.Length", "frameId": frame["id"], "context": "watch"}))["body"]["result"]
        assert length == "4", "watch assignment must update a newly added local"
    response(send("continue", {"threadId": thread}))
