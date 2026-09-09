using System.Security.Cryptography;
using Mono.Cecil;

internal static class CryptoRuntime
{
    private const string InputSha256 = "88B8E480752C77B08D8CE25982733D71B77BCBCB2A0FEAC0B687531732F3C9AF";

    internal static ModuleDefinition Read(string path)
    {
        byte[] input = File.ReadAllBytes(path);
        if (Convert.ToHexString(SHA256.HashData(input)) != InputSha256)
            throw new InvalidOperationException("Expected the pinned Microsoft.NETCore.App.Runtime.linux-x64 10.0.11 cryptography assembly; restore the SDK's exact package input.");
        var module = ModuleDefinition.ReadModule(new MemoryStream(input, writable: false));
        // The pinned package is ReadyToRun; NativeAOT consumes its managed IL.
        module.Attributes |= ModuleAttributes.ILOnly;
        module.Architecture = TargetArchitecture.I386;
        return module;
    }
}
