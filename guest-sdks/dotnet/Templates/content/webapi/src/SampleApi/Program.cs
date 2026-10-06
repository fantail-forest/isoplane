using Isoplane;
using Isoplane.AspNetCore;
using Microsoft.EntityFrameworkCore;
using SampleApi;
using SampleApi.Notifications;
using SampleApi.Todos;

var builder = IsoplaneHosting.CreateBuilder(args);

builder.Services.AddProblemDetails();
builder.Services.AddValidation();
builder.Services.AddOpenApi();
builder.Services.AddTodos(builder.Configuration);
builder.Services.AddCompletionNotifications(builder.Configuration);
builder.Services.AddSingleton<InstanceInfo>();

var app = builder.Build();

app.UseExceptionHandler();
app.UseStatusCodePages();

if (app.Environment.IsDevelopment())
{
    app.MapOpenApi();
}

app.MapGet("/", (InstanceInfo instance) =>
    new ServiceInfo(app.Environment.ApplicationName, app.Environment.EnvironmentName, instance.Id));
app.MapTodos();

// Initialization runs while Isoplane prepares the template that every
// instance starts from. Whatever it leaves in memory is copied into each instance.
Initialization.Initializing += async () =>
{
    if (app.Environment.IsDevelopment())
    {
        await using var scope = app.Services.CreateAsyncScope();
        await scope.ServiceProvider.GetRequiredService<TodoDbContext>().Database.MigrateAsync();
    }

    // A request served here leaves its route, serializers and query plans in the
    // template, so no instance pays for them on its first request.
    await app.WarmupAsync("/todos");
};

// Activation runs in each instance, before its first request. State that must
// differ between instances is set here, because initialization state is shared.
Activation.Activated += () =>
{
    app.Services.GetRequiredService<InstanceInfo>().Activate();
    return Task.CompletedTask;
};

await app.RunAsync();
