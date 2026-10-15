# Mono integration fixtures

These focused fixtures exercise SDK, runtime, PAL and host behavior. They run
as managed packages on the shared Mono runtime. Task prepares them before tests run. They are not customer application templates.

Each folder under `src/` except `Shared/` is one fixture app. `NativeIo`, `Bcl`
and `Node` pack several probes and route a request by its first path segment,
so `/dns/example.test` reaches the DNS probe with the path `/example.test`.

```bash
scripts/dev guests/build-dotnet.sh --if-stale
```

Customer starting points are the project templates in
`guest-sdks/dotnet/Templates`. Runtime and package fixtures are under
`guest-runtime/tests/guest` and `guest-runtime/tests/packages`.
