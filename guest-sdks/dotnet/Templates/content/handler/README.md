# Sample-Handler

A service that runs in the Isoplane sandbox without ASP.NET. One static method
handles every HTTP request.

The files here are commented as a guide to setting up a project of this kind,
whether you build on this one or copy from it.

## Run it

```bash
dotnet run --project src/Sample-Handler
```

Then try `/hello?name=Ada`, `/countdown`, or post a JSON greeting to `/echo`.
`isoplane dev --project src/Sample-Handler` rebuilds on every save.

## Debug it

In VS Code with the Isoplane Debugger extension, press F5, or use C# Dev Kit's
"Debug project associated with this file" button. `.vscode/launch.json` holds the
session and explains it. Set a breakpoint and send a request to the URL the
Debug Console prints.

From a terminal, `isoplane dev --debug --project src/Sample-Handler` starts a session a debugger can
attach to.

## Test it

```bash
dotnet test
```

- `GreeterTests` are ordinary unit tests. Application logic runs on your machine.
- `HandlerTests` start the application in the real Isoplane sandbox, so they need
  the `isoplane` tool. Run only the unit tests with
  `dotnet test --filter "Category!=Sandbox"`.

## Where things are

| Path | What it shows |
| --- | --- |
| `src/Sample-Handler/Sample-Handler.csproj` | The Isoplane SDK reference, the User Secrets identifier and the build's optional checks |
| `src/Sample-Handler/Handler.cs` | The handler, the lifecycle hooks, and where settings come from |
| `src/Sample-Handler/Greeter.cs` | Application logic kept apart from Isoplane, and JSON serialization |
| `src/Sample-Handler/Properties/launchSettings.json` | The local port and the settings a local session supplies |
| `.vscode/launch.json` | Debugging the application in VS Code |
| `tests/Sample-Handler.Tests/SandboxFixture.cs` | Starting the application in the sandbox for a test run |

## How it works

`src/Sample-Handler/Handler.cs` marks three static methods:

- **`[HttpHandler]`** receives every HTTP request and returns its response. An
  application has exactly one; the build fails otherwise. Take a
  `StreamingRequest` in place of the `Request` to read a large body as it arrives.
- **`[Initializer]`** runs while Isoplane prepares the template that every
  instance starts from. Whatever it leaves in memory is copied into each instance.
- **`[Activated]`** runs in each instance, before its first request. State that
  must differ between instances is set there.

Both lifecycle methods are optional.

A request owns its work: nothing runs in an instance after its response, and
memory an instance keeps between requests may be discarded at any time. Keep
state that must last in a database or another service.

## Configuration

`RuntimeConfiguration.ReadValues()` returns the application's settings. In local
development they come from three places, and a later one overrides an earlier one.

1. **[User Secrets](https://learn.microsoft.com/aspnet/core/security/app-secrets)**,
   for credentials. They live outside the project, in
   `~/.microsoft/usersecrets/<UserSecretsId>/secrets.json`
   (`%APPDATA%\Microsoft\UserSecrets\` on Windows), so they are never committed:

   ```bash
   dotnet user-secrets set "Greeting:Default" "Ada" --project src/Sample-Handler
   dotnet user-secrets list --project src/Sample-Handler
   dotnet user-secrets remove "Greeting:Default" --project src/Sample-Handler
   ```

2. **The launch profile**: the `environmentVariables` in
   `src/Sample-Handler/Properties/launchSettings.json`, where `__` in a name
   stands for `:`. This is the place for settings that are safe to commit.

3. **The shell**, for one run. Prefix the setting with `ISOPLANE_APP_`:

   ```bash
   ISOPLANE_APP_Greeting__Default=Grace dotnet run --project src/Sample-Handler
   ```

Restart the session after changing a setting. Sandbox tests supply their own
settings in `tests/Sample-Handler.Tests/SandboxFixture.cs`.

## Common changes

- **Add a route.** Add a line to the switch in `Handler.HandleAsync`, and a test
  to `HandlerTests`.
- **Return another type as JSON.** Declare it, and add a `[JsonSerializable]`
  line for it to `HandlerJson` in `Greeter.cs`.
- **Add a setting.** Read it from `RuntimeConfiguration.ReadValues()`, give it a
  development value in `launchSettings.json` or User Secrets, and a test value in
  `SandboxFixture`.
- **Accept large uploads.** Change the handler's parameter to `StreamingRequest`
  and read `request.Body` as a stream.
- **Try tighter limits.** A local session uses default resource limits.
  To see how the application behaves with less memory or time,
  add `isoplaneLimitsOverride` to the launch profile in `launchSettings.json`,
  beside `environmentVariables`:

  ```json
  "isoplaneLimitsOverride": { "memoryBytes": 134217728, "occupancyMsPerHttpDispatch": 1000 }
  ```
- **Add a NuGet package.** `dotnet add src/Sample-Handler package <name>`.
- **Use ASP.NET Core instead.** Start from `dotnet new isoplane-web` or
  `dotnet new isoplane-webapi`.
