using Isoplane;
using Isoplane.AspNetCore;
using Microsoft.EntityFrameworkCore;
using Sample_Api;
using Sample_Api.Notifications;
using Sample_Api.Todos;

// IsoplaneHosting.CreateBuilder takes the place of WebApplication.CreateBuilder.
// It returns the same WebApplicationBuilder, so services, configuration, logging
// and middleware are set up as in any ASP.NET Core application. What differs is
// that Isoplane delivers requests to the application, so it opens no listener.
//
// builder.Configuration is layered as usual. In local development a later source
// overrides an earlier one:
//
//   1. appsettings.json, then appsettings.{Environment}.json.
//   2. User Secrets, the place for credentials. They are stored outside the
//      project, in ~/.microsoft/usersecrets/<UserSecretsId>/secrets.json
//      (%APPDATA%\Microsoft\UserSecrets\ on Windows). The project file holds
//      the UserSecretsId. Assign and list them with:
//
//          dotnet user-secrets set "ConnectionStrings:Todos" "Host=..." --project src/Sample-Api
//          dotnet user-secrets list --project src/Sample-Api
//
//   3. The "environmentVariables" of the launch profile in
//      Properties/launchSettings.json, where two underscores stand for the
//      colon: "Notifications__WebhookUrl": "https://example.com/hook".
//   4. Variables named ISOPLANE_APP_<setting> in the shell that starts the
//      session, to override a setting for one run:
//
//          ISOPLANE_APP_Notifications__WebhookUrl=https://example.com/hook dotnet run --project src/Sample-Api
//
// The application sees none of your machine's other environment variables.
// Restart the session after changing a setting.
var builder = IsoplaneHosting.CreateBuilder(args);

// Each feature registers its own services in an extension method beside its
// code (Todos/TodoServices.cs, Notifications/CompletionNotifier.cs), which keeps
// this file a table of contents.
builder.Services.AddProblemDetails();
#if (UseControllers)
// Finds the controllers in this project (Todos/TodosController.cs).
builder.Services.AddControllers();
#else
builder.Services.AddValidation();
#endif
builder.Services.AddOpenApi();
builder.Services.AddTodos(builder.Configuration);
builder.Services.AddCompletionNotifications(builder.Configuration);
builder.Services.AddSingleton<InstanceInfo>();

// Not here: a BackgroundService for work that must run continuously or on a
// schedule. Isoplane runs one between requests only while its instance is
// resident, as it does a timer, and stops it without cancelling it when an idle
// instance is discarded. Start such work from a request instead, for example
// from an endpoint that a scheduler calls.

var app = builder.Build();

// Middleware runs in the order it is added, as in any ASP.NET Core application.
app.UseExceptionHandler();
app.UseStatusCodePages();

if (app.Environment.IsDevelopment())
{
    // The launch profile sets ASPNETCORE_ENVIRONMENT to Development. Without it
    // the environment is Production.
    app.MapOpenApi();
}

app.MapGet("/", (InstanceInfo instance) =>
    new ServiceInfo(app.Environment.ApplicationName, app.Environment.EnvironmentName, instance.Id));
#if (UseControllers)
app.MapControllers();
#else
app.MapTodos();
#endif

// Initialization runs while Isoplane prepares the template that every
// instance starts from. Whatever it leaves in memory is copied into each instance.
// Await any I/O and dispose its connections before the callback returns: an open
// connection cannot be captured.
Initialization.Initializing += async () =>
{
    if (app.Environment.IsDevelopment())
    {
        // Convenient while developing. Elsewhere, apply migrations as a step of
        // your release (README.md, "Change the database schema"), so that a
        // deployment never changes the schema as a side effect of starting.
        await using var scope = app.Services.CreateAsyncScope();
        await scope.ServiceProvider.GetRequiredService<TodoDbContext>().Database.MigrateAsync();
    }

    // A request served here leaves its route, serializers and query plans in the
    // template, so no instance pays for them on its first request. Warm the
    // routes that matter most; each must answer with a success status.
    await app.WarmupAsync("/todos");
};

// Activation runs in each instance, before its first request. State that must
// differ between instances is set here, because initialization state is shared:
// an identifier generated above would be the same in every instance.
Activation.Activated += () =>
{
    app.Services.GetRequiredService<InstanceInfo>().Activate();
    return Task.CompletedTask;
};

// The asynchronous RunAsync is required. The synchronous app.Run() would block,
// which a sandboxed application cannot do, and the build rejects it.
await app.RunAsync();
