---
title: Isoplane documentation
slug: /
---

Build and run a .NET application inside Isoplane's sandbox using installed tools
and ordinary .NET commands.

This initial release is the SDK for local development and testing. The hosted
Isoplane service is not available yet; no account or sign-in is needed to use
the SDK.

## Tutorials

New to Isoplane? The [Quickstart](quickstart.md) installs the tools and gets an
application running in four commands. Then build something real:

- [Build a small lightweight web service](tutorials/build-a-web-service.md): a service
  with no web framework, to which you add a route and a test.
- [Build a small API with ASP.NET Core and EF Core](tutorials/build-an-api.md): a todo
  API on PostgreSQL, to which you add a field, a migration, an endpoint and a
  test.

## How-to guides

Use these when you have a particular task to complete:

- [Install, update or repair the tools](how-to/install.md).
- [Bring an existing ASP.NET application](how-to/onboard-an-application.md).
- [Handle requests without ASP.NET](how-to/handle-requests-without-aspnet.md).
- [Build, run and watch for changes](how-to/run-and-rebuild.md).
- [Supply local development settings](how-to/local-settings.md).
- [Connect to PostgreSQL](how-to/connect-postgresql.md).
- [Add a sandbox integration test](how-to/test-an-application.md).
- [Troubleshoot local development](how-to/troubleshooting.md).

## Reference

Choose a starting point from the [Project templates](reference/project-templates.md):
an empty ASP.NET Core application, a Web API, a Razor Pages web application, a
Model Context Protocol server, a service without a web framework, and sandbox
tests for an application you already have.
Look up public types, methods and properties in the [SDK API reference](reference/sdk.md),
and what each `ISOPLANE` diagnostic means in [Error codes](reference/error-codes.md).
The settings Isoplane reads are defined in [Project settings](reference/project-settings.md),
[launchSettings.json](reference/launch-settings.md) and
[launch.json for VS Code](reference/vscode-launch.md).

## Concepts

Read [how .NET differs on Isoplane](concepts/dotnet-runtime-differences.md) for everything
that behaves differently from .NET in a server process, and
[execution limits](concepts/execution-limits.md) for what the platform bounds. Understand
[the application lifecycle](concepts/application-lifecycle.md) before choosing where to
keep state or when to perform work.
