"""Exercise managed helper initialization failures independently of LLDB installation."""
import importlib.util
from pathlib import Path
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class NativeDebuggerInitializationTests(unittest.TestCase):
    def test_failed_command_registration_reports_the_debugger_error(self):
        result = SimpleNamespace(
            Succeeded=lambda: False,
            GetError=lambda: "command registration unavailable",
        )
        interpreter = SimpleNamespace(HandleCommand=lambda command, output: None)
        debugger = SimpleNamespace(GetCommandInterpreter=lambda: interpreter)
        path = Path(__file__).resolve().parents[2] / "tools/Isoplane.Cli/Debugging/isoplane_lldb.py"
        specification = importlib.util.spec_from_file_location("isoplane_lldb", path)
        module = importlib.util.module_from_spec(specification)
        with patch.dict(sys.modules, {"lldb": SimpleNamespace(SBCommandReturnObject=lambda: result)}), \
                patch.object(sys, "path", sys.path.copy()):
            specification.loader.exec_module(module)
            with self.assertRaisesRegex(RuntimeError, "command registration unavailable",
                                        msg="Helper initialization must surface LLDB registration failures"):
                getattr(module, "__lldb_init_module")(debugger, {})


if __name__ == "__main__":
    unittest.main()
