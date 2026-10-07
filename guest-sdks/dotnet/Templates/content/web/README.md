# Sample-App

An ASP.NET Core application that runs in the Isoplane sandbox.

```bash
dotnet run
```

The application listens on the address in `Properties/launchSettings.json`.
Edit `Program.cs` and run it again, or use `isoplane dev` to rebuild on every save.

## How it differs from an ordinary ASP.NET application

- `IsoplaneHosting.CreateBuilder(args)` replaces `WebApplication.CreateBuilder(args)`.
  Isoplane delivers requests to the application, so it opens no listener.
- `Program.cs` ends with `await app.RunAsync()`. The synchronous `app.Run()`
  would block, which a sandboxed application cannot do; the build rejects it.
- `Initialization.Initializing` callbacks run during preparation, before Isoplane captures the
  template that every instance starts from. Whatever they leave in memory is
  already there when an instance serves its first request.

Configuration works as in ASP.NET: `appsettings.json`, then
`appsettings.{Environment}.json`, then [User Secrets](https://learn.microsoft.com/aspnet/core/security/app-secrets)
and the environment variables in the launch profile.

For a larger starting point with a database and tests, see
`dotnet new isoplane-webapi`. For a service without ASP.NET, see
`dotnet new isoplane-handler`.
