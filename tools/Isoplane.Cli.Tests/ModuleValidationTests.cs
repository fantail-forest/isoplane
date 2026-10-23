using System.Diagnostics;
using System.Text.Json;
using System.Xml.Linq;
using Isoplane.Cli;
using Isoplane.Native;
using Isoplane.TestSupport;
using Xunit;

namespace Isoplane.Cli.Tests;

public sealed class ModuleValidationTests
{
    // Core WASM with one bounded memory and the three required contract exports.
    internal static byte[] Module => Convert.FromBase64String("AGFzbQEAAAABFANgAn9/AX9gAn9/AX5gA35/fwF+AwQDAAECBQQBAQEBBz0EBm1lbW9yeQIADmlzb3BsYW5lX2FsbG9jAAAPaXNvcGxhbmVfaGFuZGxlAAENaXNvcGxhbmVfd2FrZQACChADBABBAAsEAEIBCwQAQgEL");

    [Fact]
    public void repeated_native_inspections_return_independent_reports()
    {
        for (var iteration = 0; iteration < 32; iteration++)
        {
            var report = ModuleValidator.Validate(Module, "{}"u8.ToArray());
            Assert.True(!report["rejected"]!.GetValue<bool>() && report["violations"]!.AsArray().Count == 0,
                "A valid module must remain accepted after earlier report buffers have been released.");
        }
    }

    [Fact]
    public void native_validation_enforces_supplied_structural_limits()
    {
        var report = ModuleValidator.Validate(Module, "{\"validation_limits\":{\"max_functions\":1}}"u8.ToArray());
        Assert.True(report["rejected"]!.GetValue<bool>() && report["violations"]!.AsArray().Count > 0,
            "Managed validation must apply the real platform's structural admission limits.");
    }

    [Fact]
    public void malformed_module_input_returns_a_managed_error()
    {
        var error = Assert.Throws<ArgumentException>(() => ModuleValidator.Validate("invalid"u8.ToArray(), "{}"u8.ToArray()));
        Assert.True(error.Message.Contains("cannot inspect module", StringComparison.Ordinal),
            "An invalid module must report an inspection error without terminating the managed process.");
    }

    [Fact]
    public void invalid_policy_returns_a_managed_error()
    {
        var error = Assert.Throws<ArgumentException>(() => ModuleValidator.Validate(Module, "{\"limits\":\"missing\"}"u8.ToArray()));
        Assert.True(error.Message.Contains("invalid validation policy", StringComparison.Ordinal),
            "An unknown policy must fail before reporting a validation verdict.");
    }

    [Theory]
    [InlineData(524288UL, "the default", "--guest-memory-bytes")]
    public void each_doctor_flag_selects_only_its_own_policy(ulong? memory, string limitsFrom, string memoryFrom)
    {
        var policy = ModuleDoctor.ResolvePolicy(memory);
        var report = ModuleValidator.Validate(Module, JsonSerializer.SerializeToUtf8Bytes(policy.Settings));
        Assert.Equal((limitsFrom, memoryFrom), (policy.LimitsFrom, policy.MemoryFrom));
        if (memory is not null) Assert.Equal(memory.Value, report["guest_memory_bytes"]!.GetValue<ulong>());
    }

    [Fact]
    public async Task module_doctor_uses_the_library_without_a_native_cli()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-validation-");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        try
        {
            // An empty module: it has none of the exports an application needs.
            var module = Path.Combine(directory.FullName, "module.wasm");
            await File.WriteAllBytesAsync(module, Convert.FromBase64String("AGFzbQEAAAA="), deadline.Token);
            var start = new ProcessStartInfo("dotnet")
            {
                WorkingDirectory = directory.FullName,
                UseShellExecute = false,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };
            foreach (var argument in new[] { typeof(CliPaths).Assembly.Location, "doctor", module, "--guest-memory-bytes", "134217728", "--json" })
                start.ArgumentList.Add(argument);
            start.Environment["ISOPLANE_CLI"] = Path.Combine(directory.FullName, "nonexistent-isoplane");
            using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start module diagnostics.");
            var output = process.StandardOutput.ReadToEndAsync(deadline.Token);
            var errors = process.StandardError.ReadToEndAsync(deadline.Token);
            try
            {
                await process.WaitForExitAsync(deadline.Token);
                Assert.True(process.ExitCode == 2, $"A rejected module must return the SDK's rejection code: {await errors}");
                using var report = JsonDocument.Parse(await output);
                Assert.True(report.RootElement.GetProperty("rejected").GetBoolean(),
                    "The managed command must apply the selected plan and return the native validator's verdict.");
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
        finally { directory.Delete(recursive: true); }
    }

    [Theory]
    [InlineData(true)]
    [InlineData(false)]
    public async Task sdk_publication_validation_obeys_the_real_module_doctor_verdict(bool accepted)
    {
        using var directory = new FileSystemTestDirectory("isoplane-sdk-validation-");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(90));
        var root = Environment.GetEnvironmentVariable("ISOPLANE_BUILD_ROOT")
            ?? throw new InvalidOperationException("Run this suite with task test:managed:standard.");
        var module = Path.Combine(directory.Path, "module.wasm");
        await File.WriteAllBytesAsync(module, accepted ? Module : Convert.FromBase64String("AGFzbQEAAAA="), deadline.Token);
        var project = Path.Combine(directory.Path, "publication.proj");
        new XDocument(new XElement("Project",
            new XElement("PropertyGroup",
                new XElement("NativeBinary", module),
                new XElement("TargetName", "module"),
                new XElement("IsoplaneCliAssembly", typeof(CliPaths).Assembly.Location),
                new XElement("IsoplanePlan", "free")),
            new XElement("Import", new XAttribute("Project", Path.Combine(root, "guest-sdks/dotnet/Build/Isoplane.Validation.targets")))))
            .Save(project);
        var start = new ProcessStartInfo("dotnet")
        {
            WorkingDirectory = directory.Path,
            UseShellExecute = false,
            RedirectStandardOutput = true,
            RedirectStandardError = true,
        };
        foreach (var argument in new[] { "msbuild", project, "-nologo", "-nodeReuse:false", "-t:IsoplaneValidateModule" })
            start.ArgumentList.Add(argument);
        using var process = Process.Start(start) ?? throw new InvalidOperationException("Could not start SDK publication validation.");
        var output = process.StandardOutput.ReadToEndAsync(deadline.Token);
        var errors = process.StandardError.ReadToEndAsync(deadline.Token);
        try
        {
            await process.WaitForExitAsync(deadline.Token);
            var reported = await output + await errors;
            Assert.True(accepted ? process.ExitCode == 0 : process.ExitCode != 0, reported);
            Assert.Contains(accepted ? "passes validator rules V1-V4" : "ISOPLANE0001", reported);
            Assert.Contains("judged against the free plan", reported);
            if (!accepted) Assert.Contains("REJECTED", reported);
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
}
