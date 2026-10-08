"""Adopt added locals in an async or protected body without losing live state."""
from pathlib import Path
import sys

ASYNC = sys.argv[-2] == "edit-and-continue-async"
EXPECTED_RESPONSE = b"35" if ASYNC else b"1035"


def configure(source, send, response):
    response(send("setExceptionBreakpoints", {"filters": ["uncaught"]}))


def exercise(source, send, response, until):
    def stop():
        event = until(lambda item: item.get("event") == "stopped")
        thread = event["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        return thread, frame

    def value(frame):
        return response(send("evaluate", {"expression": "value", "frameId": frame["id"], "context": "watch"}))["body"]["result"]

    thread, frame = stop()
    assert value(frame) == "11"
    original = Path(source).read_text()
    if ASYNC:
        marker = "await Task.Delay(1); // HOT_RELOAD_AWAIT"
        edited = original.replace(marker, 'string added = new string(\'x\', 2); // HOT_RELOAD_ADDED\n        ' + marker)
        start = edited.index("public static async Task<string> HandleAsync()")
        edited = edited[:start] + edited[start:].replace("value += 10;", "value += 10 + added.Length;")
    else:
        marker = "value += 10; // HOT_RELOAD_TRY"
        edited = original.replace(marker, 'string added = new string(\'x\', 2); // HOT_RELOAD_ADDED\n            value += 10 + added.Length; // HOT_RELOAD_TRY')
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
