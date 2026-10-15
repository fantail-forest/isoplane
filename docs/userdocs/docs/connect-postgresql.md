---
title: Connect to PostgreSQL
---

This guide connects a small service to PostgreSQL through the Npgsql driver. You
will create a service from the `isoplane-handler` template, start a local
database in Docker, keep the connection string in User Secrets, and add two
routes that read and write a table.

For a new project with a few simple tables, start here: SQL through the driver
is less code to build and to load into every instance than ASP.NET Core and
Entity Framework Core. When you want those, [Build a small API](build-an-api.md)
uses them with PostgreSQL.

Before starting, [install the tools](install.md). You also need Docker and
`curl`; the commands are written for bash or zsh.

## 1. Create the project

```bash
dotnet new isoplane-handler --name NotesService
cd NotesService
dotnet add src/NotesService package Npgsql
```

The [`isoplane-handler`](reference/project-templates.md#isoplane-handler)
template creates a service without ASP.NET; its `README.md` describes each file.
[Build a small lightweight web service](build-a-web-service.md) walks through it.

## 2. Start a local database

```bash
docker run --name notes-db --detach --publish 127.0.0.1:5432:5432 \
  --env POSTGRES_PASSWORD=development postgres:17
```

This runs the official PostgreSQL image, reachable only from your machine. The
password is for this disposable database alone. If something already listens on
port 5432, publish another port, such as `127.0.0.1:5433:5432`, and use that
port in the connection string below.

Wait for the database to accept connections, then create the table:

```bash
until docker exec notes-db pg_isready --host 127.0.0.1 --quiet; do sleep 1; done
docker exec notes-db psql -U postgres \
  -c 'CREATE TABLE notes (id integer GENERATED ALWAYS AS IDENTITY PRIMARY KEY, text text NOT NULL)'
```

The service only reads and writes rows. You create and change tables yourself,
with `psql`, your provider's SQL console or a migration tool.

## 3. Set the connection string

```bash
dotnet user-secrets set "ConnectionStrings:Notes" \
  "Host=127.0.0.1;Port=5432;Username=postgres;Password=development" \
  --project src/NotesService
```

[User Secrets](local-settings.md#keep-credentials-in-user-secrets) keep the
password outside the project, and a local session passes them to the
application. The template's project file already has the `UserSecretsId` they
need. `ConnectionStrings:Notes` is the name the code in the next step reads;
[Npgsql's connection string parameters](https://www.npgsql.org/doc/connection-string-parameters.html)
define `Host`, `Port` and the other keys.

The local database serves plain connections. Traffic to it does not leave your
machine, so it needs no TLS.

## 4. Read and write the table

Create `src/NotesService/NoteStore.cs`:

```csharp
using System.Text.Json;
using System.Text.Json.Serialization;
using Isoplane;
using Npgsql;

namespace NotesService;

public sealed record Note(int Id, string Text);

public sealed record NewNote(string Text);

/// <summary>Reads and writes the notes table, with one connection for each request.</summary>
public static class NoteStore
{
    public static async Task<Response> ListAsync(Request request)
    {
        // "await using" closes the connection, command and reader before the
        // response is returned. Nothing is left open between requests.
        await using var connection = await OpenAsync(request.Aborted);
        await using var command = new NpgsqlCommand("SELECT id, text FROM notes ORDER BY id", connection);
        await using var reader = await command.ExecuteReaderAsync(request.Aborted);
        var notes = new List<Note>();
        while (await reader.ReadAsync(request.Aborted))
        {
            notes.Add(new Note(reader.GetInt32(0), reader.GetString(1)));
        }
        return Response.Json(JsonSerializer.Serialize(notes, NotesJson.Default.ListNote));
    }

    public static async Task<Response> AddAsync(Request request)
    {
        NewNote? note;
        try { note = JsonSerializer.Deserialize(request.Body.Span, NotesJson.Default.NewNote); }
        catch (JsonException) { note = null; }
        if (string.IsNullOrWhiteSpace(note?.Text))
        {
            return Response.Text("Send a JSON note: {\"text\":\"...\"}", 400);
        }

        await using var connection = await OpenAsync(request.Aborted);
        // $1 is a parameter: the note's text is sent apart from the SQL, never pasted into it.
        await using var command = new NpgsqlCommand("INSERT INTO notes (text) VALUES ($1) RETURNING id", connection);
        command.Parameters.Add(new NpgsqlParameter { Value = note.Text });
        var id = (int)(await command.ExecuteScalarAsync(request.Aborted))!;
        return Response.Json(JsonSerializer.Serialize(new Note(id, note.Text), NotesJson.Default.Note), 201);
    }

    private static async Task<NpgsqlConnection> OpenAsync(CancellationToken cancellation)
    {
        // Read the connection string while handling the request, not in the initializer.
        // What the initializer keeps is saved in the template every instance starts
        // from, and the connection string holds the password.
        var configured = RuntimeConfiguration.ReadValues().GetValueOrDefault("ConnectionStrings:Notes")
            ?? throw new InvalidOperationException("Set ConnectionStrings:Notes in User Secrets.");
        var settings = new NpgsqlConnectionStringBuilder(configured)
        {
            // Isoplane decides how many instances run and may discard any of them
            // between requests, so a pool inside one instance neither limits the
            // connections the application opens nor keeps them for long. Pool in
            // front of the database instead.
            Pooling = false
        };
        var connection = new NpgsqlConnection(settings.ConnectionString);
        await connection.OpenAsync(cancellation);
        return connection;
    }
}

// Source-generated JSON serialization, as in the template's Greeter.cs: list each
// type the service sends or receives.
[JsonSerializable(typeof(Note))]
[JsonSerializable(typeof(List<Note>))]
[JsonSerializable(typeof(NewNote))]
[JsonSourceGenerationOptions(JsonSerializerDefaults.Web)]
public sealed partial class NotesJson : JsonSerializerContext;
```

Then route requests to it. Open `src/NotesService/Handler.cs` and add two lines
at the top of the switch in `HandleAsync`:

```csharp
    public static Task<Response> HandleAsync(Request request) => (request.Method, request.Path) switch
    {
        ("GET", "/notes") => NoteStore.ListAsync(request),
        ("POST", "/notes") => NoteStore.AddAsync(request),
```

Two choices in this code are particular to Isoplane:

- **A connection for each request, with the driver's pooling off.** Isoplane
  runs as many instances as the load needs, so a pool in each one does not cap
  the connections the service opens.
  [Pool database connections outside the application](dotnet-runtime-differences.md#pool-database-connections-outside-the-application)
  explains what to use instead.
- **The connection string is read during the request.** Settings are also
  available in the `[Initializer]` method, but what it keeps in memory is saved
  in the template that every instance starts from.
  [The application lifecycle](application-lifecycle.md) describes the template.

## 5. Run it and call it

```bash
dotnet run --project src/NotesService
```

When the service is ready, the command prints its address:

```text
  ready NotesService at http://127.0.0.1:5236 (local guest execution)
```

Your port will differ. In a second terminal, add a note and list the notes:

```bash
curl --json '{"text":"Buy milk"}' http://127.0.0.1:5236/notes
curl http://127.0.0.1:5236/notes
```

```json
{"id":1,"text":"Buy milk"}
[{"id":1,"text":"Buy milk"}]
```

The row is in the database, not in the service. Check it there:

```bash
docker exec notes-db psql -U postgres -c 'SELECT * FROM notes'
```

Then stop the service with Ctrl+C, start it again with the same `dotnet run`
command, and list the notes: the note is still there.

## Local private trust

A local session trusts the certificate authorities your machine trusts, as any
.NET program does. For a local database with a private CA, obtain the CA
certificate from whoever created that database, then either install it in your
operating system's trust store or name its PEM file in
[`SSL_CERT_FILE`](local-settings.md#reach-local-services-and-private-certificate-authorities)
for one run:

```bash
SSL_CERT_FILE=/path/to/local-ca.pem dotnet run
```

The database certificate still needs a matching hostname or IP address and a
valid chain. Trusting its CA does not disable those checks. Keep its private key
on the database server, not in the application. Restart the development session
after changing which authorities are trusted.

## Clean up

Stop the service, then remove the database, its data and the connection string:

```bash
docker rm --force notes-db
dotnet user-secrets remove "ConnectionStrings:Notes" --project src/NotesService
```
