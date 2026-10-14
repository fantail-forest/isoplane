using System.Net;
using System.Net.Http.Headers;
using System.Text.Json;

namespace Isoplane.Cli;

/// <summary>Recorded events waiting to be sent, one file each so that concurrent commands need no lock.</summary>
internal sealed class TelemetryOutbox(string directory, TimeProvider time)
{
    // An installation that never reaches the service stops recording here instead of filling the disk.
    internal const int Capacity = 1000;
    internal const int BatchSize = 100;
    internal static readonly TimeSpan LongestWait = TimeSpan.FromDays(30);

    internal void Add(Guid id, string json)
    {
        Directory.CreateDirectory(directory);
        if (Pending().Take(Capacity).Count() == Capacity) return;
        var temporary = Path.Combine(directory, id.ToString("D") + ".tmp");
        File.WriteAllText(temporary, json);
        File.Move(temporary, Path.Combine(directory, id.ToString("D") + ".json"), overwrite: true);
    }

    internal void Clear()
    {
        if (Directory.Exists(directory)) Directory.Delete(directory, recursive: true);
    }

    /// <summary>
    /// Sends what is waiting and removes what the service took or will never take. Two commands sending
    /// the same event at once is harmless: the service stores an event identifier once.
    /// </summary>
    internal async Task SendAsync(HttpClient client, Uri endpoint, CancellationToken cancellation)
    {
        if (!Directory.Exists(directory)) return;
        var oldest = time.GetUtcNow().UtcDateTime - LongestWait;
        foreach (var batch in Pending().ToArray().Chunk(BatchSize))
        {
            var waiting = batch.Where(path => !Expire(path, oldest)).Select(path => (Path: path, Event: Read(path)))
                .Where(entry => entry.Event is not null).ToArray();
            if (waiting.Length == 0) continue;
            using var content = new ByteArrayContent(Envelope(waiting.Select(entry => entry.Event!)));
            content.Headers.ContentType = new MediaTypeHeaderValue("application/json");
            using var response = await client.PostAsync(endpoint, content, cancellation);
            // A refusal of the events themselves is final; anything else may succeed on a later run.
            if (!response.IsSuccessStatusCode && response.StatusCode is not (HttpStatusCode.BadRequest or HttpStatusCode.RequestEntityTooLarge))
                return;
            foreach (var entry in waiting) File.Delete(entry.Path);
        }
    }

    private IEnumerable<string> Pending() => Directory.EnumerateFiles(directory, "*.json");

    private static bool Expire(string path, DateTime oldest)
    {
        if (File.GetLastWriteTimeUtc(path) >= oldest) return false;
        File.Delete(path);
        return true;
    }

    private static byte[]? Read(string path)
    {
        try
        {
            var json = File.ReadAllBytes(path);
            using var parsed = JsonDocument.Parse(json);
            return json;
        }
        catch (JsonException)
        {
            // Nothing can make a damaged event sendable, and keeping it would resend the failure forever.
            File.Delete(path);
            return null;
        }
        // Another command sent and removed it between the listing and the read.
        catch (FileNotFoundException) { return null; }
    }

    private static byte[] Envelope(IEnumerable<byte[]> events)
    {
        using var buffer = new MemoryStream();
        using (var writer = new Utf8JsonWriter(buffer))
        {
            writer.WriteStartObject();
            writer.WriteStartArray("events");
            foreach (var json in events) writer.WriteRawValue(json, skipInputValidation: true);
            writer.WriteEndArray();
            writer.WriteEndObject();
        }
        return buffer.ToArray();
    }
}
