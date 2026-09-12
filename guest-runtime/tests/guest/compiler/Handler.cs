using System;
using System.Net;
using System.Threading.Tasks;
using Wisp;
using Wisp.Durable;

namespace Acceptance;

/// <summary>Exercises native compiler ABI lowering without optimization.</summary>
public sealed class AcceptanceHandler : IWispHandler
{
    /// <summary>Checks a native integer boundary or socket-address serialization.</summary>
    public Task<Response> HandleAsync(Request request, Ctx context)
    {
        string[] parts = request.Path.Split('/', StringSplitOptions.RemoveEmptyEntries);
        string result;
        if (parts[0] == "serialize")
        {
            result = "ok";
            for (int port = 0; port <= ushort.MaxValue; port++)
            {
                var address = new IPEndPoint(IPAddress.Loopback, port).Serialize();
                if (address[2] != (byte)(port >> 8) || address[3] != (byte)port)
                {
                    result = $"port={port} serialized={address[2]}:{address[3]}";
                    break;
                }
            }
        }
        else result = Wisp.Abi.NativeCompilerProbe.Run(parts[0], parts[1]);
        return Task.FromResult(Response.Text(result));
    }
}
