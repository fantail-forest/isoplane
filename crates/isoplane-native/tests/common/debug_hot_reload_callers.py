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
    for text in [Path(source).with_suffix(suffix).read_text() for suffix in (".edited.cs.in", ".edited-again.cs.in")]:
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
