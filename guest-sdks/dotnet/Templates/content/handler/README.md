# SampleHandler

A service that runs in the Isoplane sandbox without ASP.NET. One static method
handles every HTTP request.

## Run it

```bash
dotnet run --project src/SampleHandler
```

Then try `/hello?name=Ada`, `/countdown`, or post a JSON greeting to `/echo`.
`isoplane dev --project src/SampleHandler` rebuilds on every save.

## Test it

```bash
dotnet test
```

- `GreeterTests` are ordinary unit tests. Application logic runs on your machine.
- `HandlerTests` start the application in the real Isoplane sandbox, so they need
  the `isoplane` tool. Run only the unit tests with
  `dotnet test --filter "Category!=Sandbox"`.

## How it works

`src/SampleHandler/Handler.cs` marks three static methods:

- **`[HttpHandler]`** receives every HTTP request and returns its response. An
  application has exactly one; the build fails otherwise. Take a
  `StreamingRequest` in place of the `Request` to read a large body as it arrives.
- **`[Initializer]`** runs while Isoplane prepares the template that every
  instance starts from. Whatever it leaves in memory is copied into each instance.
- **`[Activated]`** runs in each instance, before its first request. State that
  must differ between instances is set there.

Both lifecycle methods are optional.

A request owns its work: nothing runs in an instance after its response, and
memory an instance keeps between requests may be discarded at any time. Keep
state that must last in a database or another service.

## Configuration

`RuntimeConfiguration.ReadValues()` returns the settings the platform supplies. In
local development those are the project's [User Secrets](https://learn.microsoft.com/aspnet/core/security/app-secrets)
and the environment variables of the launch profile in
`src/SampleHandler/Properties/launchSettings.json`, where `__` in a name stands
for `:`.
