using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.DependencyInjection.Extensions;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Logging.Abstractions;
using Microsoft.Extensions.Logging.Console;
using Microsoft.Extensions.Options;

namespace Isoplane.AspNetCore;

/// <summary>
/// Lets an application keep the console logger that <c>AddConsole</c> and its formatter methods
/// add. <see cref="ConsoleLoggerProvider"/> starts a thread to write its queue, which the sandbox
/// refuses, so it is replaced by <see cref="SynchronousConsoleLoggerProvider"/>, which reads the
/// same options and formatters and writes each entry as it is logged.
/// </summary>
internal static class ConsoleLogging
{
    /// <summary>
    /// Replaces the console logger among the application's finished registrations, and removes
    /// <paramref name="isoplane"/>, whose records would repeat each console entry.
    /// </summary>
    /// <param name="container">
    /// What the host builds its service provider from. Another container, such as one an
    /// application sets with <c>UseServiceProviderFactory</c>, has already copied the
    /// registrations, so it is left as it is.
    /// </param>
    /// <param name="isoplane">The provider the builder registered.</param>
    internal static void UseSynchronousConsole(object container, ILoggerProvider isoplane)
    {
        if (container is not IServiceCollection services) return;
        var replaced = services.Where(descriptor => IsProvider(descriptor)
            && (descriptor.ImplementationType == typeof(ConsoleLoggerProvider) || ReferenceEquals(descriptor.ImplementationInstance, isoplane))).ToList();
        if (!replaced.Any(descriptor => descriptor.ImplementationType == typeof(ConsoleLoggerProvider))) return;
        foreach (var descriptor in replaced) services.Remove(descriptor);
        services.TryAddSingleton(_ => new ConsoleStreams(Console.Out, Console.Error));
        services.AddSingleton<ILoggerProvider>(provider => new SynchronousConsoleLoggerProvider(
            provider.GetRequiredService<IOptionsMonitor<ConsoleLoggerOptions>>(),
            provider.GetServices<ConsoleFormatter>(),
            provider.GetRequiredService<ConsoleStreams>()));
        services.AddSingleton<IPostConfigureOptions<LoggerFilterOptions>, ConsoleFilterRules>();
    }

    // Reading the implementation of a keyed registration throws.
    private static bool IsProvider(ServiceDescriptor descriptor) =>
        descriptor.ServiceType == typeof(ILoggerProvider) && !descriptor.IsKeyedService;

    /// <summary>
    /// Points filter rules that name <see cref="ConsoleLoggerProvider"/> by type, as
    /// <c>AddFilter&lt;ConsoleLoggerProvider&gt;</c> does, at its replacement. Rules under the
    /// <c>Console</c> alias, as in the <c>Logging:Console</c> configuration section, already apply.
    /// </summary>
    private sealed class ConsoleFilterRules : IPostConfigureOptions<LoggerFilterOptions>
    {
        public void PostConfigure(string? name, LoggerFilterOptions options)
        {
            for (var index = 0; index < options.Rules.Count; index++)
            {
                var rule = options.Rules[index];
                if (rule.ProviderName == typeof(ConsoleLoggerProvider).FullName)
                    options.Rules[index] = new LoggerFilterRule(typeof(SynchronousConsoleLoggerProvider).FullName, rule.CategoryName, rule.LogLevel, rule.Filter);
            }
        }
    }
}

/// <summary>The console's standard output and standard error, which tests substitute.</summary>
internal sealed record ConsoleStreams(TextWriter Output, TextWriter Error);

/// <summary>
/// The console logger, writing each entry to the console while the call that logs it runs.
/// It chooses a formatter as <see cref="ConsoleLoggerProvider"/> does, from
/// <see cref="ConsoleLoggerOptions.FormatterName"/>, and writes entries at or above
/// <see cref="ConsoleLoggerOptions.LogToStandardErrorThreshold"/> to standard error. With no queue,
/// the queue options have nothing to govern.
/// </summary>
[ProviderAlias("Console")]
internal sealed class SynchronousConsoleLoggerProvider : ILoggerProvider, ISupportExternalScope
{
    [ThreadStatic]
    private static StringWriter? buffer;

    private readonly Dictionary<string, ConsoleFormatter> formatters = new(StringComparer.OrdinalIgnoreCase);
    private readonly ConsoleStreams console;
    private readonly IDisposable? reload;
    private Output output;
    private IExternalScopeProvider? scopes;

