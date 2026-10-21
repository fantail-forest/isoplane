# Sample-WebApp

A server-rendered web application that runs in the Isoplane sandbox: Razor
Pages, forms with validation, Entity Framework Core with PostgreSQL, static
files, and tests. It keeps a list of notes.

The files here are commented as a guide to setting up a project of this kind,
whether you build on this one or copy from it.

## Run it

Start the development database, then the application:

```bash
docker compose up -d --wait
dotnet run --project src/Sample-WebApp
```

Open the address in `src/Sample-WebApp/Properties/launchSettings.json` in a
browser. `isoplane dev --project src/Sample-WebApp` rebuilds on every save.

The database listens on `localhost:5432`. If that port is taken, change it in
`compose.yaml` and in `src/Sample-WebApp/appsettings.Development.json`.
`docker compose down` stops it and keeps its data; add `-v` to delete the data.

## Debug it

In VS Code with the Isoplane Debugger extension, press F5, or use C# Dev Kit's
"Debug project associated with this file" button. `.vscode/launch.json` holds the
session and explains it. Set a breakpoint in a page handler and open the page at
the URL the Debug Console prints.

From a terminal, `isoplane dev --debug --project src/Sample-WebApp` starts a session a debugger can
attach to.

## Test it

```bash
dotnet test
```

- `NoteTests` are ordinary unit tests. Application logic runs on your machine.
- `NotesPagesTests` start the application in the real Isoplane sandbox against a
  disposable PostgreSQL container, and use it as a browser would: they load
  pages and submit their forms. They need Docker and the `isoplane` tool. Run
  only the unit tests with `dotnet test --filter "Category!=Sandbox"`.

## Where things are

| Path | What it holds |
| --- | --- |
| `compose.yaml` | The development database |
| `.config/dotnet-tools.json` | The version of `dotnet ef` this project uses; `dotnet tool restore` installs it. The file cannot hold comments |
| `src/Sample-WebApp/Sample-WebApp.csproj` | The Isoplane SDK reference, the packages, the User Secrets identifier and the build's optional checks |
| `src/Sample-WebApp/Program.cs` | Where configuration comes from, services, the Data Protection keys, middleware, and the initialization and activation hooks |
| `src/Sample-WebApp/appsettings.json`, `appsettings.Development.json` | Settings for every environment, and for development |
| `src/Sample-WebApp/Properties/launchSettings.json` | The local port and the settings a local session supplies |
| `src/Sample-WebApp/Pages/` | The pages: each `.cshtml` file is a page's markup, and the `.cshtml.cs` beside it handles its requests |
| `src/Sample-WebApp/Pages/Shared/_Layout.cshtml` | The markup around every page |
| `src/Sample-WebApp/Notes/Note.cs` | The note entity and the form input that creates and edits one |
| `src/Sample-WebApp/Data/` | The `DbContext` and its registration |
| `src/Sample-WebApp/Migrations/` | Entity Framework Core migrations |
| `src/Sample-WebApp/wwwroot/` | Files served as they are, such as the stylesheet |
| `.vscode/launch.json` | Debugging the application in VS Code |
| `tests/Sample-WebApp.Tests/` | Unit tests and sandbox tests |

## How it differs from an ordinary ASP.NET application

- **`IsoplaneHosting.CreateBuilder(args)`** replaces `WebApplication.CreateBuilder(args)`.
  Isoplane delivers requests to the application, so it opens no listener.
- **`Program.cs` ends with `await app.RunAsync()`.** The synchronous `app.Run()`
  would block, which a sandboxed application cannot do; the build rejects it.
- **Instances start from a template.** Isoplane runs the application up to
  `RunAsync` and through its `Initialization.Initializing` callbacks, and captures
  the result. Every instance starts as a copy of that template. The callback here
  serves a request to the home page, so the page, its tag helpers and its query
  are already warm in every instance.
- **Data Protection keys are kept in the database.** They protect the
  antiforgery token in each form and the cookie that carries a message across a
  redirect. Fresh initialization may create a different captured key, so old
  forms may be refused; reusing a template retains its captured key.
  `UseKeysFromDbContext` in `Program.cs` loads a shared key ring during initialization.
  ASP.NET Core's own `PersistKeysToDbContext` does not work in the sandbox.
  `UseKeysFromDbContext` comes from the
  `Isoplane.AspNetCore.DataProtection.EntityFrameworkCore` package. Isoplane
  puts what it adds for one library in a package named after that library, so
  an application that does not use Entity Framework Core never restores it.
- **Await work the response depends on.** Page handlers await their database calls.
  Remaining work uses a bounded background allowance and may be lost when the
  instance is discarded. Continuous execution is unavailable: a
  `BackgroundService` still running after startup is refused by name.
- **Memory is not storage.** Isoplane may discard an instance after any request,
  and successive requests can reach different instances. What must last is in
  the database, and what must follow a visitor is in a cookie: in-memory session
  state would not survive.
- **This sample disables database pooling.** `Data/DataServices.cs` sets
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
   dotnet user-secrets set "ConnectionStrings:Database" "Host=...;Database=...;Username=...;Password=..." --project src/Sample-WebApp
   dotnet user-secrets list --project src/Sample-WebApp
   dotnet user-secrets remove "ConnectionStrings:Database" --project src/Sample-WebApp
   ```

3. **The launch profile**: the `environmentVariables` in
   `src/Sample-WebApp/Properties/launchSettings.json`, where `__` in a name stands
   for `:`.

4. **The shell**, for one run. Prefix the setting with `ISOPLANE_APP_`:

   ```bash
   ISOPLANE_APP_Logging__LogLevel__Default=Debug dotnet run --project src/Sample-WebApp
   ```

Restart the session after changing a setting. Sandbox tests supply their own
settings in `tests/Sample-WebApp.Tests/SandboxFixture.cs`.

## Common changes

- **Add a page.** Add a `.cshtml` file under `Pages/` that starts with `@page`,
  and a `.cshtml.cs` beside it for its handlers. Its path under `Pages/` is its
  address. Add a test to `NotesPagesTests`.
- **Add a form.** Give the page model a `[BindProperty]` and an `OnPostAsync`,
  and write the form with `method="post"` and the `asp-for` tag helpers, as
  `Pages/Index.cshtml` does. The antiforgery token is added and checked for you.
- **Add a field to a note.** Add it to `Note` and to `NoteInput`, show it in the
  pages, then add a migration (below).
- **Add a stylesheet, a script or an image.** Put it in `wwwroot/` and link it
  with `~/`, as `Pages/Shared/_Layout.cshtml` links the stylesheet.
- **Warm another page.** Add an `app.WarmupAsync` call to the initialization
  callback in `Program.cs`. The page must answer a GET with a success status.
- **Depend on another local service.** Declare it in `compose.yaml` and give
  the application its address in `appsettings.Development.json`.
- **Try tighter limits.** A local session uses default resource limits.
  To see how the application behaves with less memory or time,
  add `isoplaneLimitsOverride` to the launch profile in `launchSettings.json`,
  beside `environmentVariables`:

  ```json
  "isoplaneLimitsOverride": { "memoryBytes": 134217728, "occupancyMsPerDispatch": 1000 }
  ```
- **Add a NuGet package.** `dotnet add src/Sample-WebApp package <name>`.

## Change the database schema

```bash
dotnet tool restore
dotnet ef migrations add AddPinned --project src/Sample-WebApp
```

In the Development environment the application applies pending migrations when it
initializes. Elsewhere, apply them as part of your release:

```bash
dotnet ef database update --project src/Sample-WebApp
```
