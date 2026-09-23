"""Present application frames and managed values through LLDB's public scripting API."""
import base64
import json
import os
import sys
from pathlib import Path

import lldb

sys.path.insert(0, str(Path(__file__).parent))
from isoplane_lldb_values import Value, checked, dereference, fields
from isoplane_lldb_async import callers


class Session:
    def __init__(self):
        self.stop = None
        self.sources = {}
        self.methods = {}
        self.handles = {}
        self.next_handle = 1
        self.stacks = {}

    def remember(self, value):
        handle = self.next_handle
        self.next_handle += 1
        self.handles[handle] = value
        return handle

    def get(self, handle):
        if handle not in self.handles:
            raise ValueError("This debugger handle is stale; refresh the paused stack or locals.")
        return self.handles[handle]

    def load_sources(self):
        if self.sources:
            return
        root = Path(os.environ["ISOPLANE_NATIVE_DEBUG_DIRECTORY"])
        for index in root.glob("*/isoplane.debug-sources.json"):
            for method in json.loads(index.read_text()):
                self.sources.setdefault(os.path.normcase(os.path.normpath(method["Source"])), []).append(method)
                if method.get("Async") is not None:
                    self.methods[method["Async"]["StateMachineType"]] = method["Async"]
        if not self.sources:
            raise ValueError("The local session has no application source index.")

    def stack(self, process, thread_id):
        if thread_id in self.stacks:
            return self.stacks[thread_id]
        self.load_sources()
        thread = process.GetThreadByID(thread_id)
        if not thread.IsValid():
            raise ValueError("The requested thread is unavailable.")
        result = []
        for frame in thread:
            entry = frame.GetLineEntry()
            if not entry.IsValid():
                continue
            path, line = str(entry.GetFileSpec()), entry.GetLine()
            matches = [method for method in self.sources.get(os.path.normcase(os.path.normpath(path)), ())
                       if method["StartLine"] <= line <= method["EndLine"]]
            if not matches:
                continue
            method = min(matches, key=lambda method: method["EndLine"] - method["StartLine"])
            handle = self.remember((frame, None))
            result.append(dict(id=handle, name=method["Name"], line=line, column=entry.GetColumn(),
                               source=dict(path=path), instructionPointerReference=hex(frame.GetPC())))
            if len(result) == 1 and method.get("Async") is not None:
                for caller, location, saved in callers(frame, self.methods):
                    handle = self.remember((frame, saved))
                    result.append(dict(id=handle, name=caller["Name"] + " [awaiting]", line=location["Line"],
                                       column=0, source=dict(path=location["Source"])))
        self.stacks[thread_id] = result
        return result

    def variable(self, name, value, frame):
        value = Value(value, frame)
        reference = self.remember(value) if value.expandable() else 0
        result = dict(name=name, value=value.text(), type=value.type_name(), variablesReference=reference)
        length = value.array_length()
        if length is not None:
            result["indexedVariables"] = length
        return result

    def scopes(self, arguments):
        frame, saved = self.get(arguments["frameId"])
        if saved is None:
            saved = []
            for value in frame.GetVariables(True, True, False, True):
                name = value.GetName()
                if name == "__locals":
                    saved.extend(fields(dereference(value, frame)))
                elif not name.startswith("__"):
                    saved.append((name, value))
        reference = self.remember((frame, saved))
        return dict(scopes=[dict(name="Locals", presentationHint="locals", expensive=False, variablesReference=reference)])

    def variables(self, arguments):
        owner = self.get(arguments["variablesReference"])
        start, count = arguments.get("start", 0), arguments.get("count", 0)
        if isinstance(owner, Value):
            frame = owner.frame
            children = owner.children(start, count)
        else:
            frame, saved = owner
            children = saved[start:start + count if count else None]
        return dict(variables=[self.variable(name, value, frame) for name, value in children])

    def set_variable(self, arguments):
        owner = self.get(arguments["variablesReference"])
        if isinstance(owner, Value):
            frame, children = owner.frame, owner.children()
        else:
            frame, children = owner
        value = next((value for name, value in children if name == arguments["name"]), None)
        if value is None:
            raise ValueError("The requested managed variable is unavailable.")
        managed = Value(value, frame)
        managed.assign(arguments["value"])
        result = self.variable(arguments["name"], managed.raw, frame)
        del result["name"]
        return result

    def evaluate(self, arguments):
        frame, saved = self.get(arguments["frameId"])
        if saved is not None:
            raise ValueError("Expression evaluation is not supported in a suspended async caller. Use its Locals view.")
        options = lldb.SBExpressionOptions()
        options.SetIgnoreBreakpoints(True)
        options.SetUnwindOnError(True)
        options.SetAllowJIT(False)
        value = checked(frame.EvaluateExpression(arguments["expression"], options))
        result = self.variable("result", value, frame)
        result["result"] = result.pop("value")
        del result["name"]
        return result

    def request(self, debugger, request):
        process = debugger.GetSelectedTarget().GetProcess()
        if not process.IsValid() or process.GetState() != lldb.eStateStopped:
            raise ValueError("Pause the guest before inspecting managed state.")
        stop = (process.GetUniqueID(), process.GetStopID())
        if stop != self.stop:
            self.handles.clear()
            self.stacks.clear()
            self.stop = stop
        command, arguments = request["command"], request["arguments"]
        if command == "stackTrace":
            frames = self.stack(process, arguments["threadId"])
            start, count = arguments.get("startFrame", 0), arguments.get("levels", 0)
            return dict(stackFrames=frames[start:start + count if count else None], totalFrames=len(frames))
        if command == "scopes":
            return self.scopes(arguments)
        if command == "variables":
            return self.variables(arguments)
        if command == "setVariable":
            return self.set_variable(arguments)
        if command == "evaluate":
            return self.evaluate(arguments)
        if command == "dataBreakpointInfo":
            return dict(dataId=None, description="Managed data breakpoints are not supported by this session.", canPersist=False)
        raise ValueError(f"Managed debugger request {command} is not supported.")


_session = Session()


def dispatch(debugger, command, result, internal_dict):
    try:
        request = json.loads(base64.b64decode(command))
        response = dict(body=_session.request(debugger, request))
    except (ValueError, KeyError, TypeError, OverflowError) as error:
        response = dict(error=str(error))
    result.AppendMessage(json.dumps(response, ensure_ascii=True))


def __lldb_init_module(debugger, internal_dict):
    debugger.HandleCommand("command script add -f isoplane_lldb.dispatch isoplane-request")
