# Isoplane

Isoplane builds and runs your .NET applications inside WebAssembly sandboxes.
This repository contains the SDK and tools for local development, testing and
debugging, plus a single-machine host for deploying your own applications.
The host is Rust; the guest SDK is C#.

[`AGENTS.md`](AGENTS.md) is the engineering handbook: architecture, tooling,
the rules code is held to, and the four invariants every host mechanism
derives from. [Customer guides](docs/userdocs/) cover local development, testing,
debugging and [self-hosted deployment](docs/userdocs/docs/how-to/deploy-an-application.md).
One host serves multiple applications by their configured Host names.

## Using Isoplane

The [Quickstart](https://isoplane.dev/docs/quickstart) walks through installing
the `Isoplane.Cli` .NET tool, setting up the SDK and running your first application.
The [documentation](https://isoplane.dev/docs/) includes tutorials, how-to guides
and API references. SDK packages are published as `Isoplane.*` on NuGet.

## Support and feedback

- [Report a bug](https://github.com/fantail-forest/isoplane/issues/new?template=bug_report.yml)
  in the SDK, CLI, templates or self-hosted daemon. Include your version, operating
  system and reproduction steps.
- [Request a feature](https://github.com/fantail-forest/isoplane/issues/new?template=feature_request.yml).
- [Ask a question or share what you built](https://github.com/fantail-forest/isoplane/discussions).
- [Report a security issue privately](SECURITY.md).

## Building and testing

Task prepares build dependencies and test fixtures; Cargo and MSBuild compile them.
Install Rust, Python 3 and your platform's C compiler first, then run:

```sh
scripts/setup
task --list
task build
task test -- -- --skip a_prepared_release_installs_builds_applications_and_uninstalls
```

Setup installs the pinned .NET SDK and guest tools. Platform runtime builds run
on Linux; customer applications compile to IL packages for that shared runtime.
The optional repository toolchain image is available through
`scripts/dev --container --build`.

Task prepares guest modules before starting tests. Fixture helpers load those
artifacts and fail with the preparation command if an input is missing. The
installation journey additionally requires an explicitly prepared release.

## Continuous integration

[CI](.github/workflows/ci.yml) checks pushes to `main` and pull-request updates.
[Path filters](.github/changes.yml) select affected build components, including
shared inputs and generated-reference dependencies. Rust checks include library
tests, ABI drift, hostile guests and sandbox enforcement. Native SDK support is
built and checked on Linux, Windows x64 and macOS ARM64. Managed SDK tests, docs
builds and extension unit tests/packaging run in separate jobs when affected.
The workflow aggregates its results in **CI passed**.

[Nightly](.github/workflows/nightly.yml) runs at 03:23 UTC. It compares inputs
against the last successful nightly run, retrying failed changes and skipping
unchanged inputs. Manual dispatch forces a run. Linux jobs cover host/runtime
acceptance, managed and installed-tool tests, debugging, VS Code integration,
PostgreSQL and release installation. They assemble unsigned SDK, VSIX and node
artifacts without publishing. Windows/macOS installed-tool journeys are not part
of this initial nightly configuration.

The compiler feed and portable runtime modules have exact-input caches; native
preparations remain local to each job. Jobs exchange compressed artifacts instead
of sharing mounts or invoking remote machines. Rust dependency caches are separate
per platform. Cache eviction causes rebuilding, never a skipped check. The pinned
source forks must be publicly readable for anonymous CI checkouts.

The workflows use standard hosted runners. They bound compilation parallelism,
omit Rust test debug information and report disk consumption; fitting a cold full
build into GitHub's available disk still requires a hosted run. `act` can exercise
Linux jobs locally, but does not emulate macOS/Windows, GitHub permissions, job
timeouts or the hosted disk allowance. Permission-sensitive tests also need a
container without root permission-bypass capabilities. The heavy workflows need
working Docker access for their integration tests. To check workflow syntax locally without fetching the source forks:

```sh
act workflow_dispatch -W .github/workflows/ci.yml -j tooling \
  -P ubuntu-24.04=catthehacker/ubuntu:act-24.04 --rm
```

## Layout

The host crates live under `crates/`; each manifest carries a `description`,
and that is the crate inventory:

```sh
cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | "\(.name): \(.description)"'
```

| Path                  | What it is                                                                |
| --------------------- | ------------------------------------------------------------------------- |
| `crates/`              | The host: the contract, engine, local snapshots, proxy, execution pool, preparation, `isoplaned` and the native Isoplane support library. |
| `guest-sdks/dotnet`    | The C# guest SDK. |
| `guest-sdks/rust`      | Internal Rust helpers for guest test fixtures. |
| `guests/`             | The guest modules: fixtures the host tests drive.                         |
| `docs/userdocs` | Customer guides for Isoplane. |
| `docs/opinion-registry` | How software is built here.                                             |
| `scripts/`            | `scripts/dev` runs a local command; `scripts/clean` reclaims build intermediates. |

## License

First-party code is available under the [Elastic License 2.0](LICENSE.txt).
Third-party dependencies and forks retain their original licenses and notices.
