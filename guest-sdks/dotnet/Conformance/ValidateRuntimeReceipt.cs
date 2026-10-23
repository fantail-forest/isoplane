using System.Text.Json;
using Microsoft.Build.Framework;

namespace Isoplane.Conformance;

/// <summary>Checks that the SDK's packaged runtime modules match their JSON receipt.</summary>
public sealed class ValidateRuntimeReceipt : Microsoft.Build.Utilities.Task
{
    /// <summary>Runtime identity written by the shared runtime build.</summary>
    [Required]
    public string ReceiptFile { get; set; } = "";

    /// <summary>SDK package version the runtime must match.</summary>
    [Required]
    public string SdkVersion { get; set; } = "";

    /// <summary>Source revision the runtime must match in a CI build.</summary>
    public string SourceRevision { get; set; } = "";

    /// <summary>Requires the runtime to have been built from the SDK's exact source revision.</summary>
    public bool ContinuousIntegrationBuild { get; set; }

    /// <summary>Modules being packaged, with SHA256 digests in their FileHash metadata.</summary>
    [Required]
    public ITaskItem[] Modules { get; set; } = [];

    /// <summary>Parses the receipt and reports invalid identity or module hashes as build errors.</summary>
    public override bool Execute()
    {
        try
        {
            using var stream = File.OpenRead(ReceiptFile);
            using var document = JsonDocument.Parse(stream, new JsonDocumentOptions { AllowDuplicateProperties = false });
            Validate(document.RootElement);
        }
        catch (Exception error) when (error is JsonException or InvalidDataException or IOException or UnauthorizedAccessException)
        {
            Log.LogError("Invalid runtime receipt {0}: {1}", ReceiptFile, error.Message);
        }
        return !Log.HasLoggedErrors;
    }

    private void Validate(JsonElement receipt)
    {
        if (receipt.ValueKind != JsonValueKind.Object)
            throw new InvalidDataException("runtime.json must contain an object.");
        var version = ReadString(receipt, "sdkVersion");
        var revision = ReadString(receipt, "sourceRevision");
        if (!string.Equals(version, SdkVersion, StringComparison.Ordinal))
            throw new InvalidDataException($"The shared runtimes were built for SDK {version}, not {SdkVersion}; rebuild them from this source.");
        if (ContinuousIntegrationBuild && !string.Equals(revision, SourceRevision, StringComparison.Ordinal))
            throw new InvalidDataException($"The shared runtimes were built from {revision}, not {SourceRevision}; rebuild them from this source.");
        var modules = ReadProperty(receipt, "modules", JsonValueKind.Object);
        foreach (var module in Modules)
        {
            var name = Path.GetFileName(module.ItemSpec);
            var record = ReadProperty(modules, name, JsonValueKind.Object);
            var digest = ReadString(record, "sha256");
            if (digest.Length != 64 || !digest.All(char.IsAsciiHexDigit))
                throw new InvalidDataException($"modules.{name}.sha256 must contain a SHA256 hexadecimal digest.");
            if (!string.Equals(digest, module.GetMetadata("FileHash"), StringComparison.OrdinalIgnoreCase))
                throw new InvalidDataException($"{name} is not the module runtime.json records; rebuild the shared runtimes.");
        }
    }

    private static string ReadString(JsonElement parent, string name)
    {
        var value = ReadProperty(parent, name, JsonValueKind.String).GetString()!;
        if (string.IsNullOrWhiteSpace(value))
            throw new InvalidDataException($"{name} must contain a nonempty string.");
        return value;
    }

    private static JsonElement ReadProperty(JsonElement parent, string name, JsonValueKind kind)
    {
        if (!parent.TryGetProperty(name, out var value) || value.ValueKind != kind)
            throw new InvalidDataException($"runtime.json requires {name} to be a JSON {kind}.");
        return value;
    }
}
