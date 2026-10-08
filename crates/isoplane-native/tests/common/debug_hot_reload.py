"""Apply source edits through DAP and jump back within the same HTTP invocation."""
from pathlib import Path

EXPECTED_RESPONSE = b"715"


def configure(source, send, response):
    response(send("setExceptionBreakpoints", {"filters": ["uncaught"]}))


def exercise(source, send, response, until):
    def stop():
        event = until(lambda item: item.get("event") == "stopped")
        thread = event["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        assert "Handle" in frame["name"], frame
        return thread, frame

    def value(frame):
        return response(send("evaluate", {"expression": "value", "frameId": frame["id"], "context": "watch"}))["body"]["result"]

    thread, frame = stop()
    assert value(frame) == "11"
    unknown = send("isoplane/hotReload", {"sources": [{"path": str(Path(source).with_name("Unrelated.cs")), "text": "class Unrelated {}"}]})
    rejected = until(lambda item: item.get("type") == "response" and item.get("request_seq") == unknown)
    assert not rejected.get("success"), "unrelated source must not terminate the compiler session"
    original = Path(source).read_text()
    invalid = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": original + "\n#error invalid_edit\n"}]}))["body"]
    assert invalid["appliedModules"] == 0 and invalid["diagnostics"], invalid
    assert value(frame) == "11", "compiler diagnostics must preserve the pause and its handles"
    for suffix, before, after in [(".updated.cs.in", "11", "111"), (".updated-again.cs.in", "111", "311")]:
        edited = Path(source).with_suffix(suffix).read_text()
        changed = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": edited}]}))["body"]
        assert changed["appliedModules"] == 1 and not changed["diagnostics"] and not changed["restartRequired"], changed
        thread, current = stop()
        stale = send("scopes", {"frameId": frame["id"]})
        rejected = until(lambda item: item.get("type") == "response" and item.get("request_seq") == stale)
        assert not rejected.get("success"), "applying changes must invalidate previously presented frame handles"
        assert value(current) == before
        line = next(index + 1 for index, text in enumerate(edited.splitlines()) if "HOT_RELOAD_BODY" in text)
        targets = response(send("gotoTargets", {"source": {"path": source}, "line": line}))["body"]["targets"]
        assert len(targets) == 1, targets
        response(send("goto", {"threadId": thread, "targetId": targets[0]["id"]}))
        thread, frame = stop()
        assert frame["line"] == line and value(frame) == before
        response(send("continue", {"threadId": thread}))
        thread, frame = stop()
        assert value(frame) == after, "jumping back must execute the edited statement with the existing local"
    start, end = edited.index("public static string Handle()"), edited.index("public static string HandleTry()")
    expanded = edited[:start] + edited[start:end].replace("return value.ToString();", "return (value + 1).ToString();") + edited[end:]
    changed = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": expanded}]}))["body"]
    assert changed["appliedModules"] == 1 and not changed["diagnostics"], changed
    thread, frame = stop()
    targets = response(send("gotoTargets", {"source": {"path": source}, "line": line}))["body"]["targets"]
    assert len(targets) == 1, targets
    response(send("goto", {"threadId": thread, "targetId": targets[0]["id"]}))
    thread, frame = stop()
    assert value(frame) == "311", "expanding frame storage must preserve existing locals"
    response(send("continue", {"threadId": thread}))
    thread, frame = stop()
    assert value(frame) == "511", "the expanded invocation must execute its updated body"
    start, end = expanded.index("public static string Handle()"), expanded.index("public static string HandleTry()")
    captured = expanded[:start] + expanded[start:end].replace("int value = 1;", "int value = 1;\n        string held = new string('x', 3);\n        ref int alias = ref value;") + expanded[end:]
    changed = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": captured}]}))["body"]
    assert changed["appliedModules"] == 1 and not changed["diagnostics"], changed
    thread, frame = stop()
    line = next(index + 1 for index, text in enumerate(captured.splitlines()) if "string held =" in text)
    targets = response(send("gotoTargets", {"source": {"path": source}, "line": line}))["body"]["targets"]
    assert len(targets) == 1, targets
    response(send("goto", {"threadId": thread, "targetId": targets[0]["id"]}))
    thread, frame = stop()
    assert value(frame) == "511"
    response(send("continue", {"threadId": thread}))
    thread, frame = stop()
    assert value(frame) == "711"
    retained = captured.replace("return (value + 1).ToString();", 'GC.Collect(); // HOT_RELOAD_GROWTH_GC\n        string more = held + "!";\n        GC.Collect();\n        return (alias + more.Length).ToString();')
    changed = response(send("isoplane/hotReload", {"sources": [{"path": source, "text": retained}]}))["body"]
    assert changed["appliedModules"] == 1 and not changed["diagnostics"], changed
    thread, frame = stop()
    line = next(index + 1 for index, text in enumerate(retained.splitlines()) if "HOT_RELOAD_GROWTH_GC" in text)
    targets = response(send("gotoTargets", {"source": {"path": source}, "line": line}))["body"]["targets"]
    assert len(targets) == 1, targets
    response(send("goto", {"threadId": thread, "targetId": targets[0]["id"]}))
    thread, frame = stop()
    assert value(frame) == "711", "expansion must retain the local addressed by the existing byref"
    # Do not evaluate held: a debugger object handle could mask a missing GC root.
    response(send("continue", {"threadId": thread}))
