# Mono integration fixtures

These focused fixtures exercise SDK, runtime, PAL and host behavior. They run
on the configured Mono runtime pack and are built through the shared fixture
helper when missing or stale. They are not customer application templates.

```bash
scripts/dev guests/build-dotnet.sh --if-stale
```

Customer examples live in `samples/` and compile to packages for the SDK's
shared runtime. Runtime and package fixtures are under
`guest-runtime/tests/guest` and `guest-runtime/tests/packages`.
