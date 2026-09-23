"""Read managed values using the compiler's layouts and Wasmtime's DWARF memory description."""
import json

import gdb


def memory_base(frame=None):
    frame = frame or gdb.selected_frame()
    while frame is not None:
        try:
            return frame.read_var("__vmctx")["memory"]
        except (gdb.error, ValueError):
            # Native transition frames do not describe a guest memory context.
            frame = frame.older()
    raise gdb.GdbError("The selected stack has no guest memory context.")


def is_reference(value):
    return str(value.type.strip_typedefs()).startswith(("WebAssemblyRefWrapper<", "WebAssemblyPtrWrapper<"))


def dereference(value, frame=None):
    if not is_reference(value):
        return value
    offset = int(value["__ptr"])
    if offset == 0:
        return None
    target = value.type.strip_typedefs().template_argument(0)
    return (memory_base(frame) + offset).cast(target.pointer()).dereference()


def fields(value):
    for field in value.type.strip_typedefs().fields():
        if field.is_base_class:
            if str(field.type) != "System_Object":
                yield from fields(value.cast(field.type))
        elif field.name is not None:
            yield field.name, value[field]


class ManagedObject(gdb.ValuePrinter):
    def __init__(self, value):
        self.value = dereference(value)
        self.members = [] if self.value is None else list(fields(self.value))

    def to_string(self):
        return "null" if self.value is None else str(self.value.type)

    def num_children(self):
        return len(self.members)

    def child(self, index):
        return self.members[index]

    def children(self):
        return iter(self.members)


class ManagedArray(gdb.ValuePrinter):
    def __init__(self, value):
        self.value = dereference(value)

    def to_string(self):
        return "null" if self.value is None else f"{self.value['Data'].type.target()}[{self.num_children()}]"

    def num_children(self):
        return 0 if self.value is None else int(self.value["Length"])

    def display_hint(self):
        return "array"

    def child(self, index):
        return f"[{index}]", self.value["Data"][index]

    def children(self):
        return (self.child(index) for index in range(self.num_children()))


class ManagedString:
    def __init__(self, value):
        self.value = dereference(value)

    def to_string(self):
        if self.value is None:
            return "null"
        length = int(self.value["_stringLength"])
        address = self.value["_firstChar"].address
        data = gdb.selected_inferior().read_memory(address, length * 2)
        text = bytes(data).decode("utf-16-le", errors="surrogatepass")
        return json.dumps(text, ensure_ascii=False).encode("utf-8", errors="backslashreplace").decode("utf-8")


def visualizer(value):
    if not is_reference(value) or value.is_optimized_out or getattr(value, "is_unavailable", False):
        return None
    target = str(value.type.strip_typedefs().template_argument(0))
    if target == "System_String":
        return ManagedString(value)
    if target.startswith("__Array<"):
        return ManagedArray(value)
    return ManagedObject(value)


gdb.pretty_printers.append(visualizer)
