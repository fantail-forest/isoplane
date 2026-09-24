# Isoplane SDK

Isoplane runs .NET applications in WebAssembly sandboxes. This package provides
its guest APIs, shared runtime modules and MSBuild integration.

For ASP.NET applications, use the
[Isoplane.AspNetCore](https://www.nuget.org/packages/Isoplane.AspNetCore) project
SDK and the `isoplane-web` template from
[Isoplane.Templates](https://www.nuget.org/packages/Isoplane.Templates).
The [Isoplane.Cli](https://www.nuget.org/packages/Isoplane.Cli) tool's
`isoplane setup` command installs the matching native build toolchain.

Keep the CLI, SDK and template package versions aligned. Installed customer
builds use the supplied native tools and do not require Docker.

Learn more at [isoplane.dev](https://isoplane.dev/).
