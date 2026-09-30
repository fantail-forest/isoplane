using System.Net;
using System.Text.Json;

namespace Isoplane.Build;

internal static class DebugSessionProtocol
{
    private const int Version = 1;
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);
    private sealed record Endpoint(int Version, int Port);
    private sealed record Command(int Version, string Action);

    internal static Task WriteEndpointAsync(Stream stream, IPEndPoint endpoint, CancellationToken cancellation) =>
        WriteAsync(stream, new Endpoint(Version, endpoint.Port), cancellation);

    internal static async Task<IPEndPoint> ReadEndpointAsync(Stream stream, CancellationToken cancellation)
    {
        var endpoint = await ReadAsync<Endpoint>(stream, cancellation)
            ?? throw new IOException("The local session closed before providing a debugger endpoint.");
        RequireVersion(endpoint.Version);
        if (endpoint.Port is < 1 or > 65535) throw new IOException("The local session returned an invalid debugger port.");
        return new IPEndPoint(IPAddress.Loopback, endpoint.Port);
    }

    internal static Task TerminateAsync(Stream stream) => WriteAsync(stream, new Command(Version, "terminate"), CancellationToken.None);

    internal static async Task ReadTerminationAsync(Stream stream, CancellationToken cancellation)
    {
        var command = await ReadAsync<Command>(stream, cancellation);
        if (command is null) return;
        RequireVersion(command.Version);
        if (command.Action != "terminate") throw new IOException("The debugger sent an unsupported session command.");
    }

    private static void RequireVersion(int version)
    {
        if (version != Version) throw new IOException("The debugger and local session use incompatible control protocols; install a matching CLI and adapter.");
    }

    private static async Task WriteAsync<T>(Stream stream, T message, CancellationToken cancellation)
    {
        using var writer = new StreamWriter(stream, leaveOpen: true) { AutoFlush = true };
        await writer.WriteLineAsync(JsonSerializer.Serialize(message, Json).AsMemory(), cancellation);
    }

    private static async Task<T?> ReadAsync<T>(Stream stream, CancellationToken cancellation) where T : class
    {
        using var reader = new StreamReader(stream, leaveOpen: true);
        var line = await reader.ReadLineAsync(cancellation);
        try { return line is null ? null : JsonSerializer.Deserialize<T>(line, Json) ?? throw new IOException("The local debugger control message was empty."); }
        catch (JsonException error) { throw new IOException("The local debugger control message was invalid.", error); }
    }
}
