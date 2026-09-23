using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class NativeDebuggerToolchainTests
{
    [Fact]
    public void an_incomplete_installed_debugger_cannot_fall_back_to_the_machine_path()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-debugger-");
        try
        {
            var error = Assert.Throws<InvalidOperationException>(() => NativeDebuggerToolchain.Resolve(directory.FullName));
            Assert.True(error.Message.Contains("isoplane setup", StringComparison.Ordinal), "An incomplete installed debugger must require repair rather than use an unrelated system LLDB.");
        }
        finally { directory.Delete(recursive: true); }
    }

    [Fact]
    public void bundled_python_settings_are_scoped_to_the_debugger_child()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-debugger-");
        var original = Environment.GetEnvironmentVariable("PYTHONHOME");
        try
        {
            var root = Path.Combine(directory.FullName, "debugger");
            Directory.CreateDirectory(Path.Combine(root, "bin"));
            Directory.CreateDirectory(Path.Combine(root, "python"));
            Directory.CreateDirectory(Path.Combine(root, "bindings", "lldb"));
            File.WriteAllText(Path.Combine(root, "bin", "lldb-dap" + Toolchain.ExecutableExtension), "test payload");
            File.WriteAllText(Path.Combine(root, "bindings", "lldb", "__init__.py"), "test bindings");
            var debugger = NativeDebuggerToolchain.Resolve(directory.FullName);
            Assert.True(Path.IsPathFullyQualified(debugger.Executable) && debugger.Environment["PYTHONHOME"] == Path.Combine(root, "python"),
                "An installed debugger must use its own executable and Python runtime.");
            Assert.True(Environment.GetEnvironmentVariable("PYTHONHOME") == original,
                "Selecting the debugger must not replace Python settings for the CLI or application.");
            Assert.True(debugger.Environment["PYTHONDONTWRITEBYTECODE"] == "1", "Debugging must not modify the integrity-checked toolchain with Python caches.");
        }
        finally { directory.Delete(recursive: true); }
    }
}
