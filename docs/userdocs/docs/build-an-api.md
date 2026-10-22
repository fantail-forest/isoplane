---
title: Build a small API with ASP.NET Core and EF Core
---

import {OnlyOn, PlatformToggle} from '@site/src/components/Platform';

<PlatformToggle />

In this tutorial you will create a todo API from an Isoplane template, run it in
the sandbox against PostgreSQL, and see where its state lives. Then you will add
a feature of your own, a due date, with a database migration, a new endpoint and
a test.

For a new project with a few simple tables, consider the lighter option first:
[Connect to PostgreSQL](connect-postgresql.md) builds a service without ASP.NET
that uses the Npgsql driver directly, so there is less code to load into every
instance.

Before starting, [install the tools](install.md). You also need Docker for the database. On macOS and Linux use `curl`;
on Windows use PowerShell. Choose your operating system above for shell commands. Allow
about twenty minutes.

## 1. Create the project

```bash
dotnet new isoplane-webapi --name TodoApi
cd TodoApi
```

The [`isoplane-webapi`](reference/project-templates.md#isoplane-webapi) template is a complete
application. These are the parts this tutorial
touches; the project's `README.md` describes the rest.

| Path | What it holds |
| --- | --- |
| `compose.yaml` | The development database |
| `src/TodoApi/Program.cs` | Services, endpoints, and the initialization and activation hooks |
| `src/TodoApi/Todos/` | The todo entity, its `DbContext` and its endpoints |
| `src/TodoApi/Migrations/` | Entity Framework Core migrations |
| `tests/TodoApi.Tests/` | Unit tests, and tests that run the application in the sandbox |

## 2. Start the database and the application

Start PostgreSQL:

```bash
docker compose up -d --wait
```

The database listens on port 5432. If something on your machine already uses
that port, change it in `compose.yaml` and in
`src/TodoApi/appsettings.Development.json` before running the command.

Now start the application:

```bash
isoplane dev --project src/TodoApi
```

The first run downloads packages and prepares the runtime, so it takes longer
than later runs. When the application is ready, the command prints its address:

```text
  ready TodoApi at http://127.0.0.1:5019 (local guest execution)
```

Your port will differ. Use yours wherever this tutorial shows `5019`. Leave the
command running and open a second terminal in the `TodoApi` directory for the
steps that follow.

## 3. Create and list todos

Create a todo:

<OnlyOn platforms="mac linux">

```bash
curl -X POST http://127.0.0.1:5019/todos -H 'Content-Type: application/json' -d '{"title":"Read the README"}'
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
(Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:5019/todos' -Method Post -ContentType 'application/json' -Body '{"title":"Read the README"}').Content
```

</OnlyOn>

```json
{"id":1,"title":"Read the README","isComplete":false,"createdAt":"2026-10-13T03:56:24.901+00:00","completedAt":null}
```

List the todos:

<OnlyOn platforms="mac linux">

```bash
curl http://127.0.0.1:5019/todos
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
(Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:5019/todos').Content
```

</OnlyOn>

```json
[{"id":1,"title":"Read the README","isComplete":false,"createdAt":"2026-10-13T03:56:24.901+00:00","completedAt":null}]
```

The request reached an ASP.NET endpoint running inside the sandbox, and the
endpoint stored the todo in PostgreSQL through Entity Framework Core.

## 4. See where state lives

The root endpoint reports which instance of the application answered:

<OnlyOn platforms="mac linux">

```bash
curl http://127.0.0.1:5019/
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
(Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:5019/').Content
```

</OnlyOn>

```json
{"application":"TodoApi","environment":"Development","instance":"c442ce0e-99e8-43cd-96b5-7385074fc783"}
```

Run it again and the `instance` value is the same: one instance is handling
your requests, and it keeps that identifier in memory.

Now replace the instance. `isoplane dev` rebuilds the application whenever you
save a source file, so open `src/TodoApi/Program.cs`, add a comment on a new line
at the end, and save. The first terminal prints:

```text
  reloaded TodoApi at http://127.0.0.1:5019 (local guest execution)
```

Ask both questions again:

<OnlyOn platforms="mac linux">

```bash
curl http://127.0.0.1:5019/
curl http://127.0.0.1:5019/todos
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
(Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:5019/').Content
(Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:5019/todos').Content
```

</OnlyOn>

```json
{"application":"TodoApi","environment":"Development","instance":"1534b7ee-f5da-44a0-998f-75ee1001b401"}
[{"id":1,"title":"Read the README","isComplete":false,"createdAt":"2026-10-13T03:56:24.901+00:00","completedAt":null}]
```

The instance identifier changed, because a new instance replaced the old one and
the old one's memory went with it. The todo is still there, because it was never
in the instance: it is in the database.

A rebuild is only one reason an instance is replaced. Isoplane may discard an
instance after any request, so the rule holds everywhere: memory is a cache, and
anything that must last goes in a database.
[The application lifecycle](application-lifecycle.md) explains the stages you
just saw.

Remove the comment you added.

## 5. Add a due date

You will now give a todo an optional due date and add an endpoint that lists the
overdue ones.

Open `src/TodoApi/Todos/Todo.cs`. In the `Todo` class, add a property below
`CompletedAt`:

```csharp
    public DateOnly? DueOn { get; set; }
```

In the same file, let a new todo carry a due date, and return it in responses.
Replace the `CreateTodo` and `TodoResponse` records with:

```csharp
public sealed record CreateTodo([Required, StringLength(200, MinimumLength = 1)] string Title, DateOnly? DueOn = null);
```

```csharp
public sealed record TodoResponse(int Id, string Title, bool IsComplete, DateTimeOffset CreatedAt, DateTimeOffset? CompletedAt, DateOnly? DueOn)
{
    public static TodoResponse From(Todo todo) => new(todo.Id, todo.Title, todo.IsComplete, todo.CreatedAt, todo.CompletedAt, todo.DueOn);
}
```

Open `src/TodoApi/Todos/TodoEndpoints.cs`. In `Create`, store the due date:

```csharp
        var todo = new Todo { Title = request.Title, CreatedAt = clock.GetUtcNow(), DueOn = request.DueOn };
```

In `MapTodos`, map a new route below the `/export` line:

```csharp
        todos.MapGet("/overdue", Overdue);
```

And add its handler to the class:

```csharp
    private static async Task<Ok<List<TodoResponse>>> Overdue(TodoDbContext db, TimeProvider clock, CancellationToken cancellation)
    {
        var today = DateOnly.FromDateTime(clock.GetUtcNow().UtcDateTime);
        return TypedResults.Ok(await db.Todos.AsNoTracking()
            .Where(todo => !todo.IsComplete && todo.DueOn < today)
            .OrderBy(todo => todo.DueOn)
            .Select(todo => TodoResponse.From(todo))
            .ToListAsync(cancellation));
    }
```

Save both files and look at the first terminal. The reload failed:

```text
isoplane: reload failed; keeping the last working app: local preparation failed: preparation rejected: initialization failed: initializer ended with Trapped: guest returned trapped(Panic)
  guest log: pre-initialization failed for Isoplane.MonoRuntime.PackageRouter: unhandled System.InvalidOperationException: An error was generated for warning 'Microsoft.EntityFrameworkCore.Migrations.PendingModelChangesWarning': The model for context 'TodoDbContext' has pending changes. Add a new migration before updating the database. ...
```

This is initialization at work. During template preparation, Isoplane runs
`Program.cs` and its `Initialization.Initializing` callback. In this application the
callback applies database migrations, and Entity Framework refused: the `Todo`
class now has a column the migrations do not. Because initialization failed,
Isoplane did not start the new version, and the previous one is still serving
requests.

## 6. Add the migration

In the second terminal:

```bash
dotnet tool restore
dotnet ef migrations add AddDueDate --project src/TodoApi
```

```text
Build started...
Build succeeded.
Done. To undo this action, use 'ef migrations remove'
```

The new files under `src/TodoApi/Migrations/` are source files, so the first
terminal rebuilds, and this time it reports:

```text
  reloaded TodoApi at http://127.0.0.1:5019 (local guest execution)
```

Initialization ran again, applied the migration, and the new version started.

Create one todo that is overdue and one that is not:

<OnlyOn platforms="mac linux">

```bash
curl -X POST http://127.0.0.1:5019/todos -H 'Content-Type: application/json' -d '{"title":"Renew the passport","dueOn":"2020-01-01"}'
curl -X POST http://127.0.0.1:5019/todos -H 'Content-Type: application/json' -d '{"title":"Plan the holiday","dueOn":"2099-01-01"}'
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
(Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:5019/todos' -Method Post -ContentType 'application/json' -Body '{"title":"Renew the passport","dueOn":"2020-01-01"}').Content
(Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:5019/todos' -Method Post -ContentType 'application/json' -Body '{"title":"Plan the holiday","dueOn":"2099-01-01"}').Content
```

</OnlyOn>

```json
{"id":2,"title":"Renew the passport","isComplete":false,"createdAt":"2026-10-13T04:00:55.814+00:00","completedAt":null,"dueOn":"2020-01-01"}
{"id":3,"title":"Plan the holiday","isComplete":false,"createdAt":"2026-10-13T04:00:55.858+00:00","completedAt":null,"dueOn":"2099-01-01"}
```

Then list the overdue ones:

<OnlyOn platforms="mac linux">

```bash
curl http://127.0.0.1:5019/todos/overdue
```

</OnlyOn>

<OnlyOn platforms="win">

```powershell
(Invoke-WebRequest -UseBasicParsing -Uri 'http://127.0.0.1:5019/todos/overdue').Content
```

</OnlyOn>

```json
[{"id":2,"title":"Renew the passport","isComplete":false,"createdAt":"2026-10-13T04:00:55.814+00:00","completedAt":null,"dueOn":"2020-01-01"}]
```

## 7. Warm the new route

The first request to a route does work that later requests do not: ASP.NET
builds the handler, and Entity Framework compiles the query. Initialization can
capture that work for instances to reuse.

Open `src/TodoApi/Program.cs` and find the `Initialization.Initializing`
callback. It already warms `/todos`. Add a line below that one:

```csharp
    await app.WarmupAsync("/todos");
    await app.WarmupAsync("/todos/overdue");
```

Save. The application reloads, and from now on every instance starts with the
overdue route already warm.

## 8. Test it

The template's tests in `tests/TodoApi.Tests/TodoApiTests.cs` run the
application in the sandbox and send it HTTP requests. Add one for the new route,
inside the `TodoApiTests` class:

```csharp
    [Fact]
    public async Task A_todo_past_its_due_date_is_listed_as_overdue()
    {
        using var created = await sandbox.Client.PostAsJsonAsync("/todos", new CreateTodo("Renew the passport", new DateOnly(2020, 1, 1)));
        var todo = (await created.Content.ReadFromJsonAsync<TodoResponse>())!;

        var overdue = await sandbox.Client.GetFromJsonAsync<List<TodoResponse>>("/todos/overdue");

        Assert.Contains(overdue!, item => item.Id == todo.Id);
    }
```

The tests start the application themselves, and only one local session can run
an application at a time. Press Ctrl+C in the first terminal to stop
`isoplane dev`, then run:

```bash
dotnet test
```

```text
Passed!  - Failed:     0, Passed:    10, Skipped:     0, Total:    10, Duration: 14 s - TodoApi.Tests.dll (net10.0)
```

The sandbox tests use a disposable database of their own, which is why they
need Docker and why they did not touch the todos you created.

## 9. Clean up

Stop the development database and delete its data:

```bash
docker compose down -v
```

## What you built

You ran an ASP.NET Core and EF Core application in the Isoplane sandbox, and
changed it the way you would change any other: an entity, a migration, an
endpoint and a test. Along the way you saw the things that are particular to
Isoplane:

- An instance can be replaced at any time, so state lives in the database.
- Initialization prepares a template before requests, and a failure there stops the new
  version from starting.
- Work done during initialization, such as warming a route, is shared by every
  instance.

Where to go next:

- [How .NET differs on Isoplane](dotnet-runtime-differences.md) lists what to
  keep in mind as the application grows.
- [The application lifecycle](application-lifecycle.md) covers each stage and
  who is responsible for what.
- [Debug an application](debug-an-application.md) shows how to set a breakpoint
  in the code you just wrote.
- [Build a small lightweight web service](build-a-web-service.md) builds a
  service without ASP.NET.
