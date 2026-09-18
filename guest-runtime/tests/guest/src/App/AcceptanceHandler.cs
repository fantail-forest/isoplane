using System;
using System.Threading.Tasks;
using Isoplane;
using Isoplane.Durable;

namespace Acceptance;

/// <summary>
/// Routes a request to the family of runtime behaviour its first path
/// segment names. Every family answers with plain text the host asserts on;
/// an exception the family did not classify is reported by type name so a
/// test failure names the managed failure rather than a trap.
/// </summary>
public sealed class AcceptanceHandler : IIsoplaneHandler
{
    private readonly EventLoop _eventLoop = new();
    private readonly Sockets _sockets = new();
    private readonly NameResolution _names = new();
    private readonly Cryptography _cryptography = new();
    private readonly Http _http = new();
    private readonly Tls _tls = new();
    private readonly Compression _compression = new();
    private readonly Globalization _globalization = new();
    private readonly Hosting _hosting = new();
    private readonly Reflection _reflection = new();

    /// <summary>Dispatches on the family segment of the path.</summary>
    public async Task<Response> HandleAsync(Request request, Ctx context)
    {
        string[] parts = request.Path.Split('/', StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length == 0) return Response.Text("acceptance");
        string[] rest = parts[1..];
        try
        {
            string answer = parts[0] switch
            {
                "loop" => await _eventLoop.RunAsync(rest).ConfigureAwait(false),
                "sockets" => await _sockets.RunAsync(rest).ConfigureAwait(false),
                "dns" => await _names.RunAsync(rest).ConfigureAwait(false),
                "crypto" => await _cryptography.RunAsync(rest).ConfigureAwait(false),
                "http" => await _http.RunAsync(rest).ConfigureAwait(false),
                "protocols" => await Protocols.RunAsync(rest).ConfigureAwait(false),
                "legacy" => await LegacyNetworking.RunAsync(rest).ConfigureAwait(false),
                "tls" => await _tls.RunAsync(rest).ConfigureAwait(false),
                "compression" => await _compression.RunAsync(rest).ConfigureAwait(false),
                "globalization" => await _globalization.RunAsync(rest).ConfigureAwait(false),
                "hosting" => await _hosting.RunAsync(rest).ConfigureAwait(false),
                "reflection" => await _reflection.RunAsync(rest).ConfigureAwait(false),
                _ => throw new ArgumentException("unknown family " + parts[0]),
            };
            return Response.Text(answer);
        }
        catch (Exception error)
        {
            // The stack is the diagnosis of a failure no family classified.
            return Response.Text("unexpected " + error.GetType().Name + ": " + error.Message + "\n" + error.StackTrace);
        }
    }
}
