using System;

namespace Isoplane;

/// <summary>
/// Marks the static method that handles every HTTP request to an application that has no entry point.
/// </summary>
/// <remarks>
/// Declare it as <c>Task&lt;Response&gt; HandleAsync(Request request)</c>, optionally with a second
/// <see cref="Ctx"/> parameter. Take a <see cref="StreamingRequest"/> in place of the
/// <see cref="Request"/> to read the body as it arrives. An application has exactly one; the build
/// fails otherwise. Instances may be reused or evicted between requests, so do not rely on state
/// retained in memory or on work continuing after the response.
/// </remarks>
[AttributeUsage(AttributeTargets.Method)]
public sealed class HttpHandlerAttribute : Attribute;

/// <summary>
/// Marks the static method that prepares reusable state before the application's template is captured.
/// </summary>
/// <remarks>
/// Declare it as <c>Task InitializeAsync()</c>. It runs as an <see cref="Initialization.Initializing"/>
/// callback and follows the same rules: what it leaves in memory is copied into every instance.
/// An application has at most one.
/// </remarks>
[AttributeUsage(AttributeTargets.Method)]
public sealed class InitializerAttribute : Attribute;

/// <summary>
/// Marks the static method that runs once in each instance, before its first request.
/// </summary>
/// <remarks>
/// Declare it as <c>Task ActivatedAsync()</c>. It runs as an <see cref="Activation.Activated"/>
/// callback: use it for state that must differ between instances, such as reseeding a random
/// generator. An application has at most one.
/// </remarks>
[AttributeUsage(AttributeTargets.Method)]
public sealed class ActivatedAttribute : Attribute;
