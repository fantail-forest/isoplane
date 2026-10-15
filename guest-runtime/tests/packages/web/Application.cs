using System;
using System.Globalization;
using System.IO;
using System.Net.Http;
using System.Text.Json;
using System.Threading.Tasks;
using Isoplane;
using Isoplane.AspNetCore;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.RateLimiting;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Logging.Console;

namespace Web;

/// <summary>
/// Minimal API routes over a singleton, a content file, a static web asset, a logged entry and, when a <c>Peer</c> setting names one, a client from <c>IHttpClientFactory</c>.
/// A <c>ConsoleLogging</c> setting names the logging calls its builder makes.
/// </summary>
public static class Application
{
    /// <summary>Builds the application, subscribes its warm-up and runs it.</summary>
    public static async Task Main(string[] args)
    {
        var builder = IsoplaneHosting.CreateBuilder(args);
        builder.Services.AddSingleton<Greeter>();
        builder.Services.AddRateLimiter(options =>
        {
            options.AddFixedWindowLimiter("limited", limiter =>
            {
                limiter.PermitLimit = 1;
                limiter.Window = TimeSpan.FromMinutes(1);
            });
            options.OnRejected = (rejected, _) =>
            {
                rejected.HttpContext.Response.StatusCode = StatusCodes.Status429TooManyRequests;
                return new ValueTask(rejected.HttpContext.Response.WriteAsync("rejected"));
            };
        });
        string? peer = builder.Configuration["Peer"];
        if (peer is not null) builder.Services.AddHttpClient("peer", client => client.BaseAddress = new Uri(peer));
        AddConsoleLogging(builder, builder.Configuration["ConsoleLogging"]);
        var app = builder.Build();
        // The middleware starts a timer that ticks every 100 ms, which initialization leaves running.
        app.UseRateLimiter();
        app.MapGet("/limited", async () =>
        {
            await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
            return "allowed";
        }).RequireRateLimiting("limited");
        app.MapGet("/hello/{name}", async (string name, Greeter greeter) =>
        {
            await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
            return greeter.Greet(name);
        });
        app.MapGet("/warm", (Greeter greeter) => greeter.Warmups++);
        // A stylesheet is answered after one host timer, like every route the package tests finish across a hibernation.
        app.Use(async (context, next) =>
        {
            if (context.Request.Path.Value!.EndsWith(".css", StringComparison.Ordinal))
                await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
            await next(context);
        });
        app.MapStaticAssets();
        app.MapGet("/log", async (ILoggerFactory loggers) =>
        {
            await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
            var logger = loggers.CreateLogger("Web.Orders");
            using (logger.BeginScope("order {Id}", 42))
                logger.LogWarning(new EventId(3, "Late"), "late by {Minutes}", 5);
            return "logged";
        });
        var calls = 0;
        app.MapGet("/outbound", async (IHttpClientFactory clients) =>
        {
            var client = clients.CreateClient("peer");
            string first = await client.GetStringAsync("/hello");
            string second = await client.GetStringAsync("/hello");
            return first + "|" + second + "; calls=" + (++calls).ToString(CultureInfo.InvariantCulture);
        });
        Initialization.Initializing += async () =>
        {
            await app.WarmupAsync("/warm");
            await app.WarmupAsync("/hello/warmup");
            if (peer is not null) await app.WarmupAsync("/outbound");
        };
        await app.RunAsync();
    }

    /// <summary>Makes the logging calls <paramref name="calls"/> names, as an application's <c>Program.cs</c> would.</summary>
    private static void AddConsoleLogging(WebApplicationBuilder builder, string? calls)
    {
        switch (calls)
        {
            case null:
                break;
            case "AddConsole":
                builder.Logging.AddConsole();
                break;
            case "ClearProviders AddConsole":
                builder.Logging.ClearProviders();
                builder.Logging.AddConsole();
                break;
            case "ClearProviders":
                builder.Logging.ClearProviders();
                break;
            case "AddJsonConsole":
                builder.Logging.AddJsonConsole(options =>
                {
                    options.IncludeScopes = true;
                    options.JsonWriterOptions = new JsonWriterOptions { Indented = true };
                });
                break;
            case "AddSimpleConsole":
                builder.Logging.AddSimpleConsole(options =>
                {
                    options.SingleLine = true;
                    options.IncludeScopes = true;
                    options.TimestampFormat = "'at' ";
                });
                break;
            case "AddSystemdConsole":
                builder.Logging.AddSystemdConsole(options => options.IncludeScopes = true);
                builder.Services.Configure<ConsoleLoggerOptions>(options => options.LogToStandardErrorThreshold = LogLevel.Warning);
                break;
            default:
                throw new ArgumentException($"No logging calls are named {calls}.", nameof(calls));
        }
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
