---
title: Supply local development settings
---

A local session takes its settings from the places an ASP.NET developer already
uses. Later sources override earlier ones:

1. `appsettings.json`, then `appsettings.{Environment}.json`, in the project.
2. The project's [.NET User Secrets](https://learn.microsoft.com/aspnet/core/security/app-secrets).
3. The `environmentVariables` of the launch profile in `Properties/launchSettings.json`.
4. Environment variables named `ISOPLANE_APP_<Key>` in the shell that starts the session.

ASP.NET applications read all of them through `IConfiguration`. Other
applications read the last three with `RuntimeConfiguration.ReadValues()`.

These sources apply to local sessions and sandbox tests. Only the `appsettings`
files travel with the application.

The application does not see your machine's other environment variables.
Restart the development session after changing a setting; the tools prepare the
application again for the changed settings.

## Keep credentials in User Secrets

Run these commands in the application directory. A project created from an
Isoplane template already has a `UserSecretsId`; `init` adds one to a project
that has none.

```bash
dotnet user-secrets init
dotnet user-secrets set "ConnectionStrings:Todo" "YOUR_DEVELOPMENT_CONNECTION_STRING"
```

Replace the placeholder with your own development connection string. User Secrets
keeps the value outside the project; it is a local development store, not an
encrypted production secret manager. Avoid putting credentials in source files or
committed configuration. Be mindful that a value typed as a shell argument can be
retained in shell history; `dotnet user-secrets set` also accepts JSON through
standard input.

Remove a local value when you no longer need it:

```bash
dotnet user-secrets remove "ConnectionStrings:Todo"
```

## Use the launch profile

`dotnet run` and `isoplane dev` read the first profile in
`Properties/launchSettings.json` whose `commandName` is `Project`. Select another
with `dotnet run --launch-profile NAME` or `isoplane dev --launch-profile NAME`,
or run without one with `--no-launch-profile`.

```json
{
  "profiles": {
    "http": {
      "commandName": "Project",
      "applicationUrl": "http://localhost:5080",
      "environmentVariables": {
        "ASPNETCORE_ENVIRONMENT": "Development",
        "Greeting__Default": "world"
      }
    }
  }
}
```

- `applicationUrl` sets the local port. The session serves plain HTTP on
  loopback and ignores an `https` address. `isoplane dev --port` overrides it.
- `environmentVariables` become settings. Two underscores in a name stand for a
  colon, so `Greeting__Default` is the setting `Greeting:Default`.
- `ASPNETCORE_ENVIRONMENT` sets the environment name of an ASP.NET application,
  and with it which `appsettings.{Environment}.json` is read. Without it the
  environment is `Production`.

## Override a setting for one run

Prefix a setting with `ISOPLANE_APP_`:

```bash
ISOPLANE_APP_ConnectionStrings__Todo="Host=..." dotnet run
```

Integration tests supply settings the same way through
[`ApplicationOptions.Settings`](test-an-application.md).

## Try the application under tighter limits

A local session runs with the largest limits the platform offers. To see how the
application behaves under smaller ones, add `isoplaneLimitsOverride` to a launch
profile:

```json
"isoplaneLimitsOverride": {
  "memoryBytes": 134217728,
  "occupancyMsPerHttpDispatch": 1000,
  "subrequestsPerDispatch": 20,
  "pendingFuturesPerDispatch": 6
}
```

Each key is optional. `memoryBytes` must be a multiple of 65536.

## Reach local services and private certificate authorities

A local session can connect to any address your machine can reach, including
`localhost`.

For TLS it trusts the certificate authorities your machine trusts, as any .NET
program does: the Windows certificate store, the macOS keychain, or the system
certificate directory on Linux. It also trusts the public authorities Isoplane
trusts when deployed. To trust a private authority for one run without
installing it, name its PEM file in `SSL_CERT_FILE`:

```bash
SSL_CERT_FILE=/path/to/private-ca.pem dotnet run
```

## Settings and the template

Settings are available while the application starts, and every instance has the
same ones. Read a setting wherever it suits the code: in `Program.cs`, in an
initialization hook or while handling a request. A value read during start-up is
saved in the template every instance starts from, which is why the tools prepare
the application again when a setting changes.
