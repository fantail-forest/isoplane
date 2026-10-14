using System.Collections.Generic;
using System.Globalization;
using System.Threading;
using System.Threading.Tasks;
using Isoplane.AspNetCore;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Mvc;
using Microsoft.Extensions.Caching.Distributed;
using Microsoft.Extensions.DependencyInjection;

namespace Session;

/// <summary>
/// Session state, and TempData kept in it, over the cache the <c>Cache</c> setting names:
/// <c>memory</c> for ASP.NET Core's in-memory cache, otherwise <see cref="TimerCache"/>. The
/// <c>Pipeline</c> setting <c>UseSession</c> adds ASP.NET Core's session middleware in place of
/// <c>UseLoadedSession</c>.
/// </summary>
public static class Application
{
    /// <summary>Builds the application and runs it.</summary>
    public static async Task Main(string[] args)
    {
        var builder = IsoplaneHosting.CreateBuilder(args);
        if (builder.Configuration["Cache"] == "memory") builder.Services.AddDistributedMemoryCache();
        else builder.Services.AddSingleton<IDistributedCache, TimerCache>();
        builder.Services.AddSession();
        builder.Services.AddControllersWithViews().AddSessionStateTempDataProvider();
        var app = builder.Build();
        if (builder.Configuration["Pipeline"] == "UseSession") app.UseSession();
        else app.UseLoadedSession();
        app.MapGet("/visits", (HttpContext context) =>
        {
            int visits = (context.Session.GetInt32("visits") ?? 0) + 1;
            context.Session.SetInt32("visits", visits);
            return visits.ToString(CultureInfo.InvariantCulture);
        });
        app.MapControllers();
        await app.RunAsync();
    }
}

/// <summary>Saves a note, then shows the message that saving left in TempData.</summary>
public sealed class NotesController : Controller
{
    /// <summary>Leaves a message in TempData and redirects to <see cref="Show"/>.</summary>
    [HttpPost("/notes")]
    public IActionResult Save()
    {
        TempData["message"] = "note saved";
        return Redirect("/notes");
    }

    /// <summary>The message TempData holds, if any.</summary>
    [HttpGet("/notes")]
    public string Show() => TempData["message"] as string ?? "no message";
}

/// <summary>
/// A distributed cache that answers each call after a host timer, as one that reaches a database
/// answers after a round trip. Its synchronous methods wait for the asynchronous ones, which an
/// application cannot do in the sandbox, so they throw as a database cache's would.
/// </summary>
public sealed class TimerCache : IDistributedCache
{
    private readonly Dictionary<string, byte[]> entries = new();

    /// <inheritdoc />
    public byte[]? Get(string key) => GetAsync(key).GetAwaiter().GetResult();

    /// <inheritdoc />
    public async Task<byte[]?> GetAsync(string key, CancellationToken token = default)
    {
        await Task.Delay(1, token);
        return entries.TryGetValue(key, out byte[]? value) ? value : null;
    }

    /// <inheritdoc />
    public void Set(string key, byte[] value, DistributedCacheEntryOptions options) =>
        SetAsync(key, value, options).GetAwaiter().GetResult();

    /// <inheritdoc />
    public async Task SetAsync(string key, byte[] value, DistributedCacheEntryOptions options, CancellationToken token = default)
    {
        await Task.Delay(1, token);
        entries[key] = value;
    }

    /// <inheritdoc />
    public void Refresh(string key) => RefreshAsync(key).GetAwaiter().GetResult();

    /// <inheritdoc />
    public Task RefreshAsync(string key, CancellationToken token = default) => Task.Delay(1, token);

    /// <inheritdoc />
    public void Remove(string key) => RemoveAsync(key).GetAwaiter().GetResult();

    /// <inheritdoc />
    public async Task RemoveAsync(string key, CancellationToken token = default)
    {
        await Task.Delay(1, token);
        entries.Remove(key);
    }
}
