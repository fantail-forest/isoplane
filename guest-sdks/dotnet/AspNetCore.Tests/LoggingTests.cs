using System.Text.Json;
using Microsoft.Extensions.Logging;
using Xunit;

namespace Wisp.AspNetCore.Tests;

public sealed class LoggingTests
{
    [Fact]
    public void application_logs_preserve_category_message_exception_and_active_scopes()
    {
        var records = new List<(int Level, string Fields)>();
        using var provider = new WispLoggerProvider((level, fields) => records.Add((level, fields)));
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
        using var provider = new WispLoggerProvider((_, _) => calls++);
        var logger = provider.CreateLogger("CustomerApplication");
        logger.Log(LogLevel.None, "disabled");
        Assert.True(calls == 0, "LogLevel.None must not emit a host event.");
    }
}
