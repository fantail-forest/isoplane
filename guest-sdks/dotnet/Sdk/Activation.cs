using System;
using System.Threading.Tasks;
using Isoplane.Runtime;

namespace Isoplane;

/// <summary>
/// Lets an application set up state that must differ between instances, as each instance starts.
/// </summary>
/// <remarks>
/// <para>
/// Isoplane initializes the application ahead of requests and saves its memory as a template. It then
/// runs as many copies of the application as its traffic needs; each copy is an instance, and each
/// starts from the template. Whatever existed when the template was saved is therefore identical in
/// every instance, including a random value drawn at start-up. Subscribe to <see cref="Activated"/>
/// to replace such state when an instance starts.
/// </para>
/// <para>
/// Isoplane already reseeds, before the event is raised, every <c>Random</c> created without a seed,
/// <c>Random.Shared</c>, the generator behind <c>Activity</c> trace and span identifiers, and the one
/// behind SQLite's <c>random()</c> and <c>randomblob()</c>.
/// Cryptographic randomness needs nothing: each call draws fresh bytes. Three things are left to you:
/// a <c>Random</c> constructed with a seed, even when that seed was itself random; a generator that
/// is not a <c>System.Random</c>, such as one a library implements; and a value already drawn, such
/// as an identifier stored in a static field.
/// </para>
/// <para>
/// An application written without ASP.NET has no start-up code to subscribe from, and marks one
/// method with <see cref="ActivatedAttribute"/> instead. For state that every instance can share, use
/// <see cref="Initialization"/>.
/// <see href="https://isoplane.dev/docs/application-lifecycle">The application lifecycle</see>
/// explains templates and instances in full.
/// </para>
/// </remarks>
/// <example>
/// In <c>Program.cs</c>, before <c>await app.RunAsync()</c>:
/// <code>
/// var instance = Guid.Empty;
///
/// Activation.Activated += () =>
/// {
///     instance = Guid.NewGuid();
///     return Task.CompletedTask;
/// };
/// </code>
/// </example>
public static class Activation
{
    private static readonly LifecycleHook Hook = new();

    /// <summary>Raised once in each instance, before the instance handles its first request.</summary>
    /// <remarks>
    /// Subscribe while the application starts: in <c>Program.cs</c> before <c>await app.RunAsync()</c>,
    /// or in an <see cref="Initialization.Initializing"/> callback. Subscriptions are saved in the
    /// template, so every instance runs them. Callbacks are awaited one at a time, in the order they
    /// subscribed. An exception from a callback fails the request the instance was started for. The
    /// event is not raised again in an instance Isoplane suspends and later resumes, because such an
    /// instance keeps its own memory.
    /// </remarks>
    public static event Func<Task>? Activated;

    /// <summary>
    /// Whether this instance has finished starting: true once every <see cref="Activated"/> callback has
    /// completed.
    /// </summary>
    /// <remarks>
    /// It is false while the application starts and its template is prepared. Code that runs in both
    /// phases, such as a library's lazy initialization, can read it to tell whether the state it is
    /// about to create will be shared by every instance or belongs to this one.
    /// </remarks>
    public static bool IsActivated => Hook.IsCompleted;

    internal static Task EnterAsync() => Hook.EnterAsync(ActivateAsync);

    private static Task ActivateAsync()
    {
        RuntimeGenerators.Reseed();
        RuntimeTimers.CountFromActivation();
        return LifecycleHook.InvokeCallbacksAsync(Activated);
    }
}
