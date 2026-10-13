using System;
using System.Globalization;
using System.IO;
using System.IO.Compression;
using System.Resources;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using Beta.Library;
using Isoplane;
using Microsoft.Extensions.DependencyInjection;

namespace Beta;

/// <summary>The package's report of one request, serialized as its response.</summary>
public sealed record Report(string Path, string Sha256, int Total, string Deflated, string Greeting, int Warmups);

/// <summary>Answers after one host timer with work that touches its whole closure.</summary>
public static class Handler
{
    private static readonly ResourceManager Strings = new("Beta.Strings", typeof(Handler).Assembly);
    private static Accumulator<int> accumulator = null!;
    private static int warmups;

    /// <summary>
    /// Resolves the accumulator through dependency injection and warms the
    /// response path before template capture.
    /// </summary>
    [Initializer]
    public static Task InitializeAsync()
    {
        accumulator = new ServiceCollection()
            .AddSingleton(new Accumulator<int>())
            .BuildServiceProvider()
            .GetRequiredService<Accumulator<int>>();
        warmups++;
        GC.KeepAlive(Answer("/warmup"));
        return Task.CompletedTask;
    }

    /// <summary>Answers the JSON <see cref="Report"/> for the request path.</summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request req)
    {
        await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
        return Answer(req.Path);
    }

    private static Response Answer(string requestPath)
    {
        byte[] path = Encoding.UTF8.GetBytes(requestPath);
        var report = new Report(
            requestPath,
            Convert.ToHexStringLower(SHA256.HashData(path)),
            accumulator.Sum(new Pair<int>(path.Length, 2), new Pair<int>(3, 4)),
            Convert.ToHexStringLower(Deflate(path)),
            Strings.GetString("Greeting", CultureInfo.GetCultureInfo("de")) ?? "",
            warmups);
        return Response.Json(JsonSerializer.Serialize(report));
    }

    private static byte[] Deflate(byte[] bytes)
    {
        using var output = new MemoryStream();
        using (var deflate = new DeflateStream(output, CompressionLevel.SmallestSize, leaveOpen: true))
            deflate.Write(bytes);
        return output.ToArray();
    }
}
