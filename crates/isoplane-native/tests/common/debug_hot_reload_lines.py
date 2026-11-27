"""Keep paused frames and jump targets aligned after source-only line changes."""
from pathlib import Path
from debug_sources import stop_event

EXPECTED_RESPONSE = b"231"


def configure(source, send, response):
    response(send("setExceptionBreakpoints", {"filters": ["uncaught"]}))


def exercise(source, send, response, until):
    def stop():
        event = until(stop_event)
        thread = event["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        return thread, frame

    thread, frame = stop()
    original = Path(source).read_text()
    incremented = Path(source).with_suffix(".increment.cs.in").read_text()
    original_line = frame["line"]
    # Leading blank lines move every statement down without changing one: the remapping under test.
    for blank_lines, text, expected_value in [(2, original, "11"), (4, original, "21"), (4, incremented, "31"), (6, incremented, "131")]:
        edited = "\n" * blank_lines + text
        changed = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": edited}]}))["body"]
        assert changed["appliedModules"] == 1 and not changed["diagnostics"] and not changed["restartRequired"], changed
        thread, frame = stop()
        assert frame["line"] == original_line + blank_lines, frame
        line = next(index + 1 for index, text in enumerate(edited.splitlines()) if "HOT_RELOAD_BODY" in text)
        targets = response(send("gotoTargets", {"source": {"path": source}, "line": line}))["body"]["targets"]
        assert len(targets) == 1, targets
        response(send("goto", {"threadId": thread, "targetId": targets[0]["id"]}))
        thread, frame = stop()
        assert frame["line"] == line, frame
        value = response(send("evaluate", {"expression": "value", "frameId": frame["id"], "context": "watch"}))["body"]["result"]
        assert value == expected_value, "line remapping must retain the live invocation"
        response(send("continue", {"threadId": thread}))
        thread, frame = stop()
    response(send("continue", {"threadId": thread}))
