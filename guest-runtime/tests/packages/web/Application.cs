using System;
using System.Globalization;
using System.IO;
using System.Threading.Tasks;
using Isoplane;
using Isoplane.AspNetCore;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.DependencyInjection;

namespace Web;

/// <summary>Minimal API routes over a singleton, a content file and a streamed upload.</summary>
public static class Application
{
    /// <summary>Builds the application, subscribes its warm-up and runs it.</summary>
    public static async Task Main(string[] args)
    {
        var builder = IsoplaneHosting.CreateBuilder(args);
        builder.Services.AddSingleton<Greeter>();
        var app = builder.Build();
        app.MapGet("/hello/{name}", async (string name, Greeter greeter) =>
        {
            await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
            return greeter.Greet(name);
        });
        app.MapGet("/warm", (Greeter greeter) => greeter.Warmups++);
        app.MapPost("/upload", async (HttpRequest request) =>
        {
            long total = 0;
            byte[] buffer = new byte[4096];
            int read;
            while ((read = await request.Body.ReadAsync(buffer)) != 0)
                total += read;
            return "received:" + total.ToString(CultureInfo.InvariantCulture);
        });
        Initialization.Initializing += async () =>
        {
            await app.WarmupAsync("/warm");
            await app.WarmupAsync("/hello/warmup");
        };
        await app.RunAsync();
    }
}

/// <summary>Greets with the content file the package serves at its content root.</summary>
public sealed class Greeter
{
    private readonly string greeting = File.ReadAllText("/greeting.txt");

    /// <summary>Warm-up requests served, all during initialization.</summary>
    public int Warmups { get; set; }

    private int greetings;

    /// <summary>The content file's greeting and counts including initialization requests.</summary>
    public string Greet(string name)
    {
        greetings++;
        return greeting + ", " + name + "; greetings=" + greetings.ToString(CultureInfo.InvariantCulture)
            + "; warmups=" + Warmups.ToString(CultureInfo.InvariantCulture);
    }
}
