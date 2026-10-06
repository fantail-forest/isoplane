using System.Net;
using System.Net.Http.Json;
using SampleApi.Notifications;
using SampleApi.Todos;

namespace SampleApi.Tests;

/// <summary>
/// Sandbox tests: HTTP requests to the application running in the real Isoplane
/// sandbox, which is where platform behavior such as initialization, activation
/// and outbound connections can be observed.
/// </summary>
[Collection(SandboxCollection.Name)]
[Trait("Category", "Sandbox")]
public sealed class TodoApiTests(SandboxFixture sandbox)
{
    [Fact]
    public async Task A_created_todo_can_be_read_completed_and_deleted()
    {
        using var created = await sandbox.Client.PostAsJsonAsync("/todos", new CreateTodo("Ship the first version"));
        Assert.True(created.StatusCode == HttpStatusCode.Created, sandbox.Diagnostics);
        var todo = (await created.Content.ReadFromJsonAsync<TodoResponse>())!;
        Assert.Equal(new Uri(sandbox.Client.BaseAddress!, $"/todos/{todo.Id}"), created.Headers.Location);

        using var completed = await sandbox.Client.PutAsJsonAsync($"/todos/{todo.Id}", new UpdateTodo(todo.Title, IsComplete: true));
        var updated = (await completed.Content.ReadFromJsonAsync<TodoResponse>())!;
        Assert.True(updated.IsComplete);
        Assert.NotNull(updated.CompletedAt);

        var complete = await sandbox.Client.GetFromJsonAsync<List<TodoResponse>>("/todos?complete=true");
        Assert.Contains(complete!, item => item.Id == todo.Id);

        using var deleted = await sandbox.Client.DeleteAsync($"/todos/{todo.Id}");
        Assert.Equal(HttpStatusCode.NoContent, deleted.StatusCode);
        using var missing = await sandbox.Client.GetAsync($"/todos/{todo.Id}");
        Assert.Equal(HttpStatusCode.NotFound, missing.StatusCode);
    }

    [Theory]
    [InlineData("")]
    [InlineData(null)]
    public async Task A_todo_without_a_title_is_refused(string? title)
    {
        using var response = await sandbox.Client.PostAsJsonAsync("/todos", new { title });

        Assert.Equal(HttpStatusCode.BadRequest, response.StatusCode);
        Assert.Contains("Title", await response.Content.ReadAsStringAsync());
    }

    [Fact]
    public async Task Completing_a_todo_posts_it_to_the_webhook()
    {
        using var created = await sandbox.Client.PostAsJsonAsync("/todos", new CreateTodo("Notify someone"));
        var todo = (await created.Content.ReadFromJsonAsync<TodoResponse>())!;

        using var completed = await sandbox.Client.PutAsJsonAsync($"/todos/{todo.Id}", new UpdateTodo(todo.Title, IsComplete: true));
        Assert.True(completed.IsSuccessStatusCode, sandbox.Diagnostics);

        var notification = System.Text.Json.JsonSerializer.Deserialize<CompletionNotification>(
            await sandbox.Webhook.NextAsync(TimeSpan.FromSeconds(30)), System.Text.Json.JsonSerializerOptions.Web)!;
        Assert.Equal(todo.Id, notification.Id);
        Assert.Equal("Notify someone", notification.Title);
    }

    [Fact]
    public async Task The_export_lists_every_todo()
    {
        using var created = await sandbox.Client.PostAsJsonAsync("/todos", new CreateTodo("Appear in the export"));
        var todo = (await created.Content.ReadFromJsonAsync<TodoResponse>())!;

        var exported = await sandbox.Client.GetFromJsonAsync<List<TodoResponse>>("/todos/export");

        Assert.Contains(exported!, item => item.Id == todo.Id && item.Title == "Appear in the export");
    }

    [Fact]
    public async Task An_activated_instance_reports_its_own_identity()
    {
        var service = await sandbox.Client.GetFromJsonAsync<ServiceInfo>("/");

        Assert.Equal("SampleApi", service!.Application);
        // The identity is assigned on activation; the template's own value is empty.
        Assert.NotEqual(Guid.Empty, service.Instance);
    }
}
