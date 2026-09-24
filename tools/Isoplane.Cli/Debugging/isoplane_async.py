"""Recover awaiting source frames from the guest runtime's task relationships."""
import json

import gdb
from gdb.FrameDecorator import FrameDecorator

import isoplane_values


def identity(value):
    return int(value["__ptr"])


def tracking_state(frame):
    current = frame
    while current is not None:
        try:
            anchor = isoplane_values.dereference(current.read_var("__isoplaneAsyncState"), current)["State"]
        except (gdb.error, ValueError):
            current = current.older()
            continue
        address = isoplane_values.memory_base(frame) + identity(anchor)
        return address.cast(gdb.lookup_type("S_P_CoreLib_System_Threading_Tasks_Task_AsyncDebugState").pointer()).dereference()
    raise gdb.GdbError("The local guest invocation has no async debugger state reference.")


class SuspendedLocal:
    def __init__(self, owner, field, name):
        self.owner = owner
        self.field = field
        self.name = name

    def symbol(self):
        return self.name

    def value(self):
        return self.owner.saved()[self.field]


class SuspendedFrame(FrameDecorator):
    def __init__(self, frame, state, method):
        super().__init__(frame)
        self.state = state
        self.method = method
        index = int(self.saved()["<>1__state"])
        self.location = method["Awaits"][index] if 0 <= index < len(method["Awaits"]) else None

    def saved(self):
        address = isoplane_values.memory_base(self.inferior_frame()) + identity(self.state)
        return address.cast(gdb.lookup_type(self.method["NativeState"]).pointer()).dereference()

    def function(self):
        return self.method["Name"] + " [awaiting]"

    def filename(self):
        return self.location["Source"]

    def line(self):
        return self.location["Line"]

    def frame_args(self):
        return ()

    def frame_locals(self):
        for field, _ in isoplane_values.fields(self.saved()):
            if field == "<>4__this":
                yield SuspendedLocal(self, field, "this")
            elif not field.startswith("<"):
                yield SuspendedLocal(self, field, field)
            elif ">5__" in field:
                yield SuspendedLocal(self, field, field[1:field.index(">5__")])


def callers(frame, methods):
    physical = frame.inferior_frame()
    physical.select()
    state = physical.read_var("__locals")
    node = tracking_state(physical)["Frames"]
    nodes = []
    seen = set()
    while identity(node) and identity(node) not in seen:
        seen.add(identity(node))
        value = isoplane_values.dereference(node, physical)
        nodes.append(value)
        node = value["Next"]
    roots = [node for node in nodes if identity(node["StateMachine"]) == identity(state)]
    if not roots:
        return
    pending = [roots[0]]
    visited = {identity(roots[0]["Operation"])}
    while pending:
        child = pending.pop()
        for parent in nodes:
            operation = identity(parent["Operation"])
            if identity(parent["Awaited"]) != identity(child["Operation"]) or operation in visited:
                continue
            visited.add(operation)
            name = json.loads(isoplane_values.ManagedString(parent["StateMachineType"]).to_string())
            if name in methods and not methods[name]["IsValueType"]:
                suspended = SuspendedFrame(frame, parent["StateMachine"], methods[name])
                if suspended.location is not None:
                    yield suspended
            pending.append(parent)
