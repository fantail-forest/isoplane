using Isoplane;
using Isoplane.AspNetCore;
using Isoplane.AspNetCore.DataProtection.EntityFrameworkCore;
using Microsoft.AspNetCore.DataProtection;
using Microsoft.EntityFrameworkCore;
using Sample_WebApp.Data;

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
//          dotnet user-secrets set "ConnectionStrings:Database" "Host=..." --project src/Sample-WebApp
//          dotnet user-secrets list --project src/Sample-WebApp
//
//   3. The "environmentVariables" of the launch profile in
//      Properties/launchSettings.json, where two underscores stand for the
//      colon: "Logging__LogLevel__Default": "Debug".
//   4. Variables named ISOPLANE_APP_<setting> in the shell that starts the
//      session, to override a setting for one run:
//
//          ISOPLANE_APP_Logging__LogLevel__Default=Debug dotnet run --project src/Sample-WebApp
//
// The application sees none of your machine's other environment variables.
// Restart the session after changing a setting.
var builder = IsoplaneHosting.CreateBuilder(args);

// The pages are in Pages/, and each page's route is its path there.
builder.Services.AddRazorPages();
builder.Services.AddAppDatabase(builder.Configuration);

// Data Protection encrypts what the application hands a browser and expects
// back: here the antiforgery token in every form, and the cookie that carries
// a message across a redirect (TempData). Sign-in cookies use it too.
//
// A key created during initialization is captured in the template. Fresh
// initialization may create a different key; reusing a template retains its
// captured key. Replacing the key refuses old forms and signs visitors out.
// Initialization loads a shared key ring from the database and can create a
// key if none is usable. A reused template retains its captured ring.
//
// This is Isoplane's counterpart of PersistKeysToDbContext, which cannot be
// used here: it reads the database synchronously, and a sandboxed application
// cannot block. The table is the same, and Data/AppDbContext.cs declares it.
// UseKeysFromDbContext comes from the
// Isoplane.AspNetCore.DataProtection.EntityFrameworkCore package, which
// Sample-WebApp.csproj references; the project file explains why it is a
// package of its own.
//
// Without a database, UseConfiguredKeys() from Isoplane.AspNetCore takes the
// key from a secret setting instead:
// https://isoplane.dev/docs/dotnet-runtime-differences#keep-data-protection-keys-across-builds
builder.Services.AddDataProtection().UseKeysFromDbContext<AppDbContext>();

// Not here: a BackgroundService that keeps running. Isoplane suspends an
// application between requests, so the application refuses to start with one.
// Do the work while handling a request, or in a hosted service whose
// StartAsync completes it.

var app = builder.Build();

// Middleware runs in the order it is added, as in any ASP.NET Core application.
if (!app.Environment.IsDevelopment())
{
    // The launch profile sets ASPNETCORE_ENVIRONMENT to Development. Without it
    // the environment is Production, and a request that throws is answered with
    // Pages/Error.cshtml.
    app.UseExceptionHandler("/Error");
}

// Serves the files in wwwroot, which are part of the application package. The
// build fingerprints them: `asp-append-version` in Pages/Shared/_Layout.cshtml
// links to a name that changes with the file's content, so a browser may cache
// it for good.
app.MapStaticAssets();
app.MapRazorPages().WithStaticAssets();

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
        await scope.ServiceProvider.GetRequiredService<AppDbContext>().Database.MigrateAsync();
    }

    // The keys are read from the database once, when initialization finishes.
    // The page warmed below renders a form, whose antiforgery token needs them
    // sooner, so load them now. Their table exists: the migration above made it.
    await app.LoadDataProtectionKeysAsync();

    // A request served here leaves its page, its tag helpers and its query in
    // the template, so no instance pays for them on its first request. Warm the
    // pages that matter most; each must answer with a success status.
    await app.WarmupAsync("/");
};

// Activation runs in each instance, before its first request. Set state that
// must differ between instances here, because initialization state is shared:
//
//   Activation.Activated += () =>
//   {
//       instanceId = Guid.NewGuid();
//       return Task.CompletedTask;
//   };

// The asynchronous RunAsync is required. The synchronous app.Run() would block,
// which a sandboxed application cannot do, and the build rejects it.
await app.RunAsync();
