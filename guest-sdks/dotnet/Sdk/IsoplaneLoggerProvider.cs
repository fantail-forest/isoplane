using System;
using System.Collections.Generic;
using System.IO;
using System.Text;
using System.Text.Json;
using Microsoft.Extensions.Logging;

namespace Isoplane;

/// <summary>
/// Sends the messages an application writes with .NET's <c>ILogger</c> to the application's log.
/// </summary>
/// <remarks>
/// <para>
/// An ASP.NET application does not create one. <c>IsoplaneHosting.CreateBuilder</c> registers it, so
/// inject <c>ILogger&lt;T&gt;</c> or use <c>app.Logger</c> as in any ASP.NET application, and set log
/// levels in the <c>Logging</c> configuration section.
/// </para>
/// <para>
/// In an application written without ASP.NET, <c>Console.WriteLine</c> already writes to the same
/// log. Create a provider there only when you want what <c>ILogger</c> adds: levels, categories and
/// named values. A logger from a provider you create writes every level except <c>None</c>.
/// Use it there in place of the console logger from <c>Microsoft.Extensions.Logging.Console</c>,
/// which starts a thread when it is created and so fails outside an ASP.NET application.
/// </para>
/// <para>
/// While developing, the log appears in the terminal running the application. A message logged at
/// <c>Critical</c> is recorded at the log's error level. Do not log credentials or connection
/// strings.
/// </para>
/// </remarks>
/// <example>
/// In an application written without ASP.NET:
/// <code>
/// using Isoplane;
/// using Microsoft.Extensions.Logging;
///
/// public static class Handler
/// {
///     private static readonly ILogger Log = new IsoplaneLoggerProvider().CreateLogger("Orders");
///
///     [HttpHandler]
///     public static Task&lt;Response&gt; HandleAsync(Request request)
///     {
///         Log.LogInformation("Handling {Path}", request.Path);
///         return Task.FromResult(Response.Text("ok"));
///     }
/// }
/// </code>
/// </example>
public sealed class IsoplaneLoggerProvider : ILoggerProvider, ISupportExternalScope
{
    private readonly Action<int, string> emit;
    private IExternalScopeProvider scopes = new LoggerExternalScopeProvider();

    /// <summary>Creates a provider. Its loggers work only while the application runs under Isoplane.</summary>
    public IsoplaneLoggerProvider() : this(Abi.GuestLog.Emit) { }

    internal IsoplaneLoggerProvider(Action<int, string> emit) => this.emit = emit;

    /// <inheritdoc />
    public ILogger CreateLogger(string categoryName) => new Logger(this, categoryName);

    /// <inheritdoc />
    public void SetScopeProvider(IExternalScopeProvider scopeProvider) => scopes = scopeProvider;

    /// <inheritdoc />
    public void Dispose() { }

    private sealed class Logger(IsoplaneLoggerProvider provider, string category) : ILogger
    {
        public IDisposable? BeginScope<TState>(TState state) where TState : notnull => provider.scopes.Push(state);

        public bool IsEnabled(LogLevel logLevel) => logLevel != LogLevel.None;

        public void Log<TState>(LogLevel logLevel, EventId eventId, TState state, Exception? exception, Func<TState, Exception?, string> formatter)
        {
            if (!IsEnabled(logLevel)) return;
            using var buffer = new MemoryStream();
            using (var writer = new Utf8JsonWriter(buffer))
            {
                writer.WriteStartObject();
                writer.WriteString("category", category);
                writer.WriteString("severity", logLevel.ToString());
                writer.WriteNumber("eventId", eventId.Id);
                if (eventId.Name is not null) writer.WriteString("eventName", eventId.Name);
                writer.WriteString("message", formatter(state, exception));
                if (exception is not null) writer.WriteString("exception", exception.ToString());
                writer.WriteStartObject("properties");
                if (state is IEnumerable<KeyValuePair<string, object?>> values)
                    foreach (var value in values) writer.WriteString(value.Key, value.Value?.ToString());
                writer.WriteEndObject();
                writer.WriteStartArray("scopes");
                provider.scopes.ForEachScope((scope, output) => output.WriteStringValue(scope?.ToString()), writer);
                writer.WriteEndArray();
                writer.WriteEndObject();
            }
            var hostLevel = logLevel == LogLevel.Critical ? LogLevel.Error : logLevel;
            provider.emit((int)hostLevel, Encoding.UTF8.GetString(buffer.GetBuffer(), 0, checked((int)buffer.Length)));
        }
    }
}
