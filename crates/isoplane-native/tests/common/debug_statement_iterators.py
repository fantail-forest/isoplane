"""Jump across and repeat iterator yields, preserving values and disposal."""
from pathlib import Path
from debug_sources import stop_event

EXPECTED_RESPONSE = b"11,11,111,11,11,111:True:True"


def locations(source):
    return {key: next(index + 1 for index, text in enumerate(Path(source).read_text().splitlines()) if key in text)
            for key in ("syncItem += 10;", "syncItem += 1000;", "// SYNC_FIRST_YIELD", "// SYNC_SECOND_YIELD",
                        "asyncItem += 10;", "asyncItem += 1000;", "// ASYNC_FIRST_YIELD", "// ASYNC_SECOND_YIELD")}


def configure(source, send, response):
    lines = locations(source)
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [
        {"line": lines[key]} for key in ("syncItem += 10;", "syncItem += 1000;", "asyncItem += 10;", "asyncItem += 1000;")]}))


def exercise(source, send, response, until):
    lines = locations(source)

    def stop():
        event = until(stop_event)
        thread = event["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        return thread, frame

    def target(key):
        result = response(send("gotoTargets", {"source": {"path": source}, "line": lines[key]}))["body"]["targets"]
        assert len(result) == 1, f"the active iterator must offer {key}: {result}"
        return result[0]["id"]

    for name, marker in [("syncItem", "SYNC"), ("asyncItem", "ASYNC")]:
        thread, frame = stop()
        assert frame["line"] == lines[name + " += 10;"], frame
        response(send("next", {"threadId": thread}))
        thread, frame = stop()
        assert frame["line"] == lines[f"// {marker}_FIRST_YIELD"], frame
        saved = response(send("evaluate", {"expression": name, "frameId": frame["id"]}))["body"]["result"]
        assert saved == "11", "the iterator's hoisted local must remain inspectable"
        forward = target(f"// {marker}_SECOND_YIELD")
        response(send("goto", {"threadId": thread, "targetId": forward}))
        thread, frame = stop()
        assert frame["line"] == lines[f"// {marker}_SECOND_YIELD"], frame
        response(send("continue", {"threadId": thread}))
        thread, frame = stop()
        assert frame["line"] == lines[name + " += 1000;"], frame
        expired = send("goto", {"threadId": thread, "targetId": forward})
        rejected = until(lambda item: item.get("type") == "response" and item.get("request_seq") == expired)
        assert not rejected.get("success"), "a yield must expire targets from the preceding MoveNext invocation"
        response(send("goto", {"threadId": thread, "targetId": target(f"// {marker}_FIRST_YIELD")}))
        thread, frame = stop()
        assert frame["line"] == lines[f"// {marker}_FIRST_YIELD"], frame
        response(send("continue", {"threadId": thread}))
