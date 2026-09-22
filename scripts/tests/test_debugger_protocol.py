"""Keep the adapter's managed watchpoint extension aligned with the runtime."""
import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class ManagedDebuggerProtocolTests(unittest.TestCase):
    def test_managed_watchpoint_commands_match_the_runtime_patch_series(self):
        runtime = {}
        declaration = re.compile(r"\b(MDBGPROT_\w+)\s*=\s*(\d+)")
        for patch in sorted((ROOT / "guest-runtime/patches/runtime").glob("*.patch")):
            for line in patch.read_text().splitlines():
                match = declaration.search(line)
                if not match:
                    continue
                name, value = match.groups()
                if line.startswith("-"):
                    runtime.pop(name, None)
                elif line.startswith("+"):
                    runtime[name] = int(value)
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
