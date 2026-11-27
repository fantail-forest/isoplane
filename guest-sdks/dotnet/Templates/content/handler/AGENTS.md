# Sample-Handler

A service that runs in the Isoplane sandbox and handles every HTTP request in
one static method, without ASP.NET, with a test project. `README.md` explains
what each file is for and how to make common changes; read it before changing
the project.

## Commands

- `dotnet run --project src/Sample-Handler` builds and runs the service and
  prints its address when it is ready. `isoplane dev --project src/Sample-Handler`
  does the same and rebuilds on every save; add `--json` for one JSON event per
  line.
- `isoplane dev --debug --project src/Sample-Handler` starts a session a
  debugger can attach to.
- `dotnet test` runs the unit tests and the sandbox tests;
  `dotnet test --filter "Category!=Sandbox"` runs only the unit tests.
- `dotnet build` reports the sandbox's requirements as `ISOPLANE` diagnostics.
- `isoplane doctor --json` checks the machine when a build or run fails for an
  unclear reason.

## The sandbox

- `Handler.cs` holds the entry methods: the static `[HttpHandler]` method
  receives each request, `[TemplateInitializer]` prepares shared state before Isoplane captures the
  template every instance starts from, and `[InstanceStart]` runs in each instance.
  Each attribute marks exactly one method, and the build checks their shapes.
- Await everything. Starting a thread or waiting synchronously, with `.Result`,
  `.Wait()`, `Thread.Sleep` or a synchronous database call, fails in the
  sandbox; the build reports these as `ISOPLANE1002` and `ISOPLANE1003`.
- Await work the response depends on. An instance can be discarded between
  requests, so background work after a response is not reliable.
- Settings come from `appsettings.json`, User Secrets and the launch profile
  in `src/Sample-Handler/Properties/launchSettings.json`, not from the
  machine's environment.

## Where to look

- A build diagnostic whose code starts with `ISOPLANE` is explained at
  `https://isoplane.dev/docs/reference/error-codes#<code in lower case>`.
- The `isoplane` skill holds the detailed rules for the SDK version the
  project was created with, and links to the documentation pages for its
  Isoplane packages. `dotnet new` wrote it into
  `.agents/skills/` and `.claude/skills/`, each unless it was switched off.
- `isoplane agent init --harness <name>` registers Isoplane's MCP server with
  a coding agent, so that it can run the application and read its logs through
  tools; `isoplane agent init --help` names the agents it knows.
- Documentation: https://isoplane.dev/docs/. Append `.md` to a page's URL to
  read it as markdown.
