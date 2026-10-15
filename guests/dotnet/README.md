# Mono integration fixtures

These focused fixtures exercise SDK, runtime, PAL and host behavior. They run
as managed packages on the shared Mono runtime. Task prepares them before tests run. They are not customer application templates.

```bash
scripts/dev guests/build-dotnet.sh --if-stale
```

Customer starting points are the project templates in
`guest-sdks/dotnet/Templates`. Runtime and package fixtures are under
`guest-runtime/tests/guest` and `guest-runtime/tests/packages`.
