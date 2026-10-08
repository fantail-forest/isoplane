---
title: Your first application
---

In this tutorial you will create an ASP.NET application, run it inside Isoplane's
sandbox, and change the response you see in your browser.

Before starting, [install the tools](install.md). You will also need a text
editor and a browser.

## Create the application

Change to a directory where you keep projects and create the application:

```bash
dotnet new isoplane-web --name MyApi
cd MyApi
```

You now have `Program.cs`, a project file, `appsettings.json` and
`Properties/launchSettings.json`, as in any ASP.NET project, and a `README.md`
that says what each file is for. The files are commented as a guide to setting
up a project of this kind. Open `Program.cs` in your editor. Find the route that returns `Hello from Isoplane!`. Leave it
unchanged for the first run.

Local state is private to the project directory.

## See the first response

Run the application:

```bash
dotnet run
```

Wait for the command to print a local address. The first build can take a while.
Open that address in your browser. You should see:

```text
Hello from Isoplane!
```

The request reached your ASP.NET route inside the sandbox. Keep the terminal
open while using the application.

## Change the response

Press Ctrl+C in the terminal to stop the application. In `Program.cs`,
replace `Hello from Isoplane!` with `Hello from my API!` and save the file.
Run the same command again:

```bash
dotnet run
```

Open the address printed for this run. You should now see:

```text
Hello from my API!
```

You have built and run an application, then rebuilt it with your own response.
Press Ctrl+C to finish the session.

## Continue

Use [run and watch](run-and-rebuild.md) to rebuild automatically after edits,
[debug it](run-and-rebuild.md#debugging) from your editor, or
[add an integration test](test-an-application.md) for the response you just
created. [Application packages](execution-modes.md) explains how the SDK selects the
shared runtime.

`isoplane-web` is the smallest starting point. Two larger templates show more of
what an application can do, each with tests and a README:

| Template | What it creates |
| --- | --- |
| `dotnet new isoplane-webapi` | A Web API with Entity Framework Core, PostgreSQL, OpenAPI and an outbound webhook |
| `dotnet new isoplane-handler` | A service that [handles requests in one static method](handle-requests-without-aspnet.md), without ASP.NET |
