using System.Diagnostics.CodeAnalysis;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Session;
using Microsoft.Extensions.Caching.Distributed;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.DependencyInjection.Extensions;
using Microsoft.Extensions.Logging;

namespace Isoplane.AspNetCore;

/// <summary>
/// Session state over a distributed cache, loaded before the application uses it.
/// </summary>
/// <remarks>
/// <para>
/// ASP.NET Core's session loads itself synchronously the first time a request reads or writes it.
/// A distributed cache that reaches a database or another service cannot be read synchronously on
/// Isoplane, so that load fails. ASP.NET Core would then carry on with an empty session and drop
/// what the request writes to it; on Isoplane, using a session that could not be loaded throws an
/// <see cref="InvalidOperationException"/> that names <see cref="UseLoadedSession"/>.
/// </para>
/// <para>
/// A session over the in-memory cache, <c>AddDistributedMemoryCache</c>, loads synchronously
/// without trouble, and works with <c>UseSession</c> as well.
/// </para>
/// </remarks>
public static class SessionLoading
{
    /// <summary>
    /// Adds ASP.NET Core's session middleware, as <c>UseSession</c> does, and loads each request's
    /// session from the cache asynchronously before the rest of the pipeline runs, so that the
    /// session and TempData kept in it work over a distributed cache. Call it in place of
    /// <c>app.UseSession()</c>.
    /// </summary>
    /// <param name="app">The application's pipeline.</param>
    /// <returns><paramref name="app"/>.</returns>
    /// <remarks>
    /// <para>
    /// Register the session and its cache as usual, with <c>AddSession</c> and an
    /// <c>IDistributedCache</c>; the options given to <c>AddSession</c> apply.
    /// </para>
    /// <para>
    /// Every request that passes through the middleware reads the cache once, whether or not it
    /// uses the session. Place it after middleware that answers requests without a session, such
    /// as <c>UseStaticFiles</c>, so those requests make no read. A failed read fails the request
    /// with the cache's exception.
    /// </para>
    /// </remarks>
    /// <example>
    /// <code>
    /// builder.Services.AddStackExchangeRedisCache(options =>
    ///     options.Configuration = builder.Configuration.GetConnectionString("Redis"));
    /// builder.Services.AddSession();
    /// var app = builder.Build();
    ///
    /// app.UseStaticFiles();
    /// app.UseLoadedSession();
    /// </code>
    /// </example>
    public static IApplicationBuilder UseLoadedSession(this IApplicationBuilder app)
    {
        ArgumentNullException.ThrowIfNull(app);
        app.UseSession();
        return app.Use(async (context, next) =>
        {
            await context.Session.LoadAsync(context.RequestAborted);
            await next(context);
        });
    }

    /// <summary>
    /// Makes the sessions <c>AddSession</c> creates throw when used without having loaded.
    /// <c>AddSession</c> registers its store only if none is registered, so this one stays.
    /// </summary>
    internal static void CheckSessionLoading(this IServiceCollection services) =>
        services.TryAddTransient<ISessionStore, LoadCheckingSessionStore>();
}

/// <summary>ASP.NET Core's distributed session store, with each session it creates checked by <see cref="LoadCheckingSession"/>.</summary>
internal sealed class LoadCheckingSessionStore(IDistributedCache cache, ILoggerFactory loggerFactory) : ISessionStore
{
    private readonly DistributedSessionStore store = new(cache, loggerFactory);

    public ISession Create(string sessionKey, TimeSpan idleTimeout, TimeSpan ioTimeout, Func<bool> tryEstablishSession, bool isNewSessionKey) =>
        new LoadCheckingSession(store.Create(sessionKey, idleTimeout, ioTimeout, tryEstablishSession, isNewSessionKey));
}

/// <summary>A session that throws when it is used and could not be loaded, where ASP.NET Core's would lose its data.</summary>
internal sealed class LoadCheckingSession(ISession session) : ISession
{
    private bool loaded;

    public bool IsAvailable => session.IsAvailable;

    public string Id => Loaded().Id;

    public IEnumerable<string> Keys => Loaded().Keys;

    public async Task LoadAsync(CancellationToken cancellationToken = default)
    {
        await session.LoadAsync(cancellationToken);
        loaded = true;
    }

    public Task CommitAsync(CancellationToken cancellationToken = default) => session.CommitAsync(cancellationToken);

    public bool TryGetValue(string key, [NotNullWhen(true)] out byte[]? value) => Loaded().TryGetValue(key, out value);

    public void Set(string key, byte[] value) => Loaded().Set(key, value);

    public void Remove(string key) => Loaded().Remove(key);

    public void Clear() => Loaded().Clear();

    private ISession Loaded()
    {
        if (loaded) return session;
        // Reading IsAvailable loads the session synchronously. If that fails, ASP.NET Core logs the
        // cache's exception and from then on serves an empty session that saves nothing.
        if (!session.IsAvailable)
            throw new InvalidOperationException(
                "The session could not be loaded from its cache. ASP.NET Core loads a session synchronously when it is first used, " +
                "and a distributed cache cannot be read synchronously on Isoplane. Call app.UseLoadedSession() from Isoplane.AspNetCore " +
                "in place of app.UseSession(), or await HttpContext.Session.LoadAsync() before using the session. " +
                "The cache's own exception is logged under Microsoft.AspNetCore.Session.DistributedSession.");
        loaded = true;
        return session;
    }
}
