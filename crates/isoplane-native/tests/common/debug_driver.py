"""Drive the installed modern adapter with maintained debugpy DAP framing.

The Rust owner bounds the entire journey, including paused HTTP requests and shutdown,
and cleans up its process tree on timeout.
"""
import concurrent.futures
import json
import http.client
import os
from pathlib import Path
import queue
import re
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import urllib.parse

from debugpy.common.messaging import JsonIOStream
import debug_async_stack
import debug_library
import debug_uncaught
import debug_features
import debug_framework_callers
import debug_hot_reload
import debug_hot_reload_regions
import debug_hot_reload_handler
import debug_hot_reload_constructor_callers
import debug_hot_reload_running
import debug_hot_reload_callers
import debug_hot_reload_iterators
import debug_hot_reload_resume
import debug_hot_reload_lines
import debug_statements
import debug_statement_awaits
import debug_statement_iterators
import debug_statement_handlers
import debug_watchpoints
import debug_watchpoint_steps
from debug_sources import same_source, stop_event

launcher, endpoint, url, source, owner, mode, module = sys.argv[1:]
# The fixture route whose code this mode pauses in, appended to whichever address serves it.
route = os.environ.get("ISOPLANE_DEBUG_ROUTE", "")
url = url.rstrip("/") + route
owner = int(owner)
terminal_mode = mode.startswith("terminal-")
launched = mode in ("launch", "reload") or terminal_mode
terminal = None
terminal_output = queue.Queue()
if mode == "protocol" or mode.startswith("hotreload"):
    root = Path(__file__).resolve().parents[4]
    driver = root / ".build/clr-anycpu/managed/bin/Isoplane.Debugging.Tests/release/Isoplane.Debugging.Tests.dll"
    if not driver.is_file():
        sys.exit("Missing debugger protocol driver; run task managed:tests before testing.")
    arguments = ["dotnet", driver, endpoint, url]
    if mode.startswith("hotreload"):
        arguments.extend([Path(os.environ["ISOPLANE_TEST_HOT_RELOAD_ARTIFACTS"]) / "bin/HotReloadFixture/release", source])
    if mode == "hotreload-without-symbols":
        arguments.append("--without-symbols")
    sys.exit(subprocess.run(arguments, timeout=240).returncode)
scenario = {"edit-and-continue": debug_hot_reload, "uncaught": debug_uncaught, "statements": debug_statements, "statement-handlers": debug_statement_handlers, "statement-awaits": debug_statement_awaits, "statement-awaits-optimized": debug_statement_awaits, "features": debug_features, "watchpoints": debug_watchpoints, "watchpoint-steps": debug_watchpoint_steps, "library": debug_library, "async-stack": debug_async_stack, "async-optimized": debug_async_stack, "framework-callers": debug_framework_callers}.get(mode)
if mode in ("edit-and-continue-async", "edit-and-continue-try"):
    scenario = debug_hot_reload_regions
if mode == "edit-and-continue-handler":
    scenario = debug_hot_reload_handler
if mode == "statement-iterators":
    scenario = debug_statement_iterators
if mode == "edit-and-continue-constructor-callers":
    scenario = debug_hot_reload_constructor_callers
if mode == "edit-and-continue-running":
    scenario = debug_hot_reload_running
if mode == "edit-and-continue-callers":
    scenario = debug_hot_reload_callers
if mode == "edit-and-continue-iterators":
    scenario = debug_hot_reload_iterators
if mode == "edit-and-continue-resume":
    scenario = debug_hot_reload_resume
if mode == "edit-and-continue-lines":
    scenario = debug_hot_reload_lines
if mode.startswith("async-"):
    scenario = debug_async_stack
