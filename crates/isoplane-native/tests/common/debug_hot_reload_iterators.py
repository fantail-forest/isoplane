"""Edit paused iterator bodies, retaining hoisted locals through yield and await."""
from pathlib import Path
from debug_sources import stop_event

EXPECTED_RESPONSE = b"101,1101,101,1101:True:True"


def documents(source):
    """The document each iterator is declared in, beside the one holding their caller."""
    return [(name, str(Path(source).with_name(f"{document}.cs")))
            for name, document in [("syncItem", "SyncIterator"), ("asyncItem", "AsyncIterator")]]


def marked_line(text, marker):
    return next(index + 1 for index, line in enumerate(text.splitlines()) if marker in line)


def configure(source, send, response):
    for _, document in documents(source):
        text = Path(document).read_text()
        response(send("setBreakpoints", {"source": {"path": document}, "breakpoints": [
            {"line": marked_line(text, marker)} for marker in ("_EDIT", "_RESUME")]}))


def exercise(source, send, response, until):
    def stop():
        event = until(stop_event)
        thread = event["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        return thread, frame

    def value(frame, name):
        return response(send("evaluate", {"expression": name, "frameId": frame["id"]}))["body"]["result"]

    for name, document in documents(source):
        edited = Path(document).read_text()
        for phase, before, after in [("edit", "1", "101"), ("resume", "101", "1101")]:
            thread, frame = stop()
            line = marked_line(edited, f"_{phase.upper()}")
            assert frame["line"] == line and value(frame, name) == before, frame
            edited = Path(document).with_suffix(f".{phase}.cs.in").read_text()
            result = response(send("isoplane/hotReload", {"sources": [{"path": document, "text": edited}]}))["body"]
            assert result["appliedModules"] == 1 and not result["diagnostics"] and not result["restartRequired"], result
            thread, frame = stop()
            assert frame["line"] == line, "applying an edit must retain the paused statement, even when another statement has identical text"
            assert value(frame, name) == before, "applying an edit must preserve the iterator's saved local"
            response(send("next", {"threadId": thread}))
            thread, frame = stop()
            assert frame["line"] == line + 1 and value(frame, name) == after, frame
            response(send("continue", {"threadId": thread}))
