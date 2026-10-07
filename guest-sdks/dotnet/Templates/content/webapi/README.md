# Sample-Api

An ASP.NET Core Web API that runs in the Isoplane sandbox: minimal API
endpoints, Entity Framework Core with PostgreSQL, OpenAPI, an outbound webhook,
and tests.

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
| `src/Sample-Api/Program.cs` | Services, endpoints, and the initialization and activation hooks |
| `src/Sample-Api/Todos/` | The todo entity, its `DbContext`, and its endpoints |
| `src/Sample-Api/Notifications/` | A typed `HttpClient` that posts completed todos to a webhook |
| `src/Sample-Api/Migrations/` | Entity Framework Core migrations |
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
  `TodoEndpoints.Update` awaits the webhook before returning. `BackgroundService`
  is not supported.
- **Connections are not pooled inside an instance.** `TodoServices` sets
  `Pooling=false`, because Isoplane decides how many instances run and may capture
  or discard any of them between requests. Use your database provider's pooler to
  share connections.

## Configuration

Configuration is layered as in ASP.NET: `appsettings.json`, then
`appsettings.{Environment}.json`, then the settings the platform supplies. In
local development those are [User Secrets](https://learn.microsoft.com/aspnet/core/security/app-secrets)
and the environment variables of the launch profile in
`src/Sample-Api/Properties/launchSettings.json`.

Keep real credentials out of `appsettings` files:

```bash
dotnet user-secrets set "ConnectionStrings:Todos" "Host=...;Database=...;Username=...;Password=..." --project src/Sample-Api
```

Set `Notifications:WebhookUrl` to have completed todos posted to a URL.

A launch profile can also narrow the sandbox's limits, to see how the application
behaves under them:

```json
"isoplaneLimitsOverride": { "memoryBytes": 134217728, "occupancyMsPerHttpDispatch": 1000 }
```

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
