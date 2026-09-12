---
title: Create an application
---

First [install the tools and template](install.md). Start in a new directory
outside the release directory. In the same terminal, `preview_release` still
points to the release you installed.

```bash
dotnet new isoplane-web --name MyApi --tenant local-development
cd MyApi
dotnet nuget add source "$preview_release/packages" \
  --name isoplane-preview --configfile nuget.config
```

The tenant name identifies your local development state. It is not a login or
a credential.

The template creates `Application.cs`, a project file, and `wisp.toml` for local
execution. `Application.Create` registers services and routes and returns a
built ASP.NET application. The supplied route responds with
`Hello from Isoplane!`.

## Run the application

```bash
dotnet run -c WispInterpretedDebug
```

Open the local address printed by the command. The request runs your application
inside Isoplane's sandbox. Press Ctrl+C to stop it.

Change the response text in `Application.cs`, run again, and refresh the page.
Continue with [build configurations and edit/restart](run-and-rebuild.md), or
[add an integration test](test-an-application.md).
