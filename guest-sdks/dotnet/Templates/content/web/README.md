# Sample-App

An ASP.NET Core application that runs in the Isoplane sandbox.

```bash
dotnet run
```

The application listens on the address in `Properties/launchSettings.json`.
Edit `Program.cs` and run it again, or use `isoplane dev` to rebuild on every save.

The files here are commented as a guide to setting up a project of this kind,
whether you build on this one or copy from it.

## Debug it

In VS Code with the Isoplane Debugger extension, press F5, or use C# Dev Kit's
"Debug project associated with this file" button. `.vscode/launch.json` holds the
session and explains it. Set a breakpoint and send a request to the URL the
Debug Console prints.

From a terminal, `isoplane dev --debug` starts a session a debugger can
attach to.

| File | What it shows |
| --- | --- |
| `Sample-App.csproj` | The Isoplane SDK reference, the User Secrets identifier and the build's optional checks |
| `Program.cs` | Where configuration comes from, where services and endpoints go, and the lifecycle hooks |
| `appsettings.json`, `appsettings.Development.json` | Settings for every environment, and for development |
| `Properties/launchSettings.json` | The local port and the settings a local session supplies |
| `.vscode/launch.json` | Debugging the application in VS Code |

## How it differs from an ordinary ASP.NET application

- `IsoplaneHosting.CreateBuilder(args)` replaces `WebApplication.CreateBuilder(args)`.
  Isoplane delivers requests to the application, so it opens no listener.
- `Program.cs` ends with `await app.RunAsync()`. The synchronous `app.Run()`
  would block, which a sandboxed application cannot do; the build rejects it.
- `Initialization.Initializing` callbacks run during preparation, before Isoplane captures the
  template that every instance starts from. Whatever they leave in memory is
  already there when an instance serves its first request.
- A request owns its work. Nothing runs in an instance after its response, and
  an application with a `BackgroundService` that keeps running after startup
  refuses to start.

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
   dotnet user-secrets set "Greeting:Message" "Hello from a secret!"
   dotnet user-secrets list
   dotnet user-secrets remove "Greeting:Message"
   ```

3. **The launch profile**: the `environmentVariables` in
   `Properties/launchSettings.json`, where `__` in a name stands for `:`.

4. **The shell**, for one run. Prefix the setting with `ISOPLANE_APP_`:

   ```bash
   ISOPLANE_APP_Greeting__Message="Hello from the shell!" dotnet run
   ```

Restart the session after changing a setting. `Greeting:Message` is the setting
read by the `/greeting` route that `Program.cs` shows commented out.

## Common changes

- **Add an endpoint or a service.** `Program.cs` marks where each goes.
- **Serve static files.** Put them in a `wwwroot` folder and add
  `app.MapStaticAssets();` to `Program.cs`.
- **Try tighter limits.** A local session uses default resource limits.
  To see how the application behaves with less memory or time,
  add `isoplaneLimitsOverride` to the launch profile in `launchSettings.json`,
  beside `environmentVariables`:

  ```json
  "isoplaneLimitsOverride": { "memoryBytes": 134217728, "occupancyMsPerHttpDispatch": 1000 }
  ```
- **Add a NuGet package.** `dotnet add package <name>`.

For a larger starting point with a database and tests, see
`dotnet new isoplane-webapi`. For a service without ASP.NET, see
`dotnet new isoplane-handler`.
