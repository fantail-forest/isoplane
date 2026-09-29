using System.Globalization;
using System.Text.Json;
using Npgsql;
using Isoplane;

namespace MinimalTodo;

/// <summary>A PostgreSQL TODO handler without ASP.NET or EF Core.</summary>
public sealed class Application : IIsoplaneHandler
{
    /// <summary>Warms the read endpoint before template capture.</summary>
    public static IIsoplaneHandler Create()
    {
        Initialization.Initializing += Warmup;
        return new Application();
    }

    /// <summary>Handles TODO CRUD with verified TLS and request-owned database connections.</summary>
    public Task<Response> HandleAsync(Request request, Ctx context) =>
        HandleAsync(request.Method, request.Path, request.Body, request.Aborted);

    private static async Task<Response> HandleAsync(string method, string path, ReadOnlyMemory<byte> body, CancellationToken cancellation)
    {
        var collection = path == "/todos";
        var id = 0;
        if (!collection && !(path.StartsWith("/todos/", StringComparison.Ordinal)
            && int.TryParse(path.AsSpan(7), NumberStyles.None, CultureInfo.InvariantCulture, out id)))
            return Response.Text("Not found", 404);
        if (!(collection && method is "GET" or "POST") && !(!collection && method is "PUT" or "DELETE"))
            return Response.Text("Method not allowed", 405);
        await using var connection = CreateConnection();
        await connection.OpenAsync(cancellation);
        return method switch
        {
            "GET" => await Read(connection, cancellation),
            "POST" => await Create(connection, Input(body), cancellation),
            "PUT" => await Update(connection, id, Input(body), cancellation),
            "DELETE" => await Delete(connection, id, cancellation),
            _ => throw new InvalidOperationException("The route admitted an unsupported method.")
        };
    }

    private static NpgsqlConnection CreateConnection()
    {
        var settings = RuntimeConfiguration.ReadValues();
        if (!settings.TryGetValue("ConnectionStrings:Todo", out var configured))
            throw new InvalidOperationException("Set ConnectionStrings:Todo through runtime configuration.");
        var connectionString = new NpgsqlConnectionStringBuilder(configured) { SslMode = SslMode.VerifyFull, Pooling = false };
        return new NpgsqlConnection(connectionString.ConnectionString);
    }

    private static async Task Warmup() =>
        _ = await HandleAsync("GET", "/todos", ReadOnlyMemory<byte>.Empty, CancellationToken.None);

    private static TodoInput Input(ReadOnlyMemory<byte> body) => JsonSerializer.Deserialize(body.Span, TodoJson.Default.TodoInput)
        ?? throw new JsonException("A TODO body is required.");

    private static async Task<Response> Read(NpgsqlConnection connection, CancellationToken cancellation)
    {
        await using var command = ReadCommand(connection);
        await using var reader = await command.ExecuteReaderAsync(cancellation);
        var todos = new List<Todo>();
        while (await reader.ReadAsync(cancellation)) todos.Add(Row(reader));
        return Response.Json(JsonSerializer.Serialize(todos.ToArray(), TodoJson.Default.TodoArray));
    }

    private static async Task<Response> Create(NpgsqlConnection connection, TodoInput input, CancellationToken cancellation)
    {
        await using var command = CreateCommand(connection, input);
        await using var reader = await command.ExecuteReaderAsync(cancellation);
        if (!await reader.ReadAsync(cancellation)) throw new InvalidOperationException("The database did not return the created TODO.");
        var todo = Row(reader);
        return Response.Http(201, JsonSerializer.SerializeToUtf8Bytes(todo, TodoJson.Default.Todo),
            [new("content-type", "application/json"), new("location", "/todos/" + todo.Id.ToString(CultureInfo.InvariantCulture))]);
    }

    private static async Task<Response> Update(NpgsqlConnection connection, int id, TodoInput input, CancellationToken cancellation)
    {
        await using var command = UpdateCommand(connection, id, input);
        await using var reader = await command.ExecuteReaderAsync(cancellation);
        if (!await reader.ReadAsync(cancellation)) return Response.Text("Not found", 404);
        return Response.Json(JsonSerializer.Serialize(Row(reader), TodoJson.Default.Todo));
    }

    private static async Task<Response> Delete(NpgsqlConnection connection, int id, CancellationToken cancellation)
    {
        await using var command = DeleteCommand(connection, id);
        return await command.ExecuteNonQueryAsync(cancellation) == 0 ? Response.Text("Not found", 404) : Response.Raw([], 204);
    }

    private static NpgsqlCommand CreateCommand(NpgsqlConnection connection, TodoInput input)
    {
        var command = new NpgsqlCommand("INSERT INTO \"Todos\" (\"Title\", \"Done\") VALUES ($1, false) RETURNING \"Id\", \"Title\", \"Done\"", connection);
        command.Parameters.AddWithValue(input.Title);
        return command;
    }

    private static NpgsqlCommand UpdateCommand(NpgsqlConnection connection, int id, TodoInput input)
    {
        var command = new NpgsqlCommand("UPDATE \"Todos\" SET \"Title\"=$1, \"Done\"=$2 WHERE \"Id\"=$3 RETURNING \"Id\", \"Title\", \"Done\"", connection);
        command.Parameters.AddWithValue(input.Title);
        command.Parameters.AddWithValue(input.Done);
        command.Parameters.AddWithValue(id);
        return command;
    }

    private static NpgsqlCommand DeleteCommand(NpgsqlConnection connection, int id)
    {
        var command = new NpgsqlCommand("DELETE FROM \"Todos\" WHERE \"Id\"=$1", connection);
        command.Parameters.AddWithValue(id);
        return command;
    }

    private static NpgsqlCommand ReadCommand(NpgsqlConnection connection)
    {
        return new NpgsqlCommand("SELECT \"Id\", \"Title\", \"Done\" FROM \"Todos\" ORDER BY \"Id\"", connection);
    }

    private static Todo Row(NpgsqlDataReader reader) => new(reader.GetInt32(0), reader.GetString(1), reader.GetBoolean(2));
}
