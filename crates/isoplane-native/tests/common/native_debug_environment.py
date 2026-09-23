"""Assert that a build launched under LLDB retains the customer's Python environment."""
import json
import os
from pathlib import Path
import sys

expected = json.loads(Path(sys.argv[1]).read_text())
actual = {name: value for name, value in os.environ.items() if name.upper().startswith('PYTHON')}
assert actual == expected, "the debugger's Python configuration must not leak into customer build commands"
