using System;
using System.Collections.Generic;
using System.IO;
using System.Text;
using System.Text.Json;
using Microsoft.Extensions.Logging;

namespace Wisp;

/// <summary>Writes .NET application logs through the sandbox's bounded host logging capability.</summary>
public sealed class WispLoggerProvider : ILoggerProvider, ISupportExternalScope
{
    private readonly Action<int, string> emit;
    private IExternalScopeProvider scopes = new LoggerExternalScopeProvider();

    /// <summary>Creates a provider for application code executing inside the WISP sandbox.</summary>
    public WispLoggerProvider() : this(Abi.GuestLog.Emit) { }

    internal WispLoggerProvider(Action<int, string> emit) => this.emit = emit;

    /// <inheritdoc />
    public ILogger CreateLogger(string categoryName) => new Logger(this, categoryName);

    /// <inheritdoc />
    public void SetScopeProvider(IExternalScopeProvider scopeProvider) => scopes = scopeProvider;

    /// <inheritdoc />
    public void Dispose() { }

    private sealed class Logger(WispLoggerProvider provider, string category) : ILogger
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
            provider.emit((int)logLevel, Encoding.UTF8.GetString(buffer.GetBuffer(), 0, checked((int)buffer.Length)));
        }
    }
}
