using System.Formats.Tar;
using System.Text.Json;

namespace Isoplane.Build;

/// <summary>Reads an uploaded managed bundle's manifest without extracting the bundle.</summary>
internal static class BundleManifest
{
    /// <summary>Bounds reading a hostile manifest; a genuine one names an assembly and a factory.</summary>
    internal const int MaximumBytes = 64 * 1024;

    /// <summary>
    /// The manifest of <paramref name="bundle"/>, or null when it is not a readable
    /// managed bundle, which the compiler then refuses with its own diagnostics.
    /// </summary>
    internal static ManagedApplication? TryRead(byte[] bundle)
    {
        try
        {
            using var reader = new TarReader(new MemoryStream(bundle, writable: false));
            while (reader.GetNextEntry() is { } entry)
            {
                if (entry.Name != "application.json") continue;
                if (entry.EntryType is not (TarEntryType.RegularFile or TarEntryType.V7RegularFile) || entry.Length > MaximumBytes || entry.DataStream is null)
                    return null;
                return JsonSerializer.Deserialize<ManagedApplication>(entry.DataStream, BuildJson.Options);
            }
            return null;
        }
        catch (Exception error) when (error is InvalidDataException or JsonException or FormatException or EndOfStreamException)
        {
            return null;
        }
    }
}
