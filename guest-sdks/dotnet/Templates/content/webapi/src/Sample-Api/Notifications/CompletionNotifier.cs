using Microsoft.Extensions.Options;
using Sample_Api.Todos;

namespace Sample_Api.Notifications;

/// <summary>
/// The "Notifications" section of configuration. Give a feature its own options
/// class and section, and its settings stay discoverable in appsettings.json.
/// </summary>
public sealed class NotificationOptions
{
    /// <summary>Where completed todos are posted. Notifications are off without it.</summary>
    public Uri? WebhookUrl { get; set; }
}

/// <summary>What the webhook receives for a completed todo.</summary>
public sealed record CompletionNotification(int Id, string Title, DateTimeOffset CompletedAt);

/// <summary>
/// Posts completed todos to the configured webhook. Outbound HTTP works as in
/// any ASP.NET Core application: a typed client from <c>AddHttpClient</c>, awaited
/// by the request that needs it.
/// </summary>
public sealed partial class CompletionNotifier(HttpClient client, IOptions<NotificationOptions> options, ILogger<CompletionNotifier> logger)
{
    public async Task NotifyAsync(Todo todo, CancellationToken cancellation)
    {
        if (options.Value.WebhookUrl is not { } webhook || todo.CompletedAt is not { } completedAt) return;
        using var response = await client.PostAsJsonAsync(webhook, new CompletionNotification(todo.Id, todo.Title, completedAt), cancellation);
        if (response.IsSuccessStatusCode) LogDelivered(todo.Id);
        else LogRefused(todo.Id, (int)response.StatusCode);
    }

    [LoggerMessage(Level = LogLevel.Information, Message = "Delivered the completion of todo {TodoId}")]
    private partial void LogDelivered(int todoId);

    [LoggerMessage(Level = LogLevel.Warning, Message = "The webhook refused the completion of todo {TodoId} with status {StatusCode}")]
    private partial void LogRefused(int todoId, int statusCode);
}

public static class NotificationServices
{
    public static IServiceCollection AddCompletionNotifications(this IServiceCollection services, IConfiguration configuration)
    {
        services.Configure<NotificationOptions>(configuration.GetSection("Notifications"));
        services.AddHttpClient<CompletionNotifier>(client => client.Timeout = TimeSpan.FromSeconds(10));
        return services;
    }
}
