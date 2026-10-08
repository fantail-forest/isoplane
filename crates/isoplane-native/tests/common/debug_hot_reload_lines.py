"""Keep paused frames and jump targets aligned after source-only line changes."""
from pathlib import Path

EXPECTED_RESPONSE = b"231"


def configure(source, send, response):
    response(send("setExceptionBreakpoints", {"filters": ["uncaught"]}))


def exercise(source, send, response, until):
    def stop():
        event = until(lambda item: item.get("event") == "stopped")
        thread = event["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        return thread, frame

    thread, frame = stop()
    original = Path(source).read_text()
    original_line = frame["line"]
    for blank_lines, increment, expected_value in [(2, 10, "11"), (4, 10, "21"), (4, 100, "31"), (6, 100, "131")]:
        edited = "\n" * blank_lines + original.replace("value += 10; // HOT_RELOAD_BODY", f"value += {increment}; // HOT_RELOAD_BODY")
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
