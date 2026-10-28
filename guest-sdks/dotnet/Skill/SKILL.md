---
name: isoplane
description: Rules, commands and reference for an Isoplane application, which runs .NET code in Isoplane's WebAssembly sandbox. Use when writing, changing, building, running, testing or debugging a project that references an Isoplane SDK package, and when explaining why something behaves differently from .NET in a server process.
---

# Isoplane

An Isoplane application is ordinary .NET code that runs in a sandbox instead
of a process. The project references an Isoplane SDK package, and the
`isoplane` CLI runs it locally. The rules below are what differs from a server
process. The files under `references/` are the customer documentation pages
for the packages this project uses, as of the SDK version that wrote them.

## Rules

1. **Await everything.** An instance runs on one thread and returns to the
   platform at every await. Starting a thread or waiting synchronously, with
   `.Result`, `.Wait()`, `Thread.Sleep` or a synchronous database call, fails
   in the sandbox. The build reports these as `ISOPLANE1002` and
   `ISOPLANE1003`; `IsoplaneAnalyzeDependencies=true` in the project file
   inspects NuGet packages too.
2. **Expect eviction.** Instances are copies of an initialized template,
   created on demand and discarded with their memory at any time. Keep durable
   state in a database. Background work after a response has a bounded
   allowance and may be lost, and a `BackgroundService` stops when its
   instance is discarded.
3. **Initialization prepares shared state; activation runs in every instance.**
   `Initialization.Initializing` or an `[Initializer]` method runs before the
   template is captured, so what it leaves in memory is in every instance.
   Preparation may retry initialization; reusing a prepared template skips it.
   `Activation.Activated` or an `[Activated]` method runs as each instance
   starts: an open connection or a value that must differ between instances
   belongs there.
4. **The application opens no listener.** An ASP.NET application calls
   `IsoplaneHosting.CreateBuilder(args)` and ends with `await app.RunAsync()`;
   the blocking `app.Run()` is rejected as `ISOPLANE1020`. An application
   without ASP.NET handles every request in one static `[HttpHandler]` method.
5. **Settings come from the application, not the machine.** `appsettings.json`,
   User Secrets and the `environmentVariables` of the launch profile in
   `Properties/launchSettings.json` are read. The machine's environment
   variables are not.
6. **The file system is read-only and memory is bounded.** Stream large
   uploads and generated files instead of buffering them.
7. **Connections are not pooled in the application.** Isoplane decides how
   many instances run. Open a connection per request with the driver's
   pooling off (`Pooling=false` for Npgsql) and use the database provider's
   pooler to share connections.

## Commands

- `dotnet run`, with `--project <dir>` in a solution, builds the application
  and runs it in the sandbox, printing its address when it is ready.
  `isoplane dev` does the same and rebuilds on every save; `isoplane dev --json`
  writes one JSON event per line for a program reading the output.
- `isoplane dev --debug` starts a session a debugger can attach to.
- `dotnet build` runs the sandbox's checks. A diagnostic whose code starts with
  `ISOPLANE` is explained in `references/sdk/reference/error-codes.md` and at
  `https://isoplane.dev/docs/reference/error-codes#<code in lower case>`.
- `dotnet test` runs unit tests and, in a test project that uses
  `Isoplane.Testing`, tests that start the application in the real sandbox.
  In a project created from an Isoplane template,
  `dotnet test --filter "Category!=Sandbox"` runs only the unit tests.
- `isoplane doctor --json` checks the machine's prerequisites when a build or
  run fails for an unclear reason.
- `isoplane agent init --harness <name>` registers Isoplane's MCP server with
  a coding agent, which then runs, watches and stops the application and reads
  its events through tools instead of a terminal.
- `references/sdk/reference/cli.md` lists every command and option.

## Documentation

The customer documentation is at https://isoplane.dev/docs/. Every page there
is available as markdown by appending `.md` to its URL or by sending
`Accept: text/markdown`. The references below are copies of its pages for the
SDK version this project uses.

## This skill

The Isoplane SDK writes this directory when the project builds, from the
packages the project references, so that it matches the SDK version in use.
Edits are overwritten on the next build, and a package added to or removed
from the project adds or removes its references. Put guidance of your own in a
skill beside this one. Commit the directory so that an agent working from a
clone sees it.

Three MSBuild properties in the project file control the writing:

- `IsoplaneWriteAgentSkill`: `false` turns it off.
- `IsoplaneAgentSkillDirectories`: the directories the skill is written to,
  separated by semicolons; the default is `.agents/skills;.claude/skills`.
  The first holds the files and each other one links to it, or holds a copy
  where links are unavailable.
- `IsoplaneAgentSkillRoot`: the directory those are created under. Without
  it, the nearest directory above the project holding a `.git` entry or a
  solution file, else the project directory.

## References