adapter_command = ["dotnet", launcher] if Path(launcher).suffix == ".dll" else [launcher]
adapter = subprocess.Popen([*adapter_command, "debug-adapter"], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
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


def launch_terminal(message):
    global terminal
    arguments = message["arguments"]
    assert arguments["kind"] == "integrated"
    terminal = subprocess.Popen(arguments["args"], cwd=arguments["cwd"],
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    def capture():
        for line in terminal.stdout:
            print("terminal: " + line.rstrip(), flush=True)
            terminal_output.put(line)
    threading.Thread(target=capture, daemon=True).start()
    stream.write_json({"seq": 0, "type": "response", "request_seq": message["seq"], "command": "runInTerminal",
                       "success": True, "body": {"processId": terminal.pid}})


def owner_event(event):
    if terminal_mode:
        marker = "  debugger detached;" if event == "debugger_detached" else "  reloaded "
        while marker not in terminal_output.get():
            pass
    else:
        print("request-owner-event:" + event, flush=True)
        assert sys.stdin.readline() == "owner-event:" + event + "\n"


def stop_owner():
    if terminal_mode:
        terminal.stdin.close()
        assert terminal.wait() == 0, "closing the terminal input must stop the detached application"
    else:
        print("request-owner-stop", flush=True)
        assert sys.stdin.readline() == "owner-stopped\n"


# Requests whose HTTP exchange has finished, with how it ended.
completed_requests = {}


def until(predicate):
    for index, value in enumerate(deferred):
        if predicate(value):
            return deferred.pop(index)
    while True:
        if predicate is stop_event and http_result in completed_requests:
            raise AssertionError("the request completed without pausing the debugger: "
                                 + completed_requests[http_result])
        value = messages.get()
        if "requestCompleted" in value:
            completed_requests[value["requestCompleted"]] = value["outcome"]
            continue
        print(json.dumps(value), flush=True)
        if value.get("type") == "request" and value.get("command") == "runInTerminal":
            launch_terminal(value)
            continue
        assert not value.get("closed"), "adapter closed before the expected debug result"
        assert mode == "stop" or value.get("event") != "terminated", "the debug session must remain attached between requests"
        if predicate(value):
            return value
        deferred.append(value)


def response(identifier):
    value = until(lambda item: item.get("type") == "response" and item.get("request_seq") == identifier)
    assert value.get("success"), f"adapter rejected a required operation: {value}"
    return value


def request_http(result):
    try:
        with urllib.request.urlopen(url) as reply:
            result.set_result((reply.status, reply.read()))
    except urllib.error.HTTPError as error:
        result.set_result((error.code, error.read()))
    except Exception as error:
        result.set_exception(error)
    outcome = repr(result.exception() or result.result())
    messages.put({"requestCompleted": result, "outcome": f"{url} -> {outcome}"})


def process_tree():
    if os.name == 'nt':
        listing = subprocess.check_output(['powershell', '-NoProfile', '-NonInteractive', '-Command',
            'Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId | ConvertTo-Json -Compress'],
            text=True, timeout=30)
        return {entry['ProcessId']: (entry['ParentProcessId'], 'running') for entry in json.loads(listing)}
    listing = subprocess.check_output(['ps', '-A', '-o', 'pid=,ppid=,stat='], text=True, timeout=30)
    return {int(pid): (int(parent), state) for pid, parent, state in (line.split() for line in listing.splitlines())}


def children(pid, tree):
    values = [child for child, (parent, _) in tree.items() if parent == pid]
    return values + [child for value in values for child in children(value, tree)]


def any_running(owned):
    tree = process_tree()
    return any(pid in tree and not tree[pid][1].startswith('Z') for pid in owned)


http_result = None
try:
    lines = Path(source).read_text().splitlines()
    if scenario is None:
        before = next(index + 1 for index, line in enumerate(lines) if "await delay;" in line)
        after = next(index + 1 for index, line in enumerate(lines) if "bool resumed = true;" in line)
    response(send("initialize", {"adapterID": "isoplane", "pathFormat": "path", "linesStartAt1": True, "columnsStartAt1": True, "supportsRunInTerminalRequest": terminal_mode}))
    if launched:
        attached = send("launch", {"project": module, "configuration": "Debug", "port": 0})
    else:
        address, port = endpoint.rsplit(":", 1)
        attach_options = {"address": address, "port": int(port)}
        if Path(launcher).suffix == ".dll":
            attach_options["sourceFileMap"] = {"/isoplane-debugger-fixture": str(Path(source).parent)}
        attached = send("attach", attach_options)
    until(lambda item: item.get("event") == "initialized")
    if scenario is not None:
        scenario.configure(source, send, response)
    else:
        response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [{"line": before}], "lines": [before]}))
    response(send("configurationDone", {}))
    if launched:
        response(attached)
        ready = until(lambda item: item.get("event") == "output" and re.search(r"ready .+ at (http://\S+)", item["body"]["output"]))
        url = re.search(r"ready .+ at (http://\S+)", ready["body"]["output"]).group(1).rstrip("/") + route
        owned = [terminal.pid] + children(terminal.pid, process_tree()) if terminal_mode else children(adapter.pid, process_tree())
        assert owned, "launch must own the local application process"
    http_endpoint = urllib.parse.urlsplit(url)
    with socket.create_connection((http_endpoint.hostname, http_endpoint.port), timeout=30) as probe:
        probe.shutdown(socket.SHUT_WR)
        assert probe.recv(1) == b"", "an empty connection must close without consuming the debug request"
    previous_frame = None
    for attempt in range(2 if mode in ("requests", "launch", "reload", "uncaught", "detach-failure", "terminal-launch", "edit-and-continue-running") else 1):
        http_result = concurrent.futures.Future()
        threading.Thread(target=request_http, args=(http_result,), daemon=True).start()
        if attempt == 0 and not launched:
            response(attached)
        if scenario is not None:
            scenario.exercise(source, send, response, until)
        else:
            bound = until(lambda item: item.get("event") == "breakpoint" and item["body"]["breakpoint"]["verified"])
            assert bound["body"]["breakpoint"]["line"] == before, "the adapter must confirm binding at the requested customer line"
            for expected in [before, after]:
                stopped = until(stop_event)
                thread = stopped["body"]["threadId"]
                frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
                if attempt > 0 and previous_frame is not None:
                    stale = send("scopes", {"frameId": previous_frame})
                    rejected = until(lambda item: item.get("type") == "response" and item.get("request_seq") == stale)
                    assert not rejected.get("success"), "frame handles from an earlier request must not resolve in the new guest"
                    previous_frame = None
                assert frame["line"] == expected and same_source(frame["source"]["path"], source), "debugger must stop in the actual customer source"
                values = []
                for scope in response(send("scopes", {"frameId": frame["id"]}))["body"]["scopes"]:
                    values.extend(response(send("variables", {"variablesReference": scope["variablesReference"]}))["body"]["variables"])
                assert any(value["name"] == "answer" and value["value"] == ("84" if mode in ("reload", "detach-failure") and attempt == 1 else "42") for value in values), "customer local answer must remain inspectable"
                if mode == "reload" and attempt == 0 and expected == before:
                    original = Path(source).read_text()
                    Path(source).write_text(original + "\n#error debugger reload regression\n")
                    until(lambda item: item.get("event") == "output" and "reload failed; keeping" in item["body"]["output"])
                    saved = response(send("evaluate", {"expression": "answer", "frameId": frame["id"], "context": "watch"}))
                    assert saved["body"]["result"] == "42", "a failed rebuild must leave the paused invocation inspectable"
                    Path(source).write_text(original.replace("int answer = 42;", "int answer = 84;"))
                    rebuilt = until(lambda item: item.get("seq", 0) > saved["seq"] and item.get("event") == "output"
                                    and "rebuilt application;" in item["body"]["output"])
                    saved = response(send("evaluate", {"expression": "answer", "frameId": frame["id"], "context": "watch"}))
                    assert saved["body"]["result"] == "42", "a successful rebuild must preserve the active invocation until it completes"
                if mode == "detach-failure" and expected == before and attempt == 0:
                    original = Path(source).read_text()
                    Path(source).write_text(original + "\n#error detach rebuild ordering\n")
                    print('request-owner-event:reload_failed', flush=True)
                    assert sys.stdin.readline() == 'owner-event:reload_failed\n'
                    Path(source).write_text(original.replace("int answer = 42;", "int answer = 84;"))
                    print('request-owner-event:reload_pending', flush=True)
                    assert sys.stdin.readline() == 'owner-event:reload_pending\n'
                    for command, arguments in [("disconnect", {"terminateDebuggee": False}), ("isoplane/prepareDetach", {})]:
                        rejected_id = send(command, arguments)
                        rejected = until(lambda item: item.get("type") == "response" and item.get("request_seq") == rejected_id)
                        assert not rejected.get("success"), "failed ordinary-runtime preparation must reject detach"
                        retained = response(send("evaluate", {"expression": "answer", "frameId": frame["id"], "context": "watch"}))
                        assert retained["body"]["result"] == "42", "failed detach must preserve the paused guest and its frame handles"
                if mode in ("detach", "terminal-detach"):
                    response(send("setExpression", {"expression": "answer", "frameId": frame["id"], "value": "84"}))
                    if terminal_mode:
                        response(send("isoplane/prepareDetach", {}))
                    response(send("disconnect", {"terminateDebuggee": False}))
                    assert adapter.wait() == 0, "detach must close the editor adapter"
                    assert http_result.result() == (200, b"84"), "detach must resume the same guest with its edited local"
                    owner_event("debugger_detached")
                    with urllib.request.urlopen(url) as reply:
                        assert (reply.status, reply.read()) == (200, b"42"), "subsequent requests must run without a debugger"
                    if not terminal_mode:
                        with socket.socket() as check:
                            assert check.connect_ex((address, int(port))) != 0, "detach must release the debugger control listener"
                    original = Path(source).read_text()
                    Path(source).write_text(original.replace("int answer = 42;", "int answer = 84;"))
                    owner_event("reloaded")
                    with urllib.request.urlopen(url) as reply:
                        assert (reply.status, reply.read()) == (200, b"84"), "source reload must keep using the ordinary runtime after detach"
                    stop_owner()
                    assert (not any((Path(module) / ".isoplane/debug").iterdir()) if terminal_mode else not Path(module).exists()), "stopping the terminal owner must release detached session artifacts"
                    break
                if mode == "stop":
                    owned = [owner] + children(owner, process_tree())
                    print('request-owner-stop', flush=True)
                    assert sys.stdin.readline() == 'owner-stopped\n', 'the development harness must complete the requested shutdown'
                    deadline = time.monotonic() + 120
                    while any_running(owned):
                        assert time.monotonic() < deadline, "CLI stop left an owned process running while paused"
                        time.sleep(0.01)
                    assert not Path(module).exists(), "CLI stop must remove the private session artifact"
                    with socket.socket() as check:
                        assert check.connect_ex((address, int(port))) != 0, "CLI stop must close its debugger relay"
                    with socket.socket() as check:
                        assert check.connect_ex((http_endpoint.hostname, http_endpoint.port)) != 0, "CLI stop must close its HTTP listener"
                    try:
                        http_result.result()
                    except (http.client.RemoteDisconnected, ConnectionResetError, ConnectionAbortedError, BrokenPipeError, http.client.IncompleteRead):
                        print("paused HTTP connection terminated", flush=True)
                    except urllib.error.URLError as error:
                        assert isinstance(error.reason, (ConnectionResetError, ConnectionAbortedError, BrokenPipeError)), f"HTTP failed without proving connection termination: {error}"
                    else:
                        raise AssertionError("a paused request must not complete normally after CLI stop")
                    break
                if expected == after:
                    assert any(value["name"] == "delay" and "RanToCompletion" in value["value"] for value in values), "DAP next must cross the incomplete await and preserve completed task state"
                response(send("next" if expected == before else "continue", {"threadId": thread}))
        if mode in ("requests", "launch", "reload"):
            previous_frame = frame["id"]
        if mode not in ("stop", "detach", "terminal-detach"):
            actual = http_result.result()
            expected = (getattr(scenario, "EXPECTED_STATUS", 200), scenario.EXPECTED_RESPONSE if scenario is not None else b"84" if mode in ("reload", "detach-failure") and attempt == 1 else b"42")
            assert actual == expected, f"continued customer code returned {actual!r}, expected {expected!r}"
        if mode == "detach-failure" and attempt == 0:
            print('request-owner-event:reloaded', flush=True)
            assert sys.stdin.readline() == 'owner-event:reloaded\n'
        if mode == "reload" and attempt == 0:
            until(lambda item: item.get("seq", 0) > rebuilt["seq"] and item.get("event") == "output"
                  and "  reloaded " in item["body"]["output"])
    if mode == "detach-idle":
        response(send("disconnect", {"terminateDebuggee": False}))
        assert adapter.wait() == 0, "idle detach must cancel the pending guest handshake"
        print('request-owner-event:debugger_detached', flush=True)
        assert sys.stdin.readline() == 'owner-event:debugger_detached\n'
        with urllib.request.urlopen(url) as reply:
            assert (reply.status, reply.read()) == (200, b"42"), "idle detach must keep the application serving requests"
        print('request-owner-stop', flush=True)
        assert sys.stdin.readline() == 'owner-stopped\n'
    if mode == "stop":
        until(lambda item: item.get("event") == "terminated")
    if mode not in ("detach", "detach-idle", "terminal-detach"):
        response(send("disconnect", {"terminateDebuggee": mode != "stop"}))
    assert adapter.wait() == 0, "the adapter must exit cleanly after editor disconnect or owner cancellation"
    if terminal is not None:
        assert terminal.wait() == 0, "terminal application must exit cleanly"
    if launched:
        assert not any_running(owned), "disconnect must reap the launched application and its children"
        with socket.socket() as check:
            assert check.connect_ex((http_endpoint.hostname, http_endpoint.port)) != 0, "disconnect must close the launched HTTP listener"
    print("debug acceptance passed: " + mode, flush=True)
finally:
    if sys.exc_info()[0] is not None:
        while True:
            try:
                print("Unconsumed debugger message:", messages.get_nowait(), flush=True)
            except queue.Empty:
                break
    if sys.exc_info()[0] is not None and http_result is not None and http_result.done():
        try:
            print("HTTP outcome at debugger failure:", http_result.result(), flush=True)
        except Exception as error:
            print("HTTP failure at debugger failure:", repr(error), flush=True)
    if terminal is not None and terminal.poll() is None:
        if not terminal.stdin.closed:
            terminal.stdin.close()
        try:
            terminal.wait(timeout=30)
        except subprocess.TimeoutExpired:
            terminal.kill()
            terminal.wait()
    if adapter.poll() is None:
        adapter.terminate()
    try:
        adapter.wait(timeout=5)
    except subprocess.TimeoutExpired:
        adapter.kill()
        adapter.wait()
