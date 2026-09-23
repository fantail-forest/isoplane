"""Present guest application frames through GDB's public frame-filter API."""
import json
import os
import sys
from pathlib import Path

import gdb
from gdb.FrameDecorator import FrameDecorator

sys.path.insert(0, str(Path(__file__).parent))
import isoplane_values
import isoplane_async


class SavedLocal:
    def __init__(self, frame, name):
        self.frame = frame
        self.name = name

    def symbol(self):
        return self.name

    def value(self):
        return isoplane_values.dereference(self.frame.read_var("__locals"), self.frame)[self.name]


class ApplicationFrame(FrameDecorator):
    def __init__(self, frame, method):
        super().__init__(frame)
        self.method = method

    def function(self):
        return self.method["Name"]

    def frame_args(self):
        return (value for value in (super().frame_args() or ()) if str(value.symbol()) != "__locals")

    def frame_locals(self):
        for value in super().frame_locals() or ():
            if str(value.symbol()) != "__vmctx":
                yield value
        frame = self.inferior_frame()
        symbol, _ = gdb.lookup_symbol("__locals", frame.block())
        if symbol is not None:
            saved = isoplane_values.dereference(frame.read_var(symbol), frame)
            for name, _ in isoplane_values.fields(saved):
                yield SavedLocal(frame, name)


class ApplicationFrames:
    name = "isoplane-application"
    priority = 100
    enabled = True

    def __init__(self):
        self.sources = None
        self.async_methods = {}

    def filter(self, frames):
        if self.sources is None:
            root = Path(os.environ["ISOPLANE_NATIVE_DEBUG_DIRECTORY"])
            self.sources = {}
            for index in root.glob("*/isoplane.debug-sources.json"):
                with index.open() as stream:
                    for method in json.load(stream):
                        self.sources.setdefault(os.path.normpath(method["Source"]), []).append(method)
                        if method.get("Async") is not None:
                            self.async_methods[method["Async"]["StateMachineType"]] = method["Async"]
            if not self.sources:
                self.sources = None
                raise gdb.GdbError("The local session has no application source index.")
        first = True
        for frame in frames:
            source, line = frame.filename(), frame.line()
            if source is None or line is None:
                continue
            matches = [method for method in self.sources.get(os.path.normpath(source), ())
                       if method["StartLine"] <= line <= method["EndLine"]]
            if matches:
                # A nested lambda can share its enclosing method's source range.
                method = min(matches, key=lambda method: method["EndLine"] - method["StartLine"])
                application = ApplicationFrame(frame, method)
                yield application
                if first and method.get("Async") is not None:
                    yield from isoplane_async.callers(application, self.async_methods)
                first = False


gdb.frame_filters[ApplicationFrames.name] = ApplicationFrames()
