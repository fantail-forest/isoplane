"""Assemble shared normal and composed async guest sources before compilation."""
from pathlib import Path

def replace_once(source, original, replacement):
    if source.count(original) != 1:
        raise ValueError(f"The async fixture must contain one substitution point: {original}")
    return source.replace(original, replacement)


here = Path(__file__).resolve().parent
output = here.parents[3] / ".build/clr-anycpu/debugger-sources"
output.mkdir(parents=True, exist_ok=True)
normal = (here / "async.cs.in").read_text(encoding="utf-8")
composed = replace_once(normal,
    "int result = await Inner(outer, outerSample, outerNumbers).ConfigureAwait(false);",
    "Task<int> child = Inner(outer, outerSample, outerNumbers);\n"
    "        Task<int[]> combined = Task.WhenAll(child, Task.FromResult(0));\n"
    "        Task<Task<int[]>> choice = Task.WhenAny(combined, new TaskCompletionSource<int[]>().Task);\n"
    "        Task<int[]> winner = await choice;\n"
    "        int result = (await winner.ConfigureAwait(false))[0];\n"
    "        EnsureTrackingReleased(child, combined, choice);",
)
composed = replace_once(composed,
    'Task delay = Task.Delay(100);\n        if (delay.IsCompleted) throw new InvalidOperationException("the host timer must be pending before await");\n        await delay;',
    "await new DebugYield();",
) + (here / "awaiter.cs.in").read_text(encoding="utf-8")
for route, namespace, body in [("async-stack", "Async", normal), ("async-composed", "Composed", composed)]:
    source = ("using System;\nusing System.Threading.Tasks;\nusing Microsoft.AspNetCore.Builder;\n\n"
              f"namespace Debugger.{namespace};\n\ninternal static class Application\n{{\n"
              f'    internal static void Map(WebApplication app) => app.MapGet("/{route}", Handle);\n\n'
              + body + "}\n")
    (output / f"{route}.cs").write_text(source, encoding="utf-8", newline="\n")
