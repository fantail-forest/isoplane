"""Resume construction into edited callers without creating the object twice."""
from pathlib import Path
from debug_sources import stop_event

EXPECTED_RESPONSE = b"3123:7:2"


def configure(source, send, response):
    pass


def exercise(source, send, response, until):
    def stop():
        return until(stop_event)["body"]["threadId"]

    text = Path(source).read_text()
    for marker, value in [("CLASS_CONSTRUCTION_EDIT", 1000), ("VALUE_CONSTRUCTION_EDIT", 2000)]:
        thread = stop()
        text = text.replace(f"result += 10; // {marker}", f"int added{value} = {value}; result += added{value}; // {marker}")
        result = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": text}]}))["body"]
        assert result["appliedModules"] == 1 and not result["diagnostics"] and not result["restartRequired"], result
        thread = stop()
        response(send("stepOut", {"threadId": thread}))
        thread = stop()
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        assert frame["name"].endswith("Callers.Construct"), frame
        local = response(send("evaluate", {"expression": "preserved", "frameId": frame["id"]}))["body"]["result"]
        assert local == "7", frame
        response(send("continue", {"threadId": thread}))
