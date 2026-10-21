using System;

namespace Isoplane;

/// <summary>
/// Marks the method that handles every HTTP request in an application written without ASP.NET.
/// </summary>
/// <remarks>
/// <para>
/// Use it when one method is all the application needs. The project uses the <c>Isoplane.Sdk</c>
/// project SDK and has no <c>Program.cs</c>; <c>dotnet new isoplane-handler</c> creates one, with
/// tests. An application that wants routing, middleware or dependency injection is an ASP.NET
/// application instead, and starts from <c>IsoplaneHosting.CreateBuilder</c>.
/// <see href="https://isoplane.dev/docs/handle-requests-without-aspnet">Handle requests without ASP.NET</see>
/// walks through a complete project.
/// </para>
/// <para>
/// The method is static, returns <c>Task&lt;Response&gt;</c> and takes a <see cref="Request"/>. It can
/// have any name. It may take a <see cref="StreamingRequest"/> in place of the <see cref="Request"/>,
/// to read a large body as it arrives. An application has exactly one such method. The build reports
/// an error on a method that breaks these rules, on a second marked method, and on a project that
/// has none.
/// </para>
/// <para>
/// Isoplane scales instances with traffic, up to the application’s plan limit, and may discard an
/// instance between requests. Await work the response depends on before returning. Remaining work
/// transfers to the instance’s bounded background allowance and may never finish. Keep durable state
/// in a database or another external service.
/// </para>
/// </remarks>
/// <example>
/// <code>
/// using Isoplane;
///
/// public static class Handler
/// {
///     [HttpHandler]
///     public static Task&lt;Response&gt; HandleAsync(Request request) =>
///         Task.FromResult(request.Path == "/"
///             ? Response.Text("Hello from Isoplane!")
///             : Response.Text("Not found", 404));
/// }
/// </code>
/// </example>
/// <seealso cref="InitializerAttribute"/>
/// <seealso cref="ActivatedAttribute"/>
[AttributeUsage(AttributeTargets.Method)]
public sealed class HttpHandlerAttribute : Attribute;

/// <summary>
/// Marks the method that prepares state every instance shares, in an application written without ASP.NET.
/// </summary>
/// <remarks>
/// <para>
/// Isoplane runs this method during template preparation, and saves the application's
/// memory as a template. Every instance then starts as a copy of that template, so what the method
/// left in static fields is already there when a request arrives. Use it for work each instance would
/// otherwise repeat: reading settings, loading reference data, and running the code a request will
/// run so that no request pays for its first use.
/// </para>
/// <para>
/// The method is static, returns <c>Task</c> and takes no parameters. It can have any name, and an
/// application has at most one. Await all I/O, and dispose connections and streams before returning:
/// an open connection cannot be saved in the template. Timers may remain; their remaining delay
/// counts from activation. An exception stops the application from being prepared, so it does not start.
/// </para>
/// <para>
/// Everything this method creates is identical in every instance. Create what must differ between
/// instances, such as a random identifier, in an <see cref="ActivatedAttribute"/> method.
/// </para>
/// <para>
/// An application with a <c>Program.cs</c> subscribes to <see cref="Initialization.Initializing"/>
/// instead; there the attribute is a build error.
/// <see href="https://isoplane.dev/docs/application-lifecycle">The application lifecycle</see>
/// explains templates and instances in full.
/// </para>
/// </remarks>
/// <example>
/// <code>
/// private static IReadOnlyDictionary&lt;string, string&gt; settings = new Dictionary&lt;string, string&gt;();
///
/// [Initializer]
/// public static Task InitializeAsync()
/// {
///     settings = RuntimeConfiguration.ReadValues();
///     return Task.CompletedTask;
/// }
/// </code>
/// </example>
[AttributeUsage(AttributeTargets.Method)]
public sealed class InitializerAttribute : Attribute;

/// <summary>
/// Marks the method that runs in each instance before its first request, in an application written
/// without ASP.NET.
/// </summary>
/// <remarks>
/// <para>
/// Every instance starts as a copy of the same template, so anything created before the template was
/// saved is identical in all of them. Use this method for state that must differ between instances,
/// such as a random identifier or a random generator you seed yourself.
/// </para>
/// <para>
/// The method is static, returns <c>Task</c> and takes no parameters. It can have any name, and an
/// application has at most one. An exception fails the request the instance was started for.
/// </para>
/// <para>
/// An application with a <c>Program.cs</c> subscribes to <see cref="Activation.Activated"/> instead;
/// there the attribute is a build error. <see cref="Activation"/> lists what Isoplane already reseeds
/// for you, and <see href="https://isoplane.dev/docs/application-lifecycle">The application lifecycle</see>
/// explains templates and instances in full.
/// </para>
/// </remarks>
/// <example>
/// <code>
/// private static Guid instance;
///
/// [Activated]
/// public static Task ActivatedAsync()
/// {
///     instance = Guid.NewGuid();
///     return Task.CompletedTask;
/// }
/// </code>
/// </example>
[AttributeUsage(AttributeTargets.Method)]
public sealed class ActivatedAttribute : Attribute;
