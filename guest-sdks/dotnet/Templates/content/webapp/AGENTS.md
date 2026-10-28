# Sample-WebApp

A server-rendered web application that runs in the Isoplane sandbox, with Razor
Pages, Entity Framework Core on PostgreSQL and a test project. `README.md`
explains what each file is for and how to make common changes; read it before
changing the project.

## Commands

- `docker compose up -d --wait` starts the development database, which the
  application and the sandbox tests need.
- `dotnet run --project src/Sample-WebApp` builds and runs the application and
  prints its address when it is ready. `isoplane dev --project src/Sample-WebApp`
  does the same and rebuilds on every save; add `--json` for one JSON event per
  line.
- `isoplane dev --debug --project src/Sample-WebApp` starts a session a
  debugger can attach to.
- `dotnet test` runs the unit tests and the sandbox tests, which need Docker;
  `dotnet test --filter "Category!=Sandbox"` runs only the unit tests.
- `dotnet tool restore`, then `dotnet ef migrations add <Name> --project src/Sample-WebApp`
  after a model change, and `dotnet ef database update --project src/Sample-WebApp`
  to apply it to the development database.
- `dotnet build` reports the sandbox's requirements as `ISOPLANE` diagnostics.
- `isoplane doctor --json` checks the machine when a build or run fails for an
  unclear reason.

## The sandbox

- Keep `IsoplaneHosting.CreateBuilder(args)` and `await app.RunAsync()` in
  `Program.cs`. The application opens no listener, and `app.Run()` is rejected.
- Await everything. Starting a thread or waiting synchronously, with `.Result`,
  `.Wait()`, `Thread.Sleep` or a synchronous database call, fails in the
  sandbox; the build reports these as `ISOPLANE1002` and `ISOPLANE1003`.
- `Initialization.Initializing` runs during preparation, before Isoplane captures the template
  every instance starts from. `Activation.Activated` runs in each instance.
- Await work the response depends on. An instance can be discarded between
  requests, so background work after a response is not reliable and a
  `BackgroundService` does not keep running.
- Database connections are not pooled in the application (`Pooling=false` in
  `DataServices`); the database provider's pooler shares them.
- Settings come from `appsettings.json`, User Secrets and the launch profile
  in `src/Sample-WebApp/Properties/launchSettings.json`, not from the
  machine's environment.

## Where to look

- A build diagnostic whose code starts with `ISOPLANE` is explained at
  `https://isoplane.dev/docs/reference/error-codes#<code in lower case>`.
- The Isoplane SDK writes skills for the SDK version in use into
  `.agents/skills/` when the project builds. They hold the detailed rules and
  the reference pages. Commit them.
- `isoplane agent init --harness <name>` registers Isoplane's MCP server with
  a coding agent, so that it can run the application and read its logs through
  tools; `isoplane agent init --help` names the agents it knows.
- Documentation: https://isoplane.dev/docs/. Append `.md` to a page's URL to
  read it as markdown.
