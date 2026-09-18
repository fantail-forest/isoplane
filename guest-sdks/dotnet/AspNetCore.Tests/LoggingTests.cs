using System.Text.Json;
using Microsoft.Extensions.Logging;
using Xunit;

namespace Isoplane.AspNetCore.Tests;

public sealed class LoggingTests
{
    [Fact]
    public void critical_logs_use_the_host_error_level_and_keep_the_framework_severity()
    {
        var records = new List<(int Level, string Fields)>();
        using var provider = new IsoplaneLoggerProvider((level, fields) => records.Add((level, fields)));
        provider.CreateLogger("CustomerApplication").LogCritical("cannot continue");
        using var payload = JsonDocument.Parse(records.Single().Fields);
        Assert.True(records[0].Level == (int)LogLevel.Error && payload.RootElement.GetProperty("severity").GetString() == "Critical",
            "Critical events must use the supported host error severity without losing the framework's diagnostic level.");
    }

    [Fact]
    public async Task console_writes_emit_during_the_call_without_retaining_post_response_work()
    {
        var records = new List<(int Level, string Fields)>();
        using var writer = new Isoplane.Runtime.GuestConsole((int)LogLevel.Information, (level, fields) => records.Add((level, fields)));
        writer.Write("first");
        var written = writer.WriteLineAsync("second");
        Assert.True(written.IsCompletedSuccessfully && records.Select(record => record.Fields).SequenceEqual(new[] { "first", "second" }),
            "Console writes must reach the logging boundary synchronously rather than schedule background output.");
        await written;
        await writer.FlushAsync();
        Assert.True(records.Count == 2, "Flushing must not publish buffered output after the dispatch ends.");
        using var cancelled = new CancellationTokenSource();
        cancelled.Cancel();
        await Assert.ThrowsAnyAsync<OperationCanceledException>(() => writer.WriteAsync("cancelled".AsMemory(), cancelled.Token));
        Assert.True(records.Count == 2, "A cancelled Console write cannot emit a log record.");
    }

    [Fact]
    public void application_logs_preserve_category_message_exception_and_active_scopes()
    {
        var records = new List<(int Level, string Fields)>();
        using var provider = new IsoplaneLoggerProvider((level, fields) => records.Add((level, fields)));
        var logger = provider.CreateLogger("CustomerApplication");
        using (logger.BeginScope("request scope"))
            logger.LogError(new EventId(17, "SaveFailed"), new InvalidOperationException("database unavailable"), "Could not save {ItemId}", 42);
        logger.LogInformation("next request");
        using var first = JsonDocument.Parse(records[0].Fields);
        var entry = first.RootElement;
        Assert.True(records[0].Level == (int)LogLevel.Error && entry.GetProperty("severity").GetString() == "Error", "The host must receive the framework's severity.");
        Assert.True(entry.GetProperty("category").GetString() == "CustomerApplication" && entry.GetProperty("message").GetString() == "Could not save 42", "Application category and formatted messages must survive the host boundary.");
        Assert.True(entry.GetProperty("eventId").GetInt32() == 17 && entry.GetProperty("properties").GetProperty("ItemId").GetString() == "42", "Event identity and named message properties must remain observable.");
        Assert.True(entry.GetProperty("exception").GetString()!.Contains("database unavailable", StringComparison.Ordinal), "Exception diagnostics must remain available to the developer.");
        Assert.True(entry.GetProperty("scopes")[0].GetString() == "request scope", "The active logging scope must accompany the event.");
        using var next = JsonDocument.Parse(records[1].Fields);
        Assert.True(next.RootElement.GetProperty("scopes").GetArrayLength() == 0, "Disposed scopes must not leak into subsequent request logs.");
    }

    [Fact]
    public void disabled_log_events_do_not_invoke_the_host()
    {
        var calls = 0;
        using var provider = new IsoplaneLoggerProvider((_, _) => calls++);
        var logger = provider.CreateLogger("CustomerApplication");
        logger.Log(LogLevel.None, "disabled");
        Assert.True(calls == 0, "LogLevel.None must not emit a host event.");
    }
}
