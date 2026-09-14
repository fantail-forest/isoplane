using System.Diagnostics;
using Wisp.Cli;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class DoctorTests
{
    [Fact]
    public async Task Doctor_announces_checks_before_reporting_a_missing_installation()
    {
        var home = Directory.CreateTempSubdirectory("isoplane-doctor-").FullName;
        try
        {
            var start = new ProcessStartInfo("dotnet")
            {
                UseShellExecute = false,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };
            start.ArgumentList.Add(typeof(Toolchain).Assembly.Location);
            start.ArgumentList.Add("doctor");
            start.Environment["ISOPLANE_HOME"] = home;
            using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start doctor.");
            var output = process.StandardOutput.ReadToEndAsync();
            var errors = process.StandardError.ReadToEndAsync();
            using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
            try
            {
                await process.WaitForExitAsync(deadline.Token);
                Assert.True(process.ExitCode != 0, "Doctor must fail when no toolchain is installed.");
                Assert.True((await errors).StartsWith("Checking toolchain integrity...", StringComparison.Ordinal),
                    "Doctor must announce its work before an installation check can fail.");
                Assert.True(string.IsNullOrEmpty(await output), "A failed doctor must not report a ready toolchain.");
            }
            finally
            {
                if (!process.HasExited)
                {
                    process.Kill(entireProcessTree: true);
                    await process.WaitForExitAsync();
                }
            }
        }
        finally
        {
            Directory.Delete(home, recursive: true);
        }
    }

    [Fact]
    public async Task A_successful_tool_check_does_not_print_tool_output()
    {
        var start = new ProcessStartInfo("dotnet")
        {
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        start.ArgumentList.Add(typeof(Wisp.Cli.Tests.OwnedChild).Assembly.Location);
        start.ArgumentList.Add("--successful-doctor-probe");
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start the doctor probe.");
        var output = process.StandardOutput.ReadToEndAsync();
        var errors = process.StandardError.ReadToEndAsync();
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        try
        {
            await process.WaitForExitAsync(deadline.Token);
            Assert.True(process.ExitCode == 0, $"A working tool must pass its check: {await errors}");
            Assert.True(string.IsNullOrEmpty(await output) && string.IsNullOrEmpty(await errors),
                "Successful tool checks must keep version and diagnostic dumps out of the terminal.");
        }
        finally
        {
            if (!process.HasExited)
            {
                process.Kill(entireProcessTree: true);
                await process.WaitForExitAsync();
            }
        }
    }

    [Fact]
    public async Task A_failed_tool_check_retains_stdout_and_stderr_diagnostics()
    {
        var runtime = Path.Combine(AppContext.BaseDirectory, "runtime", "wisp");
        var error = await Assert.ThrowsAsync<InvalidOperationException>(() => Processes.CheckAsync(
            "/bin/sh", ["-c", "echo version-detail; echo missing-library >&2; exit 7"],
            AppContext.BaseDirectory, runtime, CancellationToken.None));
        Assert.True(error.Message.Contains("version-detail", StringComparison.Ordinal)
            && error.Message.Contains("missing-library", StringComparison.Ordinal)
            && error.Message.Contains("exit 7", StringComparison.Ordinal),
            "A failed tool check must preserve both diagnostic streams and the failing exit code.");
    }
}
