---
title: Your first application
---

In this tutorial you will create an ASP.NET application, run it inside Isoplane's
sandbox, and change the response you see in your browser.

Before starting, [install the tools and template](install.md). Keep that terminal
open: `preview_release` identifies the release you installed. You will also need
a text editor and a browser.

## Create the application

Change to a directory where you keep projects, outside the extracted release.
Create the application and add its local package source:

```bash
dotnet new isoplane-web --name MyApi
cd MyApi
dotnet nuget add source "$preview_release/packages" \
  --name isoplane-preview --configfile nuget.config
```

You now have `Application.cs`, a project file, and `wisp.toml`. Open
`Application.cs` in your editor. Find the route that returns
`Hello from Isoplane!`. Leave it unchanged for the first run.

Local state is private to the project directory.

## See the first response

Run the application:

```bash
dotnet run -c WispInterpretedDebug
```

Wait for the command to print a local address. The first build can take a while.
Open that address in your browser. You should see:

```text
Hello from Isoplane!
```

The request reached your ASP.NET route inside the sandbox. Keep the terminal
open while using the application.

## Change the response

Press Ctrl+C in the terminal to stop the application. In `Application.cs`,
replace `Hello from Isoplane!` with `Hello from my API!` and save the file.
Run the same command again:

```bash
dotnet run -c WispInterpretedDebug
```

Open the address printed for this run. You should now see:

```text
Hello from my API!
```

You have built and run an application, then rebuilt it with your own response.
Press Ctrl+C to finish the session.

## Continue

Use [run and watch](run-and-rebuild.md) to restart automatically after edits, or
[add an integration test](test-an-application.md) for the response you just
created. [Execution modes](execution-modes.md) explains how this configuration
relates to NativeAOT.
