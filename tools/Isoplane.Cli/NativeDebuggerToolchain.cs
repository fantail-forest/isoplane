namespace Isoplane.Cli;

internal sealed record NativeDebuggerToolchain(string Executable, IReadOnlyDictionary<string, string> Environment)
{
    internal static NativeDebuggerToolchain Resolve(string? toolchain)
    {
        if (toolchain is null) return new("lldb-dap", new Dictionary<string, string>());
        var root = Path.Combine(toolchain, "debugger");
        var executable = Path.Combine(root, "bin", "lldb-dap" + Toolchain.ExecutableExtension);
        var python = Path.Combine(root, "python");
        var bindings = Path.Combine(root, "bindings");
        if (!File.Exists(executable) || !Directory.Exists(python) || !File.Exists(Path.Combine(bindings, "lldb", "__init__.py")))
            throw new InvalidOperationException("The installed NativeAOT debugger is incomplete. Run isoplane setup to repair the toolchain.");
        var environment = new Dictionary<string, string>
        {
            ["PYTHONHOME"] = python,
            ["PYTHONPATH"] = bindings,
            ["PYTHONNOUSERSITE"] = "1",
            ["PYTHONDONTWRITEBYTECODE"] = "1",
            ["PATH"] = Path.Combine(root, "bin") + Path.PathSeparator + python + Path.PathSeparator + System.Environment.GetEnvironmentVariable("PATH")
        };
        if (OperatingSystem.IsLinux())
            environment["LD_LIBRARY_PATH"] = Path.Combine(root, "lib") + Path.PathSeparator + Path.Combine(python, "lib")
                + (System.Environment.GetEnvironmentVariable("LD_LIBRARY_PATH") is { Length: > 0 } existing ? Path.PathSeparator + existing : "");
        if (OperatingSystem.IsMacOS()) environment["LLDB_DEBUGSERVER_PATH"] = Path.Combine(root, "bin", "debugserver");
        return new(executable, environment);
    }
}
