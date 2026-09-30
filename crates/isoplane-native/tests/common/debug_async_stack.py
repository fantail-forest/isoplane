"""Preserve logical callers and saved locals across host I/O."""
from pathlib import Path
from debug_sources import same_source

EXPECTED_RESPONSE = b"103"


def source_line(source, text):
    return next(index + 1 for index, line in enumerate(Path(source).read_text().splitlines()) if text in line)


def configure(source, send, response):
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [
        {"line": source_line(source, statement)} for statement in ["int total =", "int sum ="]]}))


def inspect(source, send, response, thread, until):
    trace = response(send("stackTrace", {"threadId": thread}))["body"]
    frames = trace["stackFrames"]
    expected = [("Inner", "int total =", "answer", "22"),
                ("Outer", "int result = await Inner", "outer", "22"),
                ("Handle", "int result = await Outer", "seed", "21")]
    application = [frame for frame in frames if same_source(frame.get("source", {}).get("path"), source)]
    assert all(not frame["name"].startswith(("System.", "Microsoft.AspNetCore.", "Isoplane.")) for frame in frames), "runtime and SDK frames must not appear in the customer async stack"
    assert len(application) == 3, "the resumed async stack must retain Inner, Outer and Handle across host parking"
    assert trace["totalFrames"] == len(frames), "the frame count must describe the filtered stack"
    page = response(send("stackTrace", {"threadId": thread, "startFrame": 1, "levels": 1}))["body"]
    assert page["totalFrames"] == len(frames) and [frame["name"] for frame in page["stackFrames"]] == [frames[1]["name"]], "stack pagination must use visible frame positions"
    watchpoints = set()
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
            value = response(send("evaluate", {"frameId": frame["id"], "expression": "outerSample.Value + outerNumbers[1] + outer", "context": "watch"}))["body"]
            assert value["result"] == "26", "a watch must evaluate the suspended caller's saved objects and locals"
            response(send("setVariable", {"variablesReference": scope, "name": "outer", "value": "outer + 8"}))
            response(send("setExpression", {"frameId": frame["id"], "expression": "outerSample.Value", "value": "outer"}))
            numbers = next(item for item in values if item["name"] == "outerNumbers")
            elements = response(send("variables", {"variablesReference": numbers["variablesReference"]}))["body"]["variables"]
            element = next(item for item in elements if item["evaluateName"] == "outerNumbers[1]")
            response(send("setVariable", {"variablesReference": numbers["variablesReference"], "name": element["name"], "value": "outerNumbers[0] + 6"}))
            edited = response(send("variables", {"variablesReference": scope}))["body"]["variables"]
            assert any(item["name"] == "outer" and item["value"] == "30" for item in edited), "expression assignment must update the suspended caller's saved state"
            locations = [response(send("dataBreakpointInfo", arguments))["body"] for arguments in [
                {"frameId": frame["id"], "name": "outerSample.Value"},
                {"variablesReference": numbers["variablesReference"], "name": element["name"]}]]
            assert all(location.get("dataId") for location in locations), "managed watchpoints must resolve from a suspended caller"
            configured = response(send("setDataBreakpoints", {"breakpoints": [{"dataId": location["dataId"]} for location in locations]}))["body"]["breakpoints"]
            assert all(item["verified"] for item in configured), "both suspended-frame watchpoints must bind"
            watchpoints = {item["id"] for item in configured}
    assert len(watchpoints) == 2, "the suspended caller must provide both watchpoints"
    return watchpoints


def exercise(source, send, response, until):
    stopped = until(lambda message: message.get("event") == "stopped")["body"]
    if stopped["reason"] == "exception":
        details = response(send("exceptionInfo", {"threadId": stopped["threadId"]}))
        raise AssertionError(f"async debugging must reach the resumed application without an unhandled exception: {details}")
    pending = inspect(source, send, response, stopped["threadId"], until)
    response(send("continue", {"threadId": stopped["threadId"]}))
    helper = until(lambda message: message.get("event") == "stopped")["body"]
    assert helper["reason"] == "breakpoint", "execution must reach the synchronous helper before writing watched storage"
    frames = response(send("stackTrace", {"threadId": helper["threadId"]}))["body"]["stackFrames"]
    assert len(frames) == 4 and all(name in frame["name"] for frame, name in zip(frames, ["ComputeTotal", "Inner", "Outer", "Handle"])), "a synchronous helper must retain the executing async method and its suspended caller chain"
    outer = response(send("evaluate", {"frameId": frames[2]["id"], "expression": "outer + input", "context": "watch"}))["body"]
    assert outer["result"] == "51", "saved-frame watches must retain the caller's edits and parameters while a synchronous helper executes"
    response(send("continue", {"threadId": helper["threadId"]}))
    while pending:
        stopped = until(lambda message: message.get("event") == "stopped")["body"]
        assert stopped["reason"] == "data breakpoint", "saved-frame watchpoints must observe writes after another await and GC"
        hits = set(stopped.get("hitBreakpointIds", []))
        assert hits and hits <= pending, "the stop must identify the expected managed storage location"
        pending -= hits
        response(send("continue", {"threadId": stopped["threadId"]}))
