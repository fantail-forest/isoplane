using Isoplane;
using Isoplane.AspNetCore;
using Sample_McpServer.Knowledge;
using Sample_McpServer.Tools;

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
//          dotnet user-secrets set "Handbook:MaxResults" "10" --project src/Sample-McpServer
//          dotnet user-secrets list --project src/Sample-McpServer
//
//   3. The "environmentVariables" of the launch profile in
//      Properties/launchSettings.json, where two underscores stand for the
//      colon: "Handbook__MaxResults": "10".
//   4. Variables named ISOPLANE_APP_<setting> in the shell that starts the
//      session, to override a setting for one run:
//
//          ISOPLANE_APP_Handbook__MaxResults=10 dotnet run --project src/Sample-McpServer
//
// The application sees none of your machine's other environment variables.
// Restart the session after changing a setting.
var builder = IsoplaneHosting.CreateBuilder(args);

builder.Services.Configure<HandbookOptions>(builder.Configuration.GetSection("Handbook"));

// Everything up to RunAsync is initialization: Isoplane runs it during
// template preparation and saves the memory it leaves as the template every instance starts
// from. Reading and indexing the articles here means no instance and no request
// does that work again. The files are the ones the project file marks as
// content, found under the content root.
var handbook = await Handbook.LoadAsync(Path.Combine(builder.Environment.ContentRootPath, "Knowledge", "Articles"));
builder.Services.AddSingleton(handbook);

builder.Services
    .AddMcpServer(options =>
    {
        // What a client is told about the server when it connects.
        options.ServerInfo = new() { Name = "Sample-McpServer", Version = "1.0.0" };
        options.ServerInstructions = "Answers questions about how the team works from its handbook. Search first, then read the article that matches.";
    })
    // Stateless: every request stands alone, and the server keeps no session
    // between them. That suits Isoplane, where successive requests can reach
    // different instances and an instance does not run between requests. It is
    // this package version's default; saying so keeps it if the default moves.
    //
    // A stateless server answers calls and cannot start an exchange of its own,
    // so sampling, elicitation and unsolicited notifications are unavailable.
    // Setting Stateless to false is refused when the application starts: session
    // tracking runs a background service, and Isoplane accepts none.
    .WithHttpTransport(options => options.Stateless = true)
    // One line for each class of tools. Tools/HandbookTools.cs explains how a
    // tool is declared.
    .WithTools<HandbookTools>();

// A tool that calls another service takes a typed HttpClient like any other
// dependency. Register it here, and keep its address in configuration:
//
//   builder.Services.AddHttpClient<StatusClient>(client =>
//       client.BaseAddress = new Uri(builder.Configuration["Status:BaseUrl"]!));

var app = builder.Build();

// The MCP endpoint. A client connects to this path with the Streamable HTTP
// transport and sends every message as a POST.
//
// It is open to anyone who can reach it. MapMcp returns an ordinary endpoint
// builder, so ASP.NET Core authorization applies to it as to any endpoint, once
// the application has authentication set up:
//
//   app.MapMcp("/mcp").RequireAuthorization();
app.MapMcp("/mcp");

// Something to see in a browser, which cannot speak MCP.
app.MapGet("/", () => $"Sample-McpServer is an MCP server with {handbook.Count} handbook articles. Connect an MCP client to /mcp.");

// Initialization callbacks run once the application has started and before the
// template is saved. Serving a request here leaves the work of a route's first
// use in the template. WarmupAsync sends a GET, and the MCP endpoint accepts
// only POST, so it is not warmed: the first MCP call an instance receives does
// that work. Await any I/O and dispose its connections before the callback
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
