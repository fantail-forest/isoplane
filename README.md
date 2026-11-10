# Isoplane

Isoplane builds and runs your .NET applications inside WebAssembly sandboxes.
This repository contains the SDK and tools for local development, testing and
debugging, plus the Isoplane engine, which runs your applications on a single
machine. The engine is Rust; the SDK is C#.

[`AGENTS.md`](AGENTS.md) is the engineering handbook: architecture, tooling,
the rules code is held to, and the four invariants every engine mechanism
derives from. [Customer guides](docs/userdocs/) cover local development, testing,
debugging and [self-hosted deployment](docs/userdocs/docs/how-to/deploy-an-application.md).
One engine serves multiple applications by their configured Host names.

## Design target

Isoplane is designed for extremely dense, oversubscribed deployments, with
response latencies as good as, if not better than, all other current .NET
hosting options that offer scale-to-zero. The initial engineering target is
1,000 instances running typical .NET API workloads on a single 16-core host
machine with 64 GiB of system memory.

The target explains the decisions that set Isoplane apart from other ways of
running .NET:

- **Every instance starts from a memory template.** The engine runs an
  application's startup code once, saves the application's memory, and starts
  each instance as a copy-on-write clone of that memory. A new instance skips
  startup work, and instances share the parts of the template they never
  modify.
- **Instances share a small pool of threads.** An instance occupies a thread
  only while its code is running. When the code awaits, the thread is free to
  run another instance, and the engine performs the I/O on a separate pool.
- **Nothing blocks.** A synchronous wait would occupy a thread without doing
  any work, so the .NET runtime throws instead of blocking, and applications
  await their I/O.

[How .NET differs on Isoplane](docs/userdocs/docs/concepts/dotnet-runtime-differences.md)
describes what these decisions mean for application code.

Please understand that we use this target as a north star to inform our
engineering decisions, and that it's not intended as a performance guarantee.
Isoplane's performance in your environment will depend heavily on your hardware
and workload.

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

The OpenSSL fork publishes WASI libraries in immutable releases named for their
source revision. The .NET runtime fork consumes its pinned OpenSSL release and
publishes a portable compiler feed. Isoplane downloads the feed for its committed
runtime submodule revision and checks its OpenSSL, ICU and WASI SDK identities.
Publish each dependency's release before updating the consuming repository's pin;
a missing release fails the consuming build with its expected download URL.
These public downloads require no credentials, including for contributor PRs.

Portable runtime modules include Isoplane SDK sources and have an exact-input
cache. Jobs exchange compressed artifacts instead of sharing mounts or invoking
remote machines. Wasmtime builds through Cargo with the consuming project's
features and toolchain; Rust dependency caches are separate per platform.
Native preparations remain local to each job.

Local builds can download the same compiler feed with `task runtime:fetch`, then
select it with `ISOPLANE_TEST_RUNTIME_FEED="$PWD/.build/wasm32/runtime/feed" task build`.
`task runtime:compiler` builds from the checked-out fork sources on Linux using
the same packaging recipe as the runtime fork's CI.

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

The engine's crates live under `crates/`; each manifest carries a `description`,
and that is the crate inventory:

```sh
cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[] | "\(.name): \(.description)"'
```

| Path                  | What it is                                                                |
| --------------------- | ------------------------------------------------------------------------- |
| `crates/`              | The Isoplane engine: the contract, wasmtime embedding, local snapshots, proxy, execution pool, preparation, `isoplaned` and the native Isoplane support library. |
| `guest-sdks/dotnet`    | The C# guest SDK. |
| `guest-sdks/rust`      | Internal Rust helpers for guest test fixtures. |
| `guests/`             | The guest modules: fixtures the engine's tests drive.                     |
| `docs/userdocs` | Customer guides for Isoplane. |
| `docs/opinion-registry` | How software is built here.                                             |
| `scripts/`            | `scripts/dev` runs a local command; `scripts/clean` reclaims build intermediates. |

## License

First-party code is available under the [Elastic License 2.0](LICENSE.txt).
Third-party dependencies and forks retain their original licenses and notices.
