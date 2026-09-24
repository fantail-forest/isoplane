"""Prove native source debugging through the installed CLI and GDB's DAP server."""
import concurrent.futures
import json
import http.client
from pathlib import Path
import queue
import subprocess
import socket
import tomllib
import sys
import threading
import time
import urllib.request
import urllib.error
import urllib.parse

from debugpy.common.messaging import JsonIOStream

import debug_async_stack
import debug_library

launcher, app, source, mode = sys.argv[1:]
adapter = subprocess.Popen([launcher, "debug-adapter", "--nativeaot"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
stream = JsonIOStream.from_process(adapter)
messages = queue.Queue()
deferred = []
sequence = 0


def read():
    try:
        while True:
            messages.put(stream.read_json())
    except EOFError:
        messages.put({"closed": True})


threading.Thread(target=read, daemon=True).start()


def send(command, arguments):
    global sequence
    sequence += 1
    stream.write_json({"seq": sequence, "type": "request", "command": command, "arguments": arguments})
    return sequence


def until(predicate):
    for index, value in enumerate(deferred):
        if predicate(value):
            return deferred.pop(index)
    deadline = time.monotonic() + 600
    while True:
        value = messages.get(timeout=max(0, deadline - time.monotonic()))
        print(json.dumps(value), flush=True)
        assert not value.get("closed"), "native adapter closed before the expected debugger result"
        if predicate(value):
            return value
        assert value.get("event") != "terminated", "the native guest exited before the expected debugger operation"
        deferred.append(value)


def response(identifier):
    value = until(lambda item: item.get("type") == "response" and item.get("request_seq") == identifier)
    assert value.get("success"), f"native adapter rejected a required operation: {value}"
    return value.get("body", {})


def output_event(name):
    def find(item):
        if item.get("event") != "output":
            return False
        try:
            return json.loads(item["body"]["output"]).get("event") == name
        except (ValueError, AttributeError):
            return False
    return json.loads(until(find)["body"]["output"])


def request_http(url):
    with urllib.request.urlopen(url, timeout=600) as reply:
        return reply.status, reply.read()


def stop_adapter():
    if adapter.poll() is None:
        adapter.terminate()
    try:
        adapter.wait(timeout=30)
    except subprocess.TimeoutExpired:
        adapter.kill()
        adapter.wait()


def finish_request(result, expected):
    assert result.result(timeout=120) == (200, expected), "the native request must complete with the inspected or edited guest state"
    until(lambda item: item.get("event") == "terminated")
    response(send("disconnect", {}))
    assert adapter.wait(timeout=30) == 0, "the native adapter must exit cleanly after request completion"


try:
    lines = Path(source).read_text().splitlines()
    before = next(index + 1 for index, line in enumerate(lines) if ("int total =" if mode == "async" else "return result;" if mode == "library" else "answer *= 2;") in line)
    after = next(index + 1 for index, line in enumerate(lines) if ("return total;" if mode == "async" else "return result;" if mode == "library" else "return answer +") in line)
    response(send("initialize", {"adapterID": "isoplane", "pathFormat": "path", "linesStartAt1": True, "columnsStartAt1": True}))
    until(lambda item: item.get("event") == "initialized")
    launch = send("launch", {"program": launcher, "cwd": app, "args": ["dev", "--project", app,
        "--configuration", "IsoplaneNativeAotDebug", "--debug", "--no-watch", "--guest-runtime", "nativeaot", "--port", "0", "--json"]})
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [{"line": before}]}))
    response(send("configurationDone", {}))
    response(launch)
    artifacts = output_event("debugger_ready")
    assert artifacts["runtime"] == "nativeaot", "debugging must preserve the selected NativeAOT execution mode"
    module = Path(artifacts["module"])
    session_root = module.parent.parent
    ready = output_event("ready")
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as requests:
        try:
            http_result = requests.submit(request_http, ready["url"])
            if mode == "library":
                debug_library.exercise(source, send, lambda identifier: {"body": response(identifier)}, until)
                finish_request(http_result, debug_library.EXPECTED_RESPONSE)
            else:
                stopped = until(lambda item: item.get("event") == "stopped")
                thread = stopped["body"]["threadId"]
            if mode == "async":
                debug_async_stack.inspect(source, send, lambda identifier: {"body": response(identifier)}, thread, until)
                frames = response(send("stackTrace", {"threadId": thread}))["stackFrames"]
                assert frames and all(frame.get("source", {}).get("path") == source for frame in frames), "the native stack must contain only customer source frames after host reentry"
                frame = frames[0]
                assert frame["line"] == before, "the resumed async frame must retain its C# source location"
                assert frame["name"].endswith(".Inner"), "the async stack must name the original C# method"
                locals_by_name = {}
                for scope in response(send("scopes", {"frameId": frame["id"]}))["scopes"]:
                    if scope["name"] == "Locals":
                        local_scope = scope["variablesReference"]
                        locals_by_name = {value["name"]: value for value in response(send("variables", {"variablesReference": local_scope}))["variables"]}
                assert locals_by_name["answer"]["value"] == "22", "the async locals view must recover values saved before await"
                assert not any(name.startswith("__") for name in locals_by_name), "async locals must hide compiler and host bookkeeping"
                sample = response(send("variables", {"variablesReference": locals_by_name["sample"]["variablesReference"]}))["variables"]
                assert {value["name"]: value["value"] for value in sample} == {"Value": "7", "Name": '\"hello\"'}, "saved async object references must remain inspectable"
                response(send("setVariable", {"variablesReference": local_scope, "name": "answer", "value": "30"}))
                response(send("continue", {"threadId": thread}))
                finish_request(http_result, b"71")
            elif mode != "library":
                frames = response(send("stackTrace", {"threadId": thread}))["stackFrames"]
                assert len(frames) == 2 and all(frame.get("source", {}).get("path") == source for frame in frames), "native stacks must preserve customer callers and hide runtime frames"
                page = response(send("stackTrace", {"threadId": thread, "startFrame": 1, "levels": 1}))["stackFrames"]
                assert page == [frames[1]], "stack pagination must count visible customer frames"
                frame = frames[0]
                assert frame["line"] == before and frame["source"]["path"] == source, "the breakpoint must stop in actual NativeAOT C# source"
                local_scope = None
                for scope in response(send("scopes", {"frameId": frame["id"]}))["scopes"]:
                    values = response(send("variables", {"variablesReference": scope["variablesReference"]}))["variables"]
                    if any(value["name"] == "answer" and value["value"] == "21" for value in values):
                        local_scope = scope["variablesReference"]
                        locals_by_name = {value["name"]: value for value in values}
                assert local_scope is not None, "the NativeAOT local must be inspectable before stepping"
                assert "__vmctx" not in locals_by_name, "host memory bookkeeping must not appear as a customer local"
                if mode == "stop":
                    response(send("disconnect", {"terminateDebuggee": True}))
                    assert adapter.wait(timeout=30) == 0, "stopping a paused native guest must release the adapter"
                    try:
                        http_result.result(timeout=30)
                    except (concurrent.futures.TimeoutError, socket.timeout) as error:
                        raise AssertionError("the paused HTTP request did not observe session termination") from error
                    except (http.client.RemoteDisconnected, ConnectionResetError, ConnectionAbortedError, BrokenPipeError, http.client.IncompleteRead):
                        print("paused native HTTP connection terminated", flush=True)
                    except urllib.error.URLError as error:
                        assert isinstance(error.reason, (ConnectionResetError, ConnectionAbortedError, BrokenPipeError)), f"HTTP failure did not prove termination: {error}"
                    else:
                        raise AssertionError("a killed guest cannot complete its HTTP request")
                else:
                    sample = locals_by_name["sample"]
                    members = response(send("variables", {"variablesReference": sample["variablesReference"]}))["variables"]
                    assert {value["name"]: value["value"] for value in members} == {"Value": "14", "Name": '"hello 🌲"'}, "managed objects must show fields and decoded strings without runtime headers"
                    numbers = locals_by_name["numbers"]
                    elements = response(send("variables", {"variablesReference": numbers["variablesReference"]}))["variables"]
                    assert [value["value"] for value in elements] == ["3", "7"], "managed arrays must expose indexed elements"
                    assert locals_by_name["absent"]["value"] == "null", "managed null references must not be shown as WASM pointer wrappers"
                    response(send("setVariable", {"variablesReference": sample["variablesReference"], "name": "Value", "value": "18"}))
                    response(send("setVariable", {"variablesReference": numbers["variablesReference"], "name": "[1]", "value": "9"}))
                    evaluated = response(send("evaluate", {"expression": "answer + 1", "frameId": frame["id"], "context": "watch"}))
                    assert evaluated["result"] == "22", "native watch expressions must read the stopped local"
                    response(send("setVariable", {"variablesReference": local_scope, "name": "answer", "value": "30"}))
                    response(send("next", {"threadId": thread}))
                    until(lambda item: item.get("event") == "stopped")
                    frame = response(send("stackTrace", {"threadId": thread}))["stackFrames"][0]
                    assert frame["line"] == after and frame["source"]["path"] == source, "native next must advance one C# source line"
                    assert response(send("evaluate", {"expression": "answer", "frameId": frame["id"], "context": "watch"}))["result"] == "60", "the guest must execute with the edited local"
                    response(send("continue", {"threadId": thread}))
                    finish_request(http_result, b"66")
        finally:
            stop_adapter()
    assert not module.exists() and not session_root.exists(), "the adapter must remove its native debug artifacts even after a paused stop"
    address = urllib.parse.urlsplit(ready["url"])
    with socket.socket() as connection:
        assert connection.connect_ex((address.hostname, address.port)) != 0, "the native debugger must close its HTTP listener"
    manifest = tomllib.loads(Path(app, "isoplane.toml").read_text())
    assert not Path(app, manifest["module"]).exists(), "debug publication must not replace the ordinary module"
    print("native debug acceptance passed: " + mode, flush=True)
finally:
    stop_adapter()
