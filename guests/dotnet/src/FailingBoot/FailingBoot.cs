using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Http;
using System.Net.Sockets;
using System.Threading.Tasks;
using Isoplane;

namespace Isoplane.App;

/// <summary>An application whose initialization cannot produce a template, in the way its configuration selects.</summary>
public static class FailingBoot
{
    private static object? retained;

    /// <summary>Leaves the configured kind of connection open, and otherwise refuses to initialize.</summary>
    [TemplateInitializer]
    public static async Task InitializeAsync()
    {
        IReadOnlyDictionary<string, string> settings = RuntimeConfiguration.ReadValues();
        switch (settings.GetValueOrDefault("Failure"))
        {
            case "open-socket":
                retained = new Socket(AddressFamily.InterNetwork, SocketType.Stream, ProtocolType.Tcp);
                break;
            case "pooled-http-connection":
                var client = new HttpClient();
                retained = client;
                (await client.GetAsync(settings["Endpoint"])).Dispose();
                break;
            case "pending-lookup":
                retained = Dns.GetHostAddressesAsync(settings["Name"]);
                break;
            default:
                throw new InvalidOperationException("fixture application initialization failed");
        }
    }

    /// <summary>Cannot be reached when initialization failed.</summary>
    [HttpHandler]
    public static Task<Response> HandleAsync(Request request) =>
        throw new InvalidOperationException("a failed application must never dispatch");
}
