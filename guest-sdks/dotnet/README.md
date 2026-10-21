# C# guest SDK

Customer projects use the Isoplane SDK to compile application IL. `dotnet run`
and `isoplane bundle` package assemblies, dependencies, assets and the SDK's
runtime identity. The application runs on the shared Mono WASM runtime supplied
with that SDK release. See the [application tutorial](../../docs/userdocs/docs/build-an-api.md).

`MonoRuntime/` builds the platform runtime. `Isoplane.Guest.props`,
`Isoplane.Guest.targets` and `build-guest.sh` serve platform builds and focused
runtime fixtures; customer projects do not import them. `native/` contains the
platform entry layer and PAL. `Abi/` binds the host contract, `Runtime/` owns
continuations, and `Sdk/` supplies managed APIs.

The [ASP.NET adapter](AspNetCore/README.md) constructs application routes and
services during preparation without opening a guest listener. Host-owned I/O
wakes the application after it returns with an empty stack.

## Randomness after activation

An activation inherits captured guest memory. The BCL's
`RandomNumberGenerator` requests host entropy when generating bytes. Values
already stored in guest memory remain captured. A generator that seeds itself
once and then advances private state (`new Random(seed)` or a library DRBG)
can therefore repeat across copies if initialization created it before capture.
Create that generator after activation, or recreate it on
`Isoplane.Activation.Activated`, raised on the first dispatch of each activation:

```csharp
Isoplane.Activation.Activated += () =>
{
    _random = new Random(RandomNumberGenerator.GetInt32(int.MaxValue));
    return Task.CompletedTask;
};
```

## Validation

Task prepares missing or stale guest fixtures from the configured runtime feed
before running tests. Fixture helpers only resolve and load prepared artifacts. SDK drift tests compare the managed ABI with the Rust contract. Runtime,
engine and node tests exercise real Mono guests and packages through their
consuming interfaces.
