"""Recover suspended callers from the guest-owned task graph."""
from isoplane_lldb_values import at, dereference, fields, identity, member, string


def tracking_state(frame):
    thread = frame.GetThread()
    for index in range(frame.GetFrameID(), thread.GetNumFrames()):
        current = thread.GetFrameAtIndex(index)
        anchor = current.FindVariable("__isoplaneAsyncState")
        if anchor.IsValid() and anchor.GetError().Success():
            state = member(dereference(anchor, current), "State")
            state_type = frame.GetModule().FindFirstType("S_P_CoreLib_System_Threading_Tasks_Task_AsyncDebugState")
            return at(frame, identity(state), state_type)
    raise ValueError("The local guest invocation has no async debugger state reference.")


def saved_locals(saved):
    for name, value in fields(saved):
        if name == "<>4__this":
            yield "this", value
        elif not name.startswith("<"):
            yield name, value
        elif ">5__" in name:
            yield name[1:name.index(">5__")], value


def callers(frame, methods):
    state = frame.FindVariable("__locals")
    node = member(tracking_state(frame), "Frames")
    nodes, seen = [], set()
    while identity(node) and identity(node) not in seen:
        seen.add(identity(node))
        value = dereference(node, frame)
        nodes.append(value)
        node = member(value, "Next")
    roots = [node for node in nodes if identity(member(node, "StateMachine")) == identity(state)]
    if not roots:
        return
    pending = [roots[0]]
    visited = {identity(member(roots[0], "Operation"))}
    while pending:
        child = pending.pop()
        for parent in nodes:
            operation = identity(member(parent, "Operation"))
            if identity(member(parent, "Awaited")) != identity(member(child, "Operation")) or operation in visited:
                continue
            visited.add(operation)
            method = methods.get(string(member(parent, "StateMachineType"), frame))
            if method is not None and not method["IsValueType"]:
                state_type = frame.GetModule().FindFirstType(method["NativeState"])
                saved = at(frame, identity(member(parent, "StateMachine")), state_type)
                index = member(saved, "<>1__state").GetValueAsSigned()
                if 0 <= index < len(method["Awaits"]):
                    yield method, method["Awaits"][index], list(saved_locals(saved))
            pending.append(parent)
