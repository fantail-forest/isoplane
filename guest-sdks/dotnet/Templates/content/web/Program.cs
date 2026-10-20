using Isoplane;
using Isoplane.AspNetCore;

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
//          dotnet user-secrets set "Greeting:Message" "Hello from a secret!"
//          dotnet user-secrets list
//
//   3. The "environmentVariables" of the launch profile in
//      Properties/launchSettings.json, where two underscores stand for the
//      colon: "Greeting__Message": "Hello from the launch profile!".
//   4. Variables named ISOPLANE_APP_<setting> in the shell that starts the
//      session, to override a setting for one run:
//
//          ISOPLANE_APP_Greeting__Message="Hello from the shell!" dotnet run
//
// The application sees none of your machine's other environment variables.
// Restart the session after changing a setting.
var builder = IsoplaneHosting.CreateBuilder(args);

// Register services here, as in any ASP.NET Core application:
//
//   builder.Services.AddProblemDetails();
//   builder.Services.AddHttpClient();
//
// One kind of service does not fit: a BackgroundService that keeps running.
// Isoplane refuses continuously running hosted services at startup. Do the
// work while handling a request, or in a hosted service whose StartAsync
// completes it.

var app = builder.Build();

// Add middleware and endpoints here. To serve the files in a wwwroot folder:
//
//   app.MapStaticAssets();

app.MapGet("/", () => "Hello from Isoplane!");

// To read a setting in an endpoint, take IConfiguration as a parameter:
//
//   app.MapGet("/greeting", (IConfiguration configuration) => configuration["Greeting:Message"]);

// Initialization runs before Isoplane captures the template that every
// instance starts from. Whatever it leaves in memory is copied into each
// instance, so serving a request here means no instance pays for this route's
// first use. Warm the routes that matter most; each must answer with a success
// status. Await any I/O and dispose its connections before the callback
// returns: an open connection cannot be captured.
Initialization.Initializing += () => app.WarmupAsync("/");

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
