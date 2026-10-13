---
title: Isoplane documentation
slug: /
---

Build and run a .NET application inside Isoplane's sandbox using installed tools
and ordinary .NET commands.

## Tutorials

New to Isoplane? The [Quickstart](quickstart.md) installs the tools and gets an
application running in four commands. [Your first application](create-an-application.md)
then takes it more slowly: create a small API, run it, and change its response.

## How-to guides

Use these when you have a particular task to complete:

- [Install, update or repair the tools](install.md).
- [Bring an existing ASP.NET application](onboard-an-application.md).
- [Handle requests without ASP.NET](handle-requests-without-aspnet.md).
- [Build, run and watch for changes](run-and-rebuild.md).
- [Supply local development settings](local-settings.md).
- [Connect to PostgreSQL](connect-postgresql.md).
- [Add a sandbox integration test](test-an-application.md).
- [Troubleshoot local development](troubleshooting.md).

## Reference

Look up public types, methods and properties in the [SDK API reference](reference/sdk.md).

## Explanation

Read [how .NET differs on Isoplane](dotnet-runtime-differences.md) for everything
that behaves differently from .NET in a server process, and
[execution limits](execution-limits.md) for what the platform bounds. Understand
[application packages and the shared runtime](execution-modes.md) and
[the application lifecycle](application-lifecycle.md) before choosing where to
keep state or when to perform work.
