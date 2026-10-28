# Sample-Api

An ASP.NET Core Web API that runs in the Isoplane sandbox, with Entity
Framework Core on PostgreSQL, OpenAPI, an outbound webhook and a test project.
`README.md` explains what each file is for and how to make common changes; read
it before changing the project.

## Commands

- `docker compose up -d --wait` starts the development database, which the
  application and the sandbox tests need.
- `dotnet run --project src/Sample-Api` builds and runs the API and prints its
  address when it is ready. `isoplane dev --project src/Sample-Api` does the
  same and rebuilds on every save; add `--json` for one JSON event per line.
  `src/Sample-Api/Sample-Api.http` has a request for each endpoint.
- `isoplane dev --debug --project src/Sample-Api` starts a session a debugger
  can attach to.
- `dotnet test` runs the unit tests and the sandbox tests, which need Docker;
  `dotnet test --filter "Category!=Sandbox"` runs only the unit tests.
- `dotnet tool restore`, then `dotnet ef migrations add <Name> --project src/Sample-Api`
  after a model change, and `dotnet ef database update --project src/Sample-Api`
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
  every instance starts from. `Activation.Activated` runs in each instance, and
  sets what must differ between instances, such as `InstanceInfo.Id`.
- Await work the response depends on, as the webhook call does. An instance can
  be discarded between requests, so background work after a response is not
  reliable and a `BackgroundService` does not keep running.
- Database connections are not pooled in the application (`Pooling=false` in
  `TodoServices`); the database provider's pooler shares them.
- Settings come from `appsettings.json`, User Secrets and the launch profile
  in `src/Sample-Api/Properties/launchSettings.json`, not from the machine's
  environment.

## Where to look

- A build diagnostic whose code starts with `ISOPLANE` is explained at
  `https://isoplane.dev/docs/reference/error-codes#<code in lower case>`.
- The Isoplane SDK writes skills for the SDK version in use into
  `.agents/skills/` when the project builds. They hold the detailed rules and
  the reference pages. Commit them.
- Documentation: https://isoplane.dev/docs/. Append `.md` to a page's URL to
  read it as markdown.
