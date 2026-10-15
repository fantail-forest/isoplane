"""Name a framework async caller that has no symbols after the application resumes."""
from pathlib import Path
from debug_sources import same_source

EXPECTED_RESPONSE = b"4"


def configure(source, send, response):
    line = next(index + 1 for index, text in enumerate(Path(source).read_text().splitlines()) if "Written += buffer.Length;" in text)
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [{"line": line}]}))


def exercise(source, send, response, until):
    stopped = until(lambda message: message.get("event") == "stopped")["body"]
    assert stopped["reason"] == "breakpoint", f"the resumed write must stop at its breakpoint: {stopped}"
    frames = response(send("stackTrace", {"threadId": stopped["threadId"]}))["body"]["stackFrames"]
    names = [frame["name"] for frame in frames]
    assert "DelayedStream.WriteAsync" in names[0] and same_source(frames[0]["source"]["path"], source), f"the debugger must stop in the application's write: {names}"
    copy = next((index for index, name in enumerate(names) if name.startswith("System.IO.Stream.CopyToAsync")), None)
    assert copy is not None and names[copy].endswith(" [awaiting]"), f"the framework method awaiting the write must appear by name after the await: {names}"
    assert frames[copy].get("source") is None and frames[copy]["line"] == 0, f"a framework caller without symbols must have no source location: {frames[copy]}"
    handler = next((index for index, name in enumerate(names) if name.endswith("Application.Handle [awaiting]")), None)
    assert handler is not None and copy < handler, f"the application's handler must await the framework caller: {names}"
    response(send("continue", {"threadId": stopped["threadId"]}))
