"""Apply edits during execution without introducing or consuming debugger stops."""
from pathlib import Path

EXPECTED_RESPONSE = b'42:1:1:True:{"Value":1,"Added":2}'


def result_line(source):
    return next(index + 1 for index, line in enumerate(Path(source).read_text().splitlines()) if "RUNNING_RESULT" in line)


def configure(source, send, response):
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [{"line": result_line(source), "condition": "value == 42"}]}))


def exercise(source, send, response, until):
    stopped = until(lambda item: item.get("event") == "stopped")
    thread = stopped["body"]["threadId"]
    response(send("continue", {"threadId": thread}))
    original = Path(source).read_text()
    invalid = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": original + "\n#error running_invalid_edit\n"}]}))["body"]
    assert invalid["appliedModules"] == 0 and invalid["diagnostics"], invalid
    edited = original.replace("Revision() => 0;", "Revision() => 42;")
    edited = edited.replace("public int Value => 1; // NOTIFICATION_PROPERTY", "public int Value => 1; public int Added => 2; // NOTIFICATION_PROPERTY")
    applied = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": edited}]}))["body"]
    assert applied["appliedModules"] == 1 and not applied["diagnostics"] and not applied["restartRequired"], applied
    until(lambda item: item.get("event") == "output" and "notification-fixture-warning" in item.get("body", {}).get("output", ""))
    stopped = until(lambda item: item.get("event") == "stopped")
    assert stopped["body"]["reason"] == "breakpoint", stopped
    thread = stopped["body"]["threadId"]
    frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
    assert frame["line"] == result_line(source), frame
    value = response(send("evaluate", {"frameId": frame["id"], "expression": "value"}))["body"]["result"]
    assert value == "42", value
    response(send("continue", {"threadId": thread}))
