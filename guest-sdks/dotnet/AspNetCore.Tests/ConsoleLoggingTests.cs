using System.Text.Json;
using Microsoft.AspNetCore.Builder;
using Microsoft.Extensions.Configuration.Memory;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Logging.Console;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class ConsoleLoggingTests
{
    private sealed record Console(WebApplication Application, StringWriter Output, StringWriter Error) : IAsyncDisposable
    {
        internal string Written => Output.ToString().ReplaceLineEndings("\n");

        internal IEnumerable<Type> Providers => Application.Services.GetServices<ILoggerProvider>().Select(provider => provider.GetType());

        /// <summary>Logs a warning with a named value inside a scope with one of its own.</summary>
        internal void LogLateOrder(string category = "Orders")
        {
            var logger = Application.Services.GetRequiredService<ILoggerFactory>().CreateLogger(category);
            using (logger.BeginScope("order {Id}", 42))
                logger.LogWarning(new EventId(3, "Late"), "late by {Minutes}", 5);
        }

        public ValueTask DisposeAsync() => Application.DisposeAsync();
    }

    private static Console Build(Action<WebApplicationBuilder> configure, Dictionary<string, string?>? settings = null)
    {
        var builder = IsoplaneHosting.CreateBuilder(typeof(ConsoleLoggingTests).Assembly.GetName().Name!,
            new MemoryConfigurationSource { InitialData = settings ?? [] });
        var console = new ConsoleStreams(new StringWriter(), new StringWriter());
        builder.Services.AddSingleton(console);
        configure(builder);
        return new(builder.Build(), (StringWriter)console.Output, (StringWriter)console.Error);
    }

#pragma warning disable CS0618 // Applications still set the deprecated console options, which take effect while no formatter is named.
    public static TheoryData<string, Action<ILoggingBuilder>, string> Formatters => new()
    {
        { "AddConsole", logging => logging.AddConsole(), "warn: Orders[3]\n      late by 5\n" },
        { "ClearProviders, AddConsole", logging => { logging.ClearProviders(); logging.AddConsole(); }, "warn: Orders[3]\n      late by 5\n" },
        {
            "AddSimpleConsole",
            logging => logging.AddSimpleConsole(options =>
            {
                options.SingleLine = true;
                options.IncludeScopes = true;
                options.TimestampFormat = "'at' ";
                options.ColorBehavior = LoggerColorBehavior.Disabled;
            }),
            "at warn: Orders[3] => order 42 late by 5\n"
        },
        { "AddSystemdConsole", logging => logging.AddSystemdConsole(options => options.IncludeScopes = true), "<4>Orders[3] => order 42 late by 5\n" },
        {
            "AddConsole with deprecated options",
            logging => logging.AddConsole(options => { options.IncludeScopes = true; options.TimestampFormat = "'at' "; }),
            "at warn: Orders[3]\n      => order 42\n      late by 5\n"
        },
        { "AddConsole with the deprecated systemd format", logging => logging.AddConsole(options => options.Format = ConsoleLoggerFormat.Systemd), "<4>Orders[3] late by 5\n" },
    };
#pragma warning restore CS0618

    [Theory]
    [MemberData(nameof(Formatters))]
    public async Task the_console_logger_writes_each_entry_as_its_formatter_formats_it_while_the_call_runs(string calls, Action<ILoggingBuilder> configure, string expected)
    {
        await using var console = Build(builder => configure(builder.Logging));
        console.LogLateOrder();
        Assert.Equal(expected, console.Written);
        Assert.Equal("", console.Error.ToString());
        Assert.True(console.Providers.SequenceEqual([typeof(SynchronousConsoleLoggerProvider)]),
            $"after {calls}, only the console logger may write, or each entry is logged twice: {string.Join(", ", console.Providers)}");
    }

    [Fact]
    public async Task the_json_formatter_writes_its_writer_options_and_scopes()
    {
        await using var console = Build(builder => builder.Logging.AddJsonConsole(options =>
        {
            options.IncludeScopes = true;
            options.JsonWriterOptions = new JsonWriterOptions { Indented = true };
        }));
        console.LogLateOrder();
        var written = console.Written;
        using var entry = JsonDocument.Parse(written);
        var root = entry.RootElement;
        Assert.Equal(("Orders", "Warning", 3, "late by 5"),
            (root.GetProperty("Category").GetString(), root.GetProperty("LogLevel").GetString(), root.GetProperty("EventId").GetInt32(), root.GetProperty("Message").GetString()));
        Assert.Equal(5, root.GetProperty("State").GetProperty("Minutes").GetInt32());
        Assert.Equal(42, root.GetProperty("Scopes")[0].GetProperty("Id").GetInt32());
        Assert.StartsWith("{\n  \"EventId\": 3,", written);
    }

    [Fact]
    public async Task entries_at_the_standard_error_threshold_go_to_standard_error()
    {
        await using var console = Build(builder => builder.Logging.AddConsole(options => options.LogToStandardErrorThreshold = LogLevel.Warning));
        var logger = console.Application.Services.GetRequiredService<ILoggerFactory>().CreateLogger("Orders");
        logger.LogInformation("accepted");
        logger.LogWarning("late");
        Assert.Equal(("info: Orders[0]\n      accepted\n", "warn: Orders[0]\n      late\n"),
            (console.Written, console.Error.ToString().ReplaceLineEndings("\n")));
    }

    [Fact]
    public async Task console_filters_and_settings_apply_to_the_console_logger()
    {
        await using var console = Build(builder =>
        {
            builder.Logging.AddConsole();
            builder.Logging.AddFilter<ConsoleLoggerProvider>("Noisy", LogLevel.Error);
        }, new()
        {
            ["Logging:Console:LogLevel:Quiet"] = "None",
            ["Logging:Console:FormatterName"] = "systemd"
        });
        console.LogLateOrder("Noisy");
        console.LogLateOrder("Quiet");
        console.LogLateOrder("Orders");
        Assert.Equal("<4>Orders[3] late by 5\n", console.Written);
    }

    [Fact]
    public async Task an_application_that_configures_the_default_service_provider_keeps_the_synchronous_console_logger()
    {
        await using var console = Build(builder =>
        {
            builder.Host.UseDefaultServiceProvider(options => options.ValidateScopes = true);
            builder.Logging.AddConsole();
        });
        Assert.Equal([typeof(SynchronousConsoleLoggerProvider)], console.Providers);
    }

    [Fact]
    public async Task clearing_the_providers_leaves_the_application_without_logging()
    {
        await using var console = Build(builder => builder.Logging.ClearProviders());
        Assert.Empty(console.Providers);
    }
}
