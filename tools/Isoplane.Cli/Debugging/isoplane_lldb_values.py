"""Read managed layouts from DWARF and resolve references through guest memory."""
import json

import lldb


def checked(value):
    if not value.IsValid() or value.GetError().Fail():
        raise ValueError(str(value.GetError()) if value.IsValid() else "The requested value is unavailable.")
    return value.GetNonSyntheticValue()


def member(value, name):
    return checked(value.GetChildMemberWithName(name))


def identity(value):
    return member(value, "__ptr").GetValueAsUnsigned()


def memory_base(frame):
    thread = frame.GetThread()
    for index in range(frame.GetFrameID(), thread.GetNumFrames()):
        context = thread.GetFrameAtIndex(index).FindVariable("__vmctx")
        memory = context.GetChildMemberWithName("memory")
        if memory.IsValid() and memory.GetError().Success():
            return memory.GetValueAsUnsigned()
    raise ValueError("The selected stack has no guest memory context.")


def at(frame, offset, value_type, name="value"):
    target = frame.GetThread().GetProcess().GetTarget()
    return checked(target.CreateValueFromAddress(name, lldb.SBAddress(memory_base(frame) + offset, target), value_type))


def is_reference(value):
    return value.GetType().GetCanonicalType().GetName().startswith(("WebAssemblyRefWrapper<", "WebAssemblyPtrWrapper<"))


def dereference(value, frame):
    value = checked(value)
    if not is_reference(value):
        return value
    offset = identity(value)
    if not offset:
        return None
    return at(frame, offset, value.GetType().GetTemplateArgumentType(0), value.GetName())


def fields(value):
    value = checked(value)
    value_type = value.GetType()
    for index in range(value_type.GetNumberOfDirectBaseClasses()):
        base = value_type.GetDirectBaseClassAtIndex(index).GetType()
        if base.GetName() != "System_Object":
            yield from fields(value.Cast(base))
    for index in range(value_type.GetNumberOfFields()):
        name = value_type.GetFieldAtIndex(index).GetName()
        yield name, member(value, name)


def string(value, frame):
    value = dereference(value, frame)
    if value is None:
        return None
    length = member(value, "_stringLength").GetValueAsUnsigned()
    if length == 0:
        return ""
    address = member(value, "_firstChar").GetLoadAddress()
    error = lldb.SBError()
    data = frame.GetThread().GetProcess().ReadMemory(address, length * 2, error)
    if error.Fail():
        raise ValueError(str(error))
    return data.decode("utf-16-le", errors="surrogatepass")


class Value:
    def __init__(self, value, frame):
        self.raw = checked(value)
        self.frame = frame
        self.value = dereference(value, frame)

    def array_length(self):
        if self.value is not None and self.value.GetTypeName().startswith("__Array<"):
            return member(self.value, "Length").GetValueAsUnsigned()
        return None

    def children(self, start=0, count=0):
        length = self.array_length()
        if length is not None:
            data = member(self.value, "Data")
            element = data.GetType().GetArrayElementType()
            target = self.frame.GetThread().GetProcess().GetTarget()
            end = min(length, start + count) if count else length
            for index in range(start, end):
                name = f"[{index}]"
                address = lldb.SBAddress(data.GetLoadAddress() + index * element.GetByteSize(), target)
                yield name, checked(target.CreateValueFromAddress(name, address, element))
        elif self.value is not None and self.value.GetTypeName() != "System_String":
            members = list(fields(self.value))
            yield from members[start:start + count if count else None]

    def text(self):
        if self.value is None:
            return "null"
        if self.value.GetTypeName() == "System_String":
            return json.dumps(string(self.raw, self.frame), ensure_ascii=False).encode("utf-8", errors="backslashreplace").decode("utf-8")
        length = self.array_length()
        if length is not None:
            return f"{member(self.value, 'Data').GetType().GetArrayElementType().GetName()}[{length}]"
        return self.value.GetValue() or self.value.GetTypeName()

    def type_name(self):
        return self.raw.GetType().GetTemplateArgumentType(0).GetName() if is_reference(self.raw) else self.raw.GetTypeName()

    def expandable(self):
        return self.array_length() is not None or next(self.children(), None) is not None

    def assign(self, text):
        error = lldb.SBError()
        if not self.raw.SetValueFromCString(text, error):
            raise ValueError(f"Cannot assign this managed value: {error}")
        self.value = dereference(self.raw, self.frame)
