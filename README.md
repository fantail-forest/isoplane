# Isoplane

Isoplane hosts customer-written .NET applications as WebAssembly guests. The
isolation boundary is the WASM sandbox, not the CLR and not a hypervisor.
Guests are pre-initialised at deploy time into a memory template, activate
copy-on-write from it, park with an empty stack while waiting on host I/O, and
can be hibernated as a delta against the template and woken later. The host is
Rust; the guest SDK is C#.

[`AGENTS.md`](AGENTS.md) is the engineering handbook: architecture, tooling,
the rules code is held to, and the four invariants every host mechanism
derives from. [Customer guides](docs/userdocs/) cover local development, testing,
debugging and [self-hosted deployment](docs/userdocs/docs/how-to/deploy-an-application.md).
One host serves multiple applications by their configured Host names.

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
