"""Keep the adapter's managed watchpoint extension aligned with the runtime."""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class ManagedDebuggerProtocolTests(unittest.TestCase):
    def test_managed_watchpoint_commands_match_the_runtime(self):
        header = ROOT / "vendor/dotnet-runtime/src/mono/mono/component/debugger-protocol.h"
        runtime = {name: int(value) for name, value in
                   re.findall(r"\b(MDBGPROT_\w+)\s*=\s*(\d+)", header.read_text())}
        backend = (ROOT / "tools/Isoplane.Debugging/Backend/ManagedDataBreakpoint.cs").read_text()
        adapter = {name: int(value) for name, value in re.findall(r"const (?:int|byte) (\w+) = (\d+);", backend)}
        counterparts = {
            "ManagedDataBreakpointCommand": "MDBGPROT_CMD_VM_MANAGED_DATA_BREAKPOINTS",
            "ManagedDataBreakpointSet": "MDBGPROT_DATA_BREAKPOINT_SET",
            "ManagedDataBreakpointTakeHits": "MDBGPROT_DATA_BREAKPOINT_TAKE_HITS",
            "ManagedDataBreakpointField": "MDBGPROT_DATA_BREAKPOINT_FIELD",
            "ManagedDataBreakpointArray": "MDBGPROT_DATA_BREAKPOINT_ARRAY",
        }
        self.assertEqual(adapter.keys(), counterparts.keys(), "every managed debugger protocol constant needs a runtime counterpart")
        for managed, native in counterparts.items():
            self.assertIn(native, runtime, "the runtime must implement each advertised managed debugger command")
            self.assertEqual(adapter[managed], runtime[native], f"{managed} must use the runtime's wire value")


if __name__ == "__main__":
    unittest.main()
