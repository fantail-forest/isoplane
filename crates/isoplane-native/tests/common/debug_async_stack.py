"""Preserve logical callers and saved locals across host I/O."""
from pathlib import Path
from debug_sources import same_source

EXPECTED_RESPONSE = b"63"


def source_line(source, text):
    return next(index + 1 for index, line in enumerate(Path(source).read_text().splitlines()) if text in line)


def configure(source, send, response):
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [{"line": source_line(source, "int total =")}]}))


def inspect(source, send, response, thread, until):
    frames = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"]
    expected = [("Inner", "int total =", "answer", "22"),
                ("Outer", "int result = await Inner", "outer", "22"),
                ("Handle", "int result = await Outer", "seed", "21")]
    application = [frame for frame in frames if same_source(frame.get("source", {}).get("path"), source)]
    assert len(application) == 3, "the resumed async stack must retain Inner, Outer and Handle across host parking"
    for frame, (method, line, local, value) in zip(application, expected):
        assert method in frame["name"], f"the async frame must name its original C# method {method}"
        assert frame["line"] == source_line(source, line), f"{method} must point at its current statement or suspended await"
        scopes = response(send("scopes", {"frameId": frame["id"]}))["body"]["scopes"]
        scope = next(scope["variablesReference"] for scope in scopes if scope["name"] == "Locals")
        values = response(send("variables", {"variablesReference": scope}))["body"]["variables"]
        assert any(item["name"] == local and item["value"] == value for item in values), f"{method} must expose its own saved local {local}"
        if method == "Outer":
            for command, arguments in [("evaluate", {"expression": "answer", "context": "watch"}),
                                       ("setExpression", {"expression": "answer", "value": "999"}),
                                       ("setVariable", {"variablesReference": scope, "name": "outer", "value": "answer"})]:
                identifier = send(command, {"frameId": frame["id"], **arguments})
                rejected = until(lambda message: message.get("type") == "response" and message.get("request_seq") == identifier)
                assert not rejected.get("success"), "an awaiting frame must never evaluate or assign an unrelated live frame's local"
            identifier = send("dataBreakpointInfo", {"frameId": frame["id"], "variablesReference": scope, "name": "outer"})
            watchpoint = until(lambda message: message.get("type") == "response" and message.get("request_seq") == identifier)
            assert not watchpoint.get("success") or watchpoint.get("body", {}).get("dataId") is None, "a saved local must not resolve a watchpoint against another method's physical frame"
            response(send("setVariable", {"variablesReference": scope, "name": "outer", "value": "30"}))
            edited = response(send("variables", {"variablesReference": scope}))["body"]["variables"]
            assert any(item["name"] == "outer" and item["value"] == "30" for item in edited), "editing a suspended local must update its saved state"


def exercise(source, send, response, until):
    stopped = until(lambda message: message.get("event") == "stopped")["body"]
    if stopped["reason"] == "exception":
        details = response(send("exceptionInfo", {"threadId": stopped["threadId"]}))
        raise AssertionError(f"async debugging must reach the resumed application without an unhandled exception: {details}")
    inspect(source, send, response, stopped["threadId"], until)
    response(send("continue", {"threadId": stopped["threadId"]}))
