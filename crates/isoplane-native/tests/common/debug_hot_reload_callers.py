"""Adopt caller edits after nested calls return without replaying side effects."""
from pathlib import Path
from debug_sources import stop_event

EXPECTED_RESPONSE = b"1326:7:1"


def configure(source, send, response):
    pass


def exercise(source, send, response, until):
    def stop():
        event = until(stop_event)
        return event["body"]["threadId"]

    thread = stop()
    original = Path(source).read_text()
    edited = original.replace("result += 10; // OUTER_EDIT", "int added = 1000; result += added; // OUTER_EDIT")
    edited = edited.replace("result += 10; // INNER_EDIT", "int added = 100; result += added; // INNER_EDIT")
    for text in [edited, edited.replace("int added = 100;", "int added = 200;")]:
        result = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": text}]}))["body"]
        assert result["appliedModules"] == 1 and not result["diagnostics"] and not result["restartRequired"], result
        thread = stop()
    response(send("stepOut", {"threadId": thread}))
    thread = stop()
    frames = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"]
    frame = frames[0]
    assert frame["name"].endswith("Callers.Outer"), frame
    value = response(send("evaluate", {"expression": "preserved", "frameId": frame["id"]}))["body"]["result"]
    assert value == "3", "stepping out must preserve the caller's existing local"
    parent = next(frame for frame in frames if frame["name"].endswith("Callers.Handle"))
    value = response(send("evaluate", {"expression": "preserved", "frameId": parent["id"]}))["body"]["result"]
    assert value == "7", "each caller must retain its own frame identity and local values"
    response(send("next", {"threadId": thread}))
    thread = stop()
    frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
    assert frame["name"].endswith("Callers.Outer"), frame
    response(send("continue", {"threadId": thread}))
