# Sample-McpServer

A [Model Context Protocol](https://modelcontextprotocol.io) server that runs in
the Isoplane sandbox. It gives an AI agent two tools over HTTP: one searches a
team handbook, and the other reads an article from it.

The files here are commented as a guide to setting up a project of this kind,
whether you build on this one or copy from it.

## Run it

```bash
dotnet run --project src/Sample-McpServer
```

The server listens on the address in
`src/Sample-McpServer/Properties/launchSettings.json`. The MCP endpoint is
`/mcp`; the address itself answers a browser with one line of text.
`isoplane dev --project src/Sample-McpServer` rebuilds on every save.

## Call it

Point an MCP client that speaks Streamable HTTP at `http://localhost:5080/mcp`.
The [MCP Inspector](https://github.com/modelcontextprotocol/inspector) lists
the tools and calls them from the command line:

```bash
npx @modelcontextprotocol/inspector --cli http://localhost:5080/mcp --transport http --method tools/list
npx @modelcontextprotocol/inspector --cli http://localhost:5080/mcp --transport http \
  --method tools/call --tool-name search_handbook --tool-arg query=release
```

The server keeps no session, so one request is a whole exchange and `curl`
works too:

```bash
curl http://localhost:5080/mcp \
  -H "Content-Type: application/json" -H "Accept: application/json, text/event-stream" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"read_article","arguments":{"slug":"code-review"}}}'
```

## Debug it

In VS Code with the Isoplane Debugger extension, press F5, or use C# Dev Kit's
"Debug project associated with this file" button. `.vscode/launch.json` holds the
session and explains it. Set a breakpoint in a tool and call it at the URL the
Debug Console prints.

From a terminal, `isoplane dev --debug --project src/Sample-McpServer` starts a session a debugger can
attach to.

## Test it

```bash
dotnet test
```

- `HandbookTests` are ordinary unit tests. Application logic runs on your machine.
- `McpServerTests` start the server in the real Isoplane sandbox and call it with
  an MCP client, so they need the `isoplane` tool. Run only the unit tests with
  `dotnet test --filter "Category!=Sandbox"`.

## Where things are

| Path | What it shows |
| --- | --- |
| `src/Sample-McpServer/Sample-McpServer.csproj` | The Isoplane SDK reference, the MCP package, and files packaged as content |
| `src/Sample-McpServer/Program.cs` | Where configuration comes from, loading data during initialization, the MCP server and its endpoint |
| `src/Sample-McpServer/Tools/HandbookTools.cs` | How a tool is declared, described, given services and made to fail usefully |
| `src/Sample-McpServer/Knowledge/Handbook.cs` | Application logic kept apart from MCP and Isoplane, and the options class a setting binds to |
| `src/Sample-McpServer/Knowledge/Articles/` | The Markdown files the handbook is loaded from |
| `src/Sample-McpServer/appsettings.json`, `appsettings.Development.json` | Settings for every environment, and for development |
| `src/Sample-McpServer/Properties/launchSettings.json` | The local port and the settings a local session supplies |
| `.vscode/launch.json` | Debugging the application in VS Code |
| `tests/Sample-McpServer.Tests/SandboxFixture.cs` | Starting the server in the sandbox and connecting an MCP client to it |

## How it works

- **The server is stateless.** Each request stands alone and the server keeps
  no session. On Isoplane successive requests can reach different instances,
  and an instance does not run between requests, so there is nowhere for a
  session to live. A stateless server answers calls and cannot begin an exchange
  itself: sampling, elicitation and unsolicited notifications are unavailable.
- **The handbook is loaded during initialization.** `Program.cs` runs up
  to `RunAsync`, before Isoplane captures the template that every instance
  starts from. The articles it reads are already in memory when an instance
  serves its first request.
- **The endpoint is warm before the first call.** An
  `Initialization.Initializing` callback posts one search to `/mcp` before the
  template is captured, so the work of a first call is in the template too.
- **A tool is a method.** `[McpServerTool]` marks it, `[Description]` tells the
  model when and how to call it, and its parameters are the tool's inputs.
- **A request owns its work.** Nothing runs in an instance after its response,
  and memory an instance keeps between requests may be discarded at any time.
  Keep state that must last in a database or another service.

`IsoplaneHosting.CreateBuilder(args)` replaces `WebApplication.CreateBuilder(args)`,
and `Program.cs` ends with `await app.RunAsync()`: the synchronous `app.Run()`
would block, which a sandboxed application cannot do, and the build rejects it.

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
   dotnet user-secrets set "Handbook:MaxResults" "10" --project src/Sample-McpServer
   dotnet user-secrets list --project src/Sample-McpServer
   dotnet user-secrets remove "Handbook:MaxResults" --project src/Sample-McpServer
   ```

3. **The launch profile**: the `environmentVariables` in
   `src/Sample-McpServer/Properties/launchSettings.json`, where `__` in a name
   stands for `:`.

4. **The shell**, for one run. Prefix the setting with `ISOPLANE_APP_`:

   ```bash
   ISOPLANE_APP_Handbook__MaxResults=10 dotnet run --project src/Sample-McpServer
   ```

Restart the session after changing a setting. Sandbox tests supply their own
settings in `tests/Sample-McpServer.Tests/SandboxFixture.cs`.

`Handbook:MaxResults` is the most articles one search returns.

## Common changes

- **Add a tool.** Add a method marked `[McpServerTool]` to `HandbookTools`, and
  a test to `McpServerTests`. For a new group of tools, add a class marked
  `[McpServerToolType]` and a `WithTools<...>()` line in `Program.cs`.
- **Warm another tool.** Send it a call in the initialization callback in
  `Program.cs`, as the search is sent. Choose calls that change nothing: every
  build of the application serves them again.
- **Add an article.** Put a Markdown file in `Knowledge/Articles/`. Its file
  name is its slug and its first heading is its title.
- **Call another service from a tool.** Register a typed client with
  `AddHttpClient` in `Program.cs`, take it as a parameter of the tool, and await
  it, passing on the tool's `CancellationToken`. `HandbookTools.cs` has an
  example.
- **Add a setting.** Bind it to an options class as `HandbookOptions` is, give
  it a default in `appsettings.json`, and a test value in `SandboxFixture`.
- **Protect the endpoint.** It is open to anyone who can reach it. Set up
  ASP.NET Core authentication and add `.RequireAuthorization()` to `MapMcp` in
  `Program.cs`.
- **Try tighter limits.** A local session uses default resource limits.
  To see how the application behaves with less memory or time,
  add `isoplaneLimitsOverride` to the launch profile in `launchSettings.json`,
  beside `environmentVariables`:

  ```json
  "isoplaneLimitsOverride": { "memoryBytes": 134217728, "occupancyMsPerHttpDispatch": 1000 }
  ```
- **Add a NuGet package.** `dotnet add src/Sample-McpServer package <name>`.
