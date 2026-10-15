"""Keep the live rethrow continuation usable when Roslyn rejects a handler edit."""
from pathlib import Path
from debug_sources import stop_event

EXPECTED_RESPONSE = b"11"


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
    edited = original.replace("value += 10; // HOT_RELOAD_UNCHANGED", "value += 20; // HOT_RELOAD_UNCHANGED")
    changed = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": edited}]}))["body"]
    assert changed["appliedModules"] == 0 and changed["diagnostics"] and changed["restartRequired"], changed
    value = response(send("evaluate", {"expression": "value", "frameId": frame["id"], "context": "watch"}))["body"]["result"]
    assert value == "11", "a rejected edit must leave the original handler and its locals usable"
    response(send("continue", {"threadId": thread}))
