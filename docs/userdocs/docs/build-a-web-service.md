---
title: Build a small lightweight web service
---

An Isoplane application does not have to use ASP.NET. A service can be a single
static method that receives every HTTP request. In this tutorial you will create
one from a template, run it, read the three methods Isoplane calls, and then add
a route and a test of your own.

Before starting, [install the tools](install.md). You also need `curl`; the
commands are written for bash or zsh. Allow about ten minutes.

## 1. Create the project

```bash
dotnet new isoplane-handler --name HelloService
cd HelloService
```

This is the [`isoplane-handler`](reference/project-templates.md#isoplane-handler) template. The
application is in `src/HelloService/`, and its tests are in
`tests/HelloService.Tests/`. The project's `README.md` describes every file.

## 2. Run it

```bash
isoplane dev --project src/HelloService
```

The first run downloads packages and prepares the runtime, so it takes longer
than later runs. When the service is ready, the command prints its address:

```text
  ready HelloService at http://127.0.0.1:5241 (local guest execution)
```

Your port will differ. Use yours wherever this tutorial shows `5241`. Leave the
command running and open a second terminal in the `HelloService` directory.

## 3. Call it

```bash
curl "http://127.0.0.1:5241/hello?name=Ada"
```

```json
{"message":"Hello, Ada!","name":"Ada"}
```

The first terminal shows the line the service logged while handling the request:

```text
  info   Greeting Ada
```

This route sends its response in pieces, as it produces them:

```bash
curl http://127.0.0.1:5241/countdown
```

```text
3
2
1
Liftoff!
```

## 4. Read the handler

Open `src/HelloService/Handler.cs`. Isoplane finds three methods in it by their
attributes.

- **`[HttpHandler]`** marks `HandleAsync`, which receives every request. It
  takes a `Request` and returns a `Task<Response>`. Its body is a `switch` on
  the method and path, with one line for each route.
- **`[Initializer]`** marks a method that runs during template preparation.
  Isoplane saves what it leaves in memory, and every instance of the service
  starts from that copy. Here it reads a setting and creates the `Greeter`.
- **`[Activated]`** marks a method that runs in each instance before its first
  request. Here it gives the instance an identifier, which would be the same in
  every instance if the initializer had created it.

Ask the service which instance answered:

```bash
curl http://127.0.0.1:5241/
```

```json
{"application":"HelloService","instance":"72cae793-01b3-4d57-81b9-e89c51abeb34"}
```

## 5. Add a route

You will add a `/tip` route that returns one of a few tips, chosen at random.

First define what the route returns. Open `src/HelloService/Greeter.cs` and add
a record below `ServiceInfo`:

```csharp
public sealed record Tip(string Text);
```

In the same file, the `HandlerJson` class lists the types the service sends and
receives as JSON. Add `Tip` to the list:

```csharp
[JsonSerializable(typeof(Tip))]
```

Now open `src/HelloService/Handler.cs`. Add a field below `instance` to hold the
tips:

```csharp
    private static Tip[] tips = [];
```

Fill it in the initializer. In `InitializeAsync`, add this above
`return Task.CompletedTask;`:

```csharp
        tips =
        [
            new("Await everything a request needs before it responds."),
            new("Keep what must last in a database, not in memory."),
            new("Do one-time work in the initializer."),
        ];
```

In `HandleAsync`, add the route below the `/countdown` line:

```csharp
        ("GET", "/tip") => Task.FromResult(Json(tips[Random.Shared.Next(tips.Length)], HandlerJson.Default.Tip)),
```

The line below the routes answers a known path called with the wrong method. Add
`/tip` to it:

```csharp
        (_, "/" or "/hello" or "/echo" or "/countdown" or "/tip") => Task.FromResult(Response.Text("Method not allowed", 405)),
```

Save both files. `isoplane dev` rebuilds the service, and the first terminal
prints:

```text
  reloaded HelloService at http://127.0.0.1:5241 (local guest execution)
```

Call the new route four times:

```bash
curl http://127.0.0.1:5241/tip
```

```json
{"text":"Keep what must last in a database, not in memory."}
{"text":"Keep what must last in a database, not in memory."}
{"text":"Await everything a request needs before it responds."}
{"text":"Do one-time work in the initializer."}
```

Yours will come out in a different order.

Two things happened that are particular to Isoplane. The array of tips was built
during initialization, and each instance starts with it already in memory. The
choice between them is still random in every instance, because Isoplane reseeds
`Random.Shared` each time an instance starts.

## 6. Test it

The template's tests in `tests/HelloService.Tests/HandlerTests.cs` run the
service in the sandbox and send it HTTP requests. Add one for the new route,
inside the `HandlerTests` class:

```csharp
    [Fact]
    public async Task A_tip_has_some_text()
    {
        var tip = await sandbox.Client.GetFromJsonAsync<Tip>("/tip");
        Assert.False(string.IsNullOrWhiteSpace(tip!.Text), sandbox.Diagnostics);
    }
```

The tests start the service themselves, and only one local session can run an
application at a time. Press Ctrl+C in the first terminal to stop
`isoplane dev`, then run:

```bash
dotnet test
```

```text
Passed!  - Failed:     0, Passed:    13, Skipped:     0, Total:    13, Duration: 3 s - HelloService.Tests.dll (net10.0)
```

## What you built

You ran a service with no web framework in the Isoplane sandbox, and added a
route to it with data prepared at initialization and a test that runs in the
sandbox.

Where to go next:

- [Handle requests without ASP.NET](handle-requests-without-aspnet.md) covers
  request bodies, streaming, settings and the rules for the three methods.
- [The application lifecycle](application-lifecycle.md) explains initialization
  and activation, and who is responsible for what at each stage.
- [How .NET differs on Isoplane](dotnet-runtime-differences.md) lists what to
  keep in mind as the service grows.
- [Build a small API with ASP.NET Core and EF Core](build-an-api.md) builds an
  application with a database.
