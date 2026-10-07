# Sample-Api

An ASP.NET Core Web API that runs in the Isoplane sandbox: minimal API
endpoints, Entity Framework Core with PostgreSQL, OpenAPI, an outbound webhook,
and tests.

The files here are commented as a guide to setting up a project of this kind,
whether you build on this one or copy from it.

## Run it

Start the development database, then the application:

```bash
docker compose up -d --wait
dotnet run --project src/Sample-Api
```

`src/Sample-Api/Sample-Api.http` has a request for each endpoint, and
`/openapi/v1.json` describes them. `isoplane dev --project src/Sample-Api`
rebuilds on every save.

The database listens on `localhost:5432`. If that port is taken, change it in
`compose.yaml` and in `src/Sample-Api/appsettings.Development.json`.
`docker compose down` stops it and keeps its data; add `-v` to delete the data.

## Test it

```bash
dotnet test
```

- `TodoTests` are ordinary unit tests. Application logic runs on your machine.
- `TodoApiTests` start the application in the real Isoplane sandbox against a
  disposable PostgreSQL container, so they need Docker and the `isoplane` tool.
  Run only the unit tests with `dotnet test --filter "Category!=Sandbox"`.

## Where things are

| Path | What it holds |
| --- | --- |
| `compose.yaml` | The development database |
| `.config/dotnet-tools.json` | The version of `dotnet ef` this project uses; `dotnet tool restore` installs it. The file cannot hold comments |
| `src/Sample-Api/Sample-Api.csproj` | The Isoplane SDK reference, the User Secrets identifier and the build's optional checks |
| `src/Sample-Api/Program.cs` | Where configuration comes from, services, endpoints, and the initialization and activation hooks |
| `src/Sample-Api/appsettings.json`, `appsettings.Development.json` | Settings for every environment, and for development |
| `src/Sample-Api/Properties/launchSettings.json` | The local port, settings, and the limits a local session can run under |
| `src/Sample-Api/Todos/` | The todo entity, its `DbContext`, and its endpoints |
| `src/Sample-Api/Notifications/` | A typed `HttpClient` that posts completed todos to a webhook |
| `src/Sample-Api/Migrations/` | Entity Framework Core migrations |
| `src/Sample-Api/Sample-Api.http` | A request for each endpoint |
| `tests/Sample-Api.Tests/` | Unit tests and sandbox tests |

## How it differs from an ordinary ASP.NET application

- **`IsoplaneHosting.CreateBuilder(args)`** replaces `WebApplication.CreateBuilder(args)`.
  Isoplane delivers requests to the application, so it opens no listener.
- **`Program.cs` ends with `await app.RunAsync()`.** The synchronous `app.Run()`
  would block, which a sandboxed application cannot do; the build rejects it.
- **Instances start from a template.** Isoplane runs the application up to
  `RunAsync` and through its `Initialization.Initializing` callbacks, and captures
  the result. Every instance starts as a copy of that template. The callback here
  serves a request to `/todos`, so the route, serializers and query plans are
  already warm in every instance.
- **`Activation.Activated` runs in each instance.** State that must differ between
  instances, such as `InstanceInfo.Id`, is set there and not during initialization.
- **A request owns its work.** Nothing runs in an instance after its response, so
  `TodoEndpoints.Update` awaits the webhook before returning. An application
  with a `BackgroundService` that keeps running after startup refuses to start,
  and names the service.
- **Connections are not pooled inside an instance.** `TodoServices` sets
  `Pooling=false`, because Isoplane decides how many instances run and may capture
  or discard any of them between requests. Use your database provider's pooler to
  share connections.

## Configuration

Configuration is layered as in ASP.NET. In local development a later source
overrides an earlier one.

1. **`appsettings.json`**, then **`appsettings.{Environment}.json`**, for settings
   that are safe to commit.

2. **[User Secrets](https://learn.microsoft.com/aspnet/core/security/app-secrets)**,
   for credentials. They live outside the project, in
   `~/.microsoft/usersecrets/<UserSecretsId>/secrets.json`
   (`%APPDATA%\Microsoft\UserSecrets\` on Windows), so they are never committed:

   ```bash
   dotnet user-secrets set "ConnectionStrings:Todos" "Host=...;Database=...;Username=...;Password=..." --project src/Sample-Api
   dotnet user-secrets list --project src/Sample-Api
   dotnet user-secrets remove "ConnectionStrings:Todos" --project src/Sample-Api
   ```

3. **The launch profile**: the `environmentVariables` in
   `src/Sample-Api/Properties/launchSettings.json`, where `__` in a name stands
   for `:`.

4. **The shell**, for one run. Prefix the setting with `ISOPLANE_APP_`:

   ```bash
   ISOPLANE_APP_Notifications__WebhookUrl=https://example.com/hook dotnet run --project src/Sample-Api
   ```

Restart the session after changing a setting. Sandbox tests supply their own
settings in `tests/Sample-Api.Tests/SandboxFixture.cs`.

Set `Notifications:WebhookUrl` to have completed todos posted to a URL.

## Common changes

- **Add an endpoint.** Add a handler to `TodoEndpoints` and map it in `MapTodos`,
  with a request to `Sample-Api.http` and a test to `TodoApiTests`.
- **Add a feature.** Give it a folder like `Todos/`: its entity, an endpoints
  class, and a services extension, each called from `Program.cs`.
- **Add a setting.** Bind it to an options class as `NotificationOptions` does,
  give it a default in `appsettings.json`, and a test value in `SandboxFixture`.
- **Warm another route.** Add an `app.WarmupAsync` call to the initialization
  callback in `Program.cs`. The route must answer with a success status.
- **Call another service.** Register a typed client with `AddHttpClient`, as
  `Notifications/CompletionNotifier.cs` does, and await it in the request.
- **Depend on another local service.** Declare it in `compose.yaml` and give
  the application its address in `appsettings.Development.json`.
- **Try tighter limits.** `launchSettings.json` lists the limits a local session
  runs under, switched off. Switch them on to see how the application behaves
  with less memory or time.
- **Add a NuGet package.** `dotnet add src/Sample-Api package <name>`.

## Change the database schema

```bash
dotnet tool restore
dotnet ef migrations add AddDueDate --project src/Sample-Api
```

In the Development environment the application applies pending migrations when it
initializes. Elsewhere, apply them as part of your release:

```bash
dotnet ef database update --project src/Sample-Api
```
