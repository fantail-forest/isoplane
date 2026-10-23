using System.Net;
using System.Text.Json;

namespace Isoplane.Build;

internal sealed record DebugProject(string Path, Dictionary<string, string> Properties);

internal static class DebugSessionProtocol
{
    private const int Version = 3;
    private static readonly JsonSerializerOptions Json = new(JsonSerializerDefaults.Web);
    private sealed record Endpoint(int Version, int Port, string Package, DebugProject? Project);
    internal sealed record SessionEndpoint(IPEndPoint Guests, string Package, DebugProject? Project);
    private sealed record Command(int Version, string Action);
    internal sealed record Reply(int Version, string Action, string? Error = null);

    /// <summary>Opens a session: where its guests' debugger agents connect, and the application package they run.</summary>
    internal static Task WriteEndpointAsync(Stream stream, IPEndPoint endpoint, string package, DebugProject? project, CancellationToken cancellation) =>
        WriteAsync(stream, new Endpoint(Version, endpoint.Port, package, project), cancellation);

    internal static Task TerminateAsync(Stream stream) => WriteAsync(stream, new Command(Version, "terminate"), CancellationToken.None);

    internal static Task RequestDetachAsync(Stream stream, bool prepared, CancellationToken cancellation) =>
        WriteAsync(stream, new Command(Version, prepared ? "detach" : "prepareDetach"), cancellation);

    /// <summary>The reply action that tells the debugger the session rebuilt its application package in place.</summary>
    internal const string PackageChanged = "packageChanged";

    /// <summary>Writes one message to the debugger. <paramref name="writes"/> orders it among the session's other messages.</summary>
    internal static async Task ReplyAsync(Stream stream, SemaphoreSlim writes, string action, string? error, CancellationToken cancellation)
    {
        await writes.WaitAsync(cancellation);
        try { await WriteAsync(stream, new Reply(Version, action, error), cancellation); }
        finally { writes.Release(); }
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

    /// <summary>Preserves buffered control messages for the lifetime of one connection.</summary>
    internal sealed class Reader(Stream stream) : IDisposable
    {
        private readonly StreamReader reader = new(stream, leaveOpen: true);

        internal async Task<SessionEndpoint> ReadEndpointAsync(CancellationToken cancellation)
        {
            var endpoint = await ReadAsync<Endpoint>(cancellation)
                ?? throw new IOException("The local session closed before providing a debugger endpoint.");
            RequireVersion(endpoint.Version);
            if (endpoint.Port is < 1 or > 65535) throw new IOException("The local session returned an invalid debugger port.");
            if (!Path.IsPathFullyQualified(endpoint.Package ?? "")) throw new IOException("The local session did not name its application package.");
            if (endpoint.Project is { } project && (!Path.IsPathFullyQualified(project.Path ?? "") || project.Properties is null))
                throw new IOException("The local session returned an invalid Hot Reload project.");
            return new(new IPEndPoint(IPAddress.Loopback, endpoint.Port), endpoint.Package!, endpoint.Project);
        }

        internal async Task<Reply?> ReadReplyAsync(CancellationToken cancellation)
        {
            var reply = await ReadAsync<Reply>(cancellation);
            if (reply is not null) RequireVersion(reply.Version);
            return reply;
        }

        internal async Task<bool> ReadTerminationAsync(SemaphoreSlim writes, Func<CancellationToken, Task>? prepareDetach, CancellationToken cancellation)
        {
            bool prepared = false;
            while (true)
            {
                var command = await ReadAsync<Command>(cancellation);
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
                            await ReplyAsync(reader.BaseStream, writes, "detachReady", null, cancellation);
                        }
                        catch (Exception error) when (error is InvalidOperationException or IOException or UnauthorizedAccessException)
                        {
                            await ReplyAsync(reader.BaseStream, writes, "detachReady", error.Message, cancellation);
                        }
                        break;
                    case "detach" when prepared:
                        await ReplyAsync(reader.BaseStream, writes, "detached", null, cancellation);
                        return true;
                    default: throw new IOException("The debugger sent an unsupported session command.");
                }
            }
        }

        /// <summary>Releases buffered input while leaving the connection stream open.</summary>
        public void Dispose() => reader.Dispose();

        private async Task<T?> ReadAsync<T>(CancellationToken cancellation) where T : class
        {
            var line = await reader.ReadLineAsync(cancellation);
            try { return line is null ? null : JsonSerializer.Deserialize<T>(line, Json) ?? throw new IOException("The local debugger control message was empty."); }
            catch (JsonException error) { throw new IOException("The local debugger control message was invalid.", error); }
        }
    }
}
