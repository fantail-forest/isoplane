"""Edit paused iterator bodies, retaining hoisted locals through yield and await."""
from pathlib import Path

EXPECTED_RESPONSE = b"101,1101,101,1101:True:True"


def configure(source, send, response):
    lines = Path(source).read_text().splitlines()
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [
        {"line": index + 1} for index, line in enumerate(lines) if "_EDIT" in line or "_RESUME" in line]}))


def exercise(source, send, response, until):
    edited = Path(source).read_text()

    def stop():
        event = until(lambda item: item.get("event") == "stopped")
        thread = event["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        return thread, frame

    def value(frame, name):
        return response(send("evaluate", {"expression": name, "frameId": frame["id"]}))["body"]["result"]

    for name, marker in [("syncItem", "SYNC"), ("asyncItem", "ASYNC")]:
        for phase, before, after, change in [("EDIT", "1", "101", (10, 100)), ("RESUME", "101", "1101", (100, 1000))]:
            thread, frame = stop()
            line = next(index + 1 for index, text in enumerate(edited.splitlines()) if f"// {marker}_{phase}" in text)
            assert frame["line"] == line and value(frame, name) == before, frame
            old, new = change
            edited = edited.replace(f"{name} += {old}; // {marker}_{phase}", f"{name} += {new}; // {marker}_{phase}")
            result = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": edited}]}))["body"]
            assert result["appliedModules"] == 1 and not result["diagnostics"] and not result["restartRequired"], result
            thread, frame = stop()
            assert frame["line"] == line, "applying an edit must retain the paused statement, even when another statement has identical text"
            assert value(frame, name) == before, "applying an edit must preserve the iterator's saved local"
            response(send("next", {"threadId": thread}))
            thread, frame = stop()
            assert frame["line"] == line + 1 and value(frame, name) == after, frame
            response(send("continue", {"threadId": thread}))
