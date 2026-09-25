using System.Diagnostics;
using System.Text.Json;
using Isoplane.Cli;
using Isoplane.Native;
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
    [InlineData(null, 262144UL)]
    [InlineData(524288UL, 524288UL)]
    public void doctor_flags_override_only_the_corresponding_manifest_policy(ulong? memory, ulong expectedMemory)
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-policy-");
        try
        {
            File.WriteAllText(Path.Combine(directory.FullName, "isoplane.toml"), "limits = { module_bytes_compressed = 5242880, package_bytes = 5242880, instance_memory_max = 67108864, cpu_ms_per_http_dispatch = 10, subrequests_per_dispatch = 20, pending_futures_per_dispatch = 6 }\nguest_memory_bytes=262144\n");
            var policy = ModuleDoctor.ResolvePolicy(Path.Combine(directory.FullName, "app.wasm"), memory);
            var report = ModuleValidator.Validate(Module, JsonSerializer.SerializeToUtf8Bytes(policy.Settings));
            Assert.True(report["limits"]!["instance_memory_max"]!.GetValue<ulong>() == 67108864UL
                && report["guest_memory_bytes"]!.GetValue<ulong>() == expectedMemory,
                "An explicit doctor flag must override its manifest setting without changing the other policy.");
        }
        finally { directory.Delete(recursive: true); }
    }

    [Fact]
    public async Task module_doctor_uses_the_library_without_a_native_cli()
    {
        var directory = Directory.CreateTempSubdirectory("isoplane-validation-");
        using var deadline = new CancellationTokenSource(TimeSpan.FromSeconds(30));
        try
        {
            var module = Path.Combine(directory.FullName, "module.wasm");
            await File.WriteAllBytesAsync(module, Module, deadline.Token);
            await File.WriteAllTextAsync(Path.Combine(directory.FullName, "isoplane.toml"),
                "limits = { module_bytes_compressed = 5242880, package_bytes = 5242880, instance_memory_max = 67108864, cpu_ms_per_http_dispatch = 10, subrequests_per_dispatch = 20, pending_futures_per_dispatch = 6 }\n[validation_limits]\nmax_functions=1\n", deadline.Token);
            var start = new ProcessStartInfo("dotnet")
            {
                WorkingDirectory = directory.FullName,
                UseShellExecute = false,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
            };
            foreach (var argument in new[] { typeof(Toolchain).Assembly.Location, "doctor", module, "--json" })
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
                    "The managed command must resolve manifest policy and return the native validator's verdict.");
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
}
