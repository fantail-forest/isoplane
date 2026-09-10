"""Drive an external Mono adapter with maintained debugpy DAP framing."""
import concurrent.futures
import json
import http.client
import os
from pathlib import Path
import queue
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import urllib.parse

from debugpy.common.messaging import JsonIOStream

launcher, endpoint, url, source, owner, mode, module = sys.argv[1:]
owner = int(owner)
adapter = subprocess.Popen([launcher], stdin=subprocess.PIPE, stdout=subprocess.PIPE)
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
    deadline = time.monotonic() + 180
    while True:
        value = messages.get(timeout=max(0, deadline - time.monotonic()))
        print(json.dumps(value), flush=True)
        assert not value.get("closed"), "adapter closed before the expected debug result"
        if predicate(value):
            return value
        deferred.append(value)


def response(identifier):
    value = until(lambda item: item.get("type") == "response" and item.get("request_seq") == identifier)
    assert value.get("success"), f"adapter rejected a required operation: {value}"
    return value


def request_http(result):
    try:
        with urllib.request.urlopen(url, timeout=180) as reply:
            result.set_result((reply.status, reply.read()))
    except Exception as error:
        result.set_exception(error)


def children(pid):
    path = Path(f"/proc/{pid}/task/{pid}/children")
    values = [int(value) for value in path.read_text().split()] if path.exists() else []
    return values + [child for value in values for child in children(value)]


def running(pid):
    try:
        return Path(f"/proc/{pid}/stat").read_text().split(") ", 1)[1][0] != "Z"
    except FileNotFoundError:
        return False


try:
    lines = Path(source).read_text().splitlines()
    before = next(index + 1 for index, line in enumerate(lines) if "await delay;" in line)
    after = next(index + 1 for index, line in enumerate(lines) if "bool resumed = true;" in line)
    http_endpoint = urllib.parse.urlsplit(url)
    with socket.create_connection((http_endpoint.hostname, http_endpoint.port), timeout=30) as probe:
        probe.shutdown(socket.SHUT_WR)
        assert probe.recv(1) == b"", "an empty connection must close without consuming the debug request"
    response(send("initialize", {"adapterID": "mono", "pathFormat": "path", "linesStartAt1": True, "columnsStartAt1": True}))
    response(send("setBreakpoints", {"source": {"path": source}, "breakpoints": [{"line": before}], "lines": [before]}))
    address, port = endpoint.rsplit(":", 1)
    attached = send("attach", {"address": address, "port": int(port)})
    http = concurrent.futures.Future()
    threading.Thread(target=request_http, args=(http,), daemon=True).start()
    response(attached)
    for expected in [before, after]:
        stopped = until(lambda item: item.get("event") == "stopped")
        thread = stopped["body"]["threadId"]
        frame = response(send("stackTrace", {"threadId": thread}))["body"]["stackFrames"][0]
        assert frame["line"] == expected and frame["source"]["path"] == source, "debugger must stop in the actual customer source"
        values = []
        for scope in response(send("scopes", {"frameId": frame["id"]}))["body"]["scopes"]:
            values.extend(response(send("variables", {"variablesReference": scope["variablesReference"]}))["body"]["variables"])
        assert any(value["name"] == "answer" and value["value"] == "42" for value in values), "customer local answer must remain inspectable"
        if mode == "stop":
            owned = [owner] + children(owner)
            os.kill(owner, signal.SIGINT)
            deadline = time.monotonic() + 120
            while any(running(pid) for pid in owned):
                assert time.monotonic() < deadline, "CLI stop left an owned process running while paused"
                time.sleep(0.01)
            assert not Path(module).exists(), "CLI stop must remove the private session artifact"
            with socket.socket() as check:
                assert check.connect_ex((address, int(port))) != 0, "CLI stop must close its debugger relay"
            with socket.socket() as check:
                assert check.connect_ex((http_endpoint.hostname, http_endpoint.port)) != 0, "CLI stop must close its HTTP listener"
            try:
                http.result(timeout=30)
            except (concurrent.futures.TimeoutError, socket.timeout) as error:
                raise AssertionError("paused HTTP requester did not observe connection termination") from error
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
    if mode != "stop":
        assert http.result(timeout=180) == (200, b"42"), "continued customer code must complete the actual HTTP response"
    print("debug acceptance passed: " + mode, flush=True)
finally:
    if adapter.poll() is None:
        adapter.terminate()
    try:
        adapter.wait(timeout=5)
    except subprocess.TimeoutExpired:
        adapter.kill()
        adapter.wait()
