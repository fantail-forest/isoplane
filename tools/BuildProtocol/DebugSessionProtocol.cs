using System.Net;
using System.Text.Json;

namespace Isoplane.Build;

internal static class DebugSessionProtocol
{
    private const int Version = 1;
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);
    private sealed record Endpoint(int Version, int Port);
    private sealed record Command(int Version, string Action);
    internal sealed record Reply(int Version, string Action, string? Error = null);

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

    internal static Task RequestDetachAsync(Stream stream, bool prepared, CancellationToken cancellation) =>
        WriteAsync(stream, new Command(Version, prepared ? "detach" : "prepareDetach"), cancellation);

    internal static Task ReplyAsync(Stream stream, string action, string? error, CancellationToken cancellation) =>
        WriteAsync(stream, new Reply(Version, action, error), cancellation);

    internal static async Task<Reply?> ReadReplyAsync(Stream stream, CancellationToken cancellation)
    {
        var reply = await ReadAsync<Reply>(stream, cancellation);
        if (reply is not null) RequireVersion(reply.Version);
        return reply;
    }

    internal static async Task<bool> ReadTerminationAsync(Stream stream, Func<CancellationToken, Task>? prepareDetach, CancellationToken cancellation)
    {
        bool prepared = false;
        while (true)
        {
            var command = await ReadAsync<Command>(stream, cancellation);
            if (command is null) return false;
            RequireVersion(command.Version);
            switch (command.Action)
            {
                case "terminate": return false;
                case "prepareDetach" when !prepared:
                    try
                    {
                        if (prepareDetach is null) throw new InvalidOperationException("This local session cannot detach from its debugger.");
                        await prepareDetach(cancellation);
                        prepared = true;
                        await ReplyAsync(stream, "detachReady", null, cancellation);
                    }
                    catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException)
                    {
                        await ReplyAsync(stream, "detachReady", error.Message, cancellation);
                    }
                    break;
                case "detach" when prepared:
                    await ReplyAsync(stream, "detached", null, cancellation);
                    return true;
                default: throw new IOException("The debugger sent an unsupported session command.");
            }
        }
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
