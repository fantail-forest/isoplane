---
title: Quickstart
---

Four commands take you from a .NET SDK to an ASP.NET application running in
Isoplane's sandbox on your own machine.

You need the .NET 10 SDK on Linux x64, Windows x64 or macOS ARM64, and an
internet connection. [Install the SDK](install.md#prerequisite-a-net-sdk) lists
what each platform needs.

## 1. Install the CLI

```bash
dotnet tool install --global Isoplane.Cli --version 0.1.0-preview.5
```

This adds the `isoplane` command from the current preview release,
`0.1.0-preview.5`. If your shell cannot find it afterwards, the
.NET tools directory is missing from your `PATH`: see
[Install the CLI and the SDK](install.md#install-the-cli-and-the-sdk).

## 2. Set up your machine

```bash
isoplane setup
```

Setup checks that your machine has what Isoplane needs and installs the project
templates. If something is missing it names it and stops. It ends by reporting
that Isoplane `0.1.0-preview.5` is ready.

## 3. Create an application

```bash
dotnet new isoplane-web --name MyApi
cd MyApi
```

`MyApi` is an ordinary ASP.NET project: a `Program.cs`, a project file and
settings files, with one route at `/`.

## 4. Run it

```bash
dotnet run
```

The first run downloads the Isoplane SDK packages, builds the application and
compiles the runtime it runs on, so it takes longer than later runs. When the
application is ready, the command prints its address:

```text
  ready MyApi at http://127.0.0.1:5129 (local guest execution)
```

Your port will differ. `dotnet new` picks one when it creates the project and
records it in `Properties/launchSettings.json`.

Open the address in a browser:

```text
Hello from Isoplane!
```

That response came from your ASP.NET route, running inside the sandbox. Press
Ctrl+C in the terminal to stop the application.

## Next steps

The tutorials cover the same ground more slowly and go further:

- [Your first application](create-an-application.md) walks through this project,
  changes its response, and introduces the larger templates.

For a particular task, such as rebuilding on every save, debugging, local
settings or testing, see the how-to guides on the
[documentation home page](index.md#how-to-guides). If a step here failed, start
with [Troubleshoot local development](troubleshooting.md).
