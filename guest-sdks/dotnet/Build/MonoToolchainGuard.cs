using System;
using System.IO;
using System.Security.Cryptography;
using Microsoft.Build.Framework;

/// <summary>Checks the pinned upstream Mono target before the WISP toolchain override runs.</summary>
public sealed class WispMonoToolchainGuard : Microsoft.Build.Utilities.Task
{
    /// <summary>The actual workload target imported by this build.</summary>
    [Required] public string UpstreamTarget { get; set; }
    /// <summary>The explicitly selected wasi-sdk installation.</summary>
    [Required] public string Sdk { get; set; }

    /// <summary>The native SDK release validated by this profile.</summary>
    [Output] public string ExpectedSdkVersion => "29.0";

    /// <summary>Refuses changed workload bindings or an unsupported native compiler.</summary>
    public override bool Execute()
    {
        if (!File.Exists(UpstreamTarget))
        {
            Log.LogError("wisp: install the pinned wasi-experimental workload for .NET 10.0.400 (Mono 10.0.11).");
            return false;
        }
        using (var hash = SHA256.Create())
        {
            var actual = BitConverter.ToString(hash.ComputeHash(File.ReadAllBytes(UpstreamTarget))).Replace("-", "");
            if (actual != "12F7F9975D1F0D8B78A1EDA80C9EAC94F3656EEAEFB332CAEFC82594BDF29C37")
            {
                Log.LogError("wisp: Mono WasiApp.targets changed; this profile is bound to Microsoft.NET.Runtime.WebAssembly.Wasi.Sdk 10.0.11. Restore that workload before publishing.");
                return false;
            }
        }
        var version = Path.Combine(Sdk, "VERSION");
        var versions = File.Exists(version) ? File.ReadAllLines(version) : Array.Empty<string>();
        if (versions.Length == 0 || versions[0].Trim() != ExpectedSdkVersion)
        {
            Log.LogError($"wisp: the Mono core profile requires wasi-sdk {ExpectedSdkVersion}; set WASI_SDK_PATH to that installation.");
            return false;
        }
        return true;
    }
}
