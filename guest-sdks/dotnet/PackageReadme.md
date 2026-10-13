# Isoplane SDK

Isoplane runs .NET applications in WebAssembly sandboxes. This package provides
its guest APIs, shared runtime modules and MSBuild integration, and analyzers
that check as you type what the platform requires of an application.

For ASP.NET applications, use the
[Isoplane.AspNetCore](https://www.nuget.org/packages/Isoplane.AspNetCore) project
SDK. [Isoplane.Templates](https://www.nuget.org/packages/Isoplane.Templates)
provides `dotnet new` templates, such as `isoplane-web` and `isoplane-webapi`.
The [Isoplane.Cli](https://www.nuget.org/packages/Isoplane.Cli) tool runs
applications locally, and its `isoplane setup` command checks prerequisites and
installs the templates.

Keep the CLI, SDK and template package versions aligned. Building and running an
application needs the .NET SDK and that tool; it needs no workload, native
compiler or Docker.

Learn more at [isoplane.dev](https://isoplane.dev/).
