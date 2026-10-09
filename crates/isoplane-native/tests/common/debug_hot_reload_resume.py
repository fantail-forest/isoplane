"""Adopt compiler edits through ordinary stepping and continuing, without a jump."""
from pathlib import Path

EXPECTED_RESPONSE = b"1101"


def configure(source, send, response):
    line = next(index + 1 for index, text in enumerate(Path(source).read_text().splitlines()) if "HOT_RELOAD_BODY" in text)
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [{"line": line}]}))


def exercise(source, send, response, until):
    def stop():
        event = until(lambda item: item.get("event") == "stopped")
        thread = event["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        return thread, frame

    def value(frame):
        return response(send("evaluate", {"expression": "value", "frameId": frame["id"]}))["body"]["result"]

    def apply(text):
        result = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": text}]}))["body"]
        assert result["appliedModules"] == 1 and not result["diagnostics"] and not result["restartRequired"], result
        return stop()

    thread, frame = stop()
    assert value(frame) == "1"
    original_line = frame["line"]
    edited = "\n\n" + Path(source).read_text().replace("value += 10; // HOT_RELOAD_BODY", "value += 100; // HOT_RELOAD_BODY")
    thread, frame = apply(edited)
    assert frame["line"] == original_line + 2 and value(frame) == "1", frame
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": []}))
    response(send("next", {"threadId": thread}))
    thread, frame = stop()
    assert frame["line"] == original_line + 3 and value(frame) == "101", frame
    start, end = edited.index("public static string Handle()"), edited.index("public static string HandleTry()")
    edited = edited[:start] + edited[start:end].replace("return value.ToString();", "return (value + 10).ToString();") + edited[end:]
    thread, frame = apply(edited)
    assert value(frame) == "101", "remapping must not repeat the preceding increment"
    response(send("continue", {"threadId": thread}))
    thread, frame = stop()
    assert value(frame) == "101", "Debugger.Break must execute once after continuing from its statement"
    edited = edited.replace("return (value + 10).ToString();", "return (value + 1000).ToString();")
    thread, frame = apply(edited)
    assert value(frame) == "101"
    response(send("next", {"threadId": thread}))
    thread, frame = stop()
    return_line = next(index + 1 for index, text in enumerate(edited.splitlines()) if "return (value + 1000).ToString();" in text)
    assert frame["line"] == return_line, "remapping a completed Debugger.Break must advance instead of invoking it again"
    response(send("continue", {"threadId": thread}))