    internal SynchronousConsoleLoggerProvider(IOptionsMonitor<ConsoleLoggerOptions> options, IEnumerable<ConsoleFormatter> formatters, ConsoleStreams console)
    {
        foreach (var formatter in formatters) this.formatters.TryAdd(formatter.Name, formatter);
        this.console = console;
        output = Select(options.CurrentValue);
        reload = options.OnChange(changed =>
        {
            var previous = output;
            output = Select(changed);
            previous.Dispose();
        });
    }

    /// <inheritdoc />
    public ILogger CreateLogger(string categoryName) => new Logger(this, categoryName);

    /// <inheritdoc />
    public void SetScopeProvider(IExternalScopeProvider scopeProvider) => scopes = scopeProvider;

    /// <inheritdoc />
    public void Dispose()
    {
        reload?.Dispose();
        output.Dispose();
    }

    private Output Select(ConsoleLoggerOptions options)
    {
#pragma warning disable CS0618 // ConsoleLoggerProvider still honours the deprecated options, and applications still set them.
        if (options.FormatterName is null) return DeprecatedOptionsOutput(options);
        var fallback = options.Format == ConsoleLoggerFormat.Systemd ? ConsoleFormatterNames.Systemd : ConsoleFormatterNames.Simple;
#pragma warning restore CS0618
        return new(formatters.GetValueOrDefault(options.FormatterName) ?? formatters[fallback], options.LogToStandardErrorThreshold, null);
    }

    /// <summary>
    /// With no formatter named, <see cref="ConsoleLoggerProvider"/> formats entries with the
    /// simple or systemd formatter set from the deprecated options, in place of the formatter's
    /// own. Those formatters' types are not public, so a private container creates them.
    /// </summary>
    private static Output DeprecatedOptionsOutput(ConsoleLoggerOptions options)
    {
#pragma warning disable CS0618 // These are the deprecated options being honoured.
        var systemd = options.Format == ConsoleLoggerFormat.Systemd;
        var (colors, scopes, timestamp, utc) = (!options.DisableColors, options.IncludeScopes, options.TimestampFormat, options.UseUtcTimestamp);
#pragma warning restore CS0618
        var services = new ServiceCollection().AddLogging(logging =>
        {
            if (systemd)
                logging.AddSystemdConsole(formatter => (formatter.IncludeScopes, formatter.TimestampFormat, formatter.UseUtcTimestamp) = (scopes, timestamp, utc));
            else
                logging.AddSimpleConsole(formatter =>
                {
                    formatter.ColorBehavior = colors ? LoggerColorBehavior.Default : LoggerColorBehavior.Disabled;
                    (formatter.IncludeScopes, formatter.TimestampFormat, formatter.UseUtcTimestamp) = (scopes, timestamp, utc);
                });
        }).BuildServiceProvider();
        var name = systemd ? ConsoleFormatterNames.Systemd : ConsoleFormatterNames.Simple;
        return new(services.GetServices<ConsoleFormatter>().Single(formatter => formatter.Name == name), options.LogToStandardErrorThreshold, services);
    }

    private void Write<TState>(in LogEntry<TState> entry)
    {
        var current = output;
        var text = buffer ??= new StringWriter();
        current.Formatter.Write(in entry, scopes, text);
        var builder = text.GetStringBuilder();
        if (builder.Length == 0) return;
        var formatted = builder.ToString();
        builder.Clear();
        if (builder.Capacity > 1024) builder.Capacity = 1024;
        var stream = entry.LogLevel >= current.ErrorThreshold ? console.Error : console.Output;
        // Each write to the console is one record in the application's log, which would show the
        // line ending a formatter closes an entry with as an empty line.
        if (formatted.EndsWith(Environment.NewLine, StringComparison.Ordinal))
            stream.WriteLine(formatted.AsSpan(0, formatted.Length - Environment.NewLine.Length));
        else
            stream.Write(formatted);
    }

    private sealed record Output(ConsoleFormatter Formatter, LogLevel ErrorThreshold, IDisposable? Owner) : IDisposable
    {
        public void Dispose() => Owner?.Dispose();
    }

    private sealed class Logger(SynchronousConsoleLoggerProvider provider, string category) : ILogger
    {
        public IDisposable? BeginScope<TState>(TState state) where TState : notnull => provider.scopes?.Push(state);

        public bool IsEnabled(LogLevel logLevel) => logLevel != LogLevel.None;

        public void Log<TState>(LogLevel logLevel, EventId eventId, TState state, Exception? exception, Func<TState, Exception?, string> formatter)
        {
            if (!IsEnabled(logLevel)) return;
            ArgumentNullException.ThrowIfNull(formatter);
            provider.Write(new LogEntry<TState>(logLevel, category, eventId, state, exception, formatter));
        }
    }
}
