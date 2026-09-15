using System.Globalization;
using System.Text.Json;
using Npgsql;
using Wisp;
using Wisp.Durable;

namespace MinimalTodo;

/// <summary>A PostgreSQL TODO handler without ASP.NET or EF Core.</summary>
public sealed class Application : IWispHandler
{
    /// <summary>Constructs the handler without reading secrets or opening connections during preparation.</summary>
    public static IWispHandler Create()
    {
        Initialization.Initializing += () =>
        {
            _ = TodoJson.Default.Todo;
            _ = TodoJson.Default.TodoArray;
            _ = TodoJson.Default.TodoInput;
        };
        return new Application();
    }

    /// <summary>Handles TODO CRUD with verified TLS and request-owned database connections.</summary>
    public async Task<Response> HandleAsync(Request request, Ctx context)
    {
        var collection = request.Path == "/todos";
        var id = 0;
        if (!collection && !(request.Path.StartsWith("/todos/", StringComparison.Ordinal)
            && int.TryParse(request.Path.AsSpan(7), NumberStyles.None, CultureInfo.InvariantCulture, out id)))
            return Response.Text("Not found", 404);
        if (!(collection && request.Method is "GET" or "POST") && !(!collection && request.Method is "PUT" or "DELETE"))
            return Response.Text("Method not allowed", 405);
        var settings = RuntimeConfiguration.ReadValues();
        if (!settings.TryGetValue("ConnectionStrings:Todo", out var configured))
            throw new InvalidOperationException("Set ConnectionStrings:Todo through runtime configuration.");
        var connectionString = new NpgsqlConnectionStringBuilder(configured) { SslMode = SslMode.VerifyFull, Pooling = false };
        await using var connection = new NpgsqlConnection(connectionString.ConnectionString);
        await connection.OpenAsync(request.Aborted);
        return request.Method switch
        {
            "GET" => await Read(connection, request.Aborted),
            "POST" => await Create(connection, Input(request), request.Aborted),
            "PUT" => await Update(connection, id, Input(request), request.Aborted),
            "DELETE" => await Delete(connection, id, request.Aborted),
            _ => throw new InvalidOperationException("The route admitted an unsupported method.")
        };
    }

    private static TodoInput Input(Request request) => JsonSerializer.Deserialize(request.Body.Span, TodoJson.Default.TodoInput)
        ?? throw new JsonException("A TODO body is required.");

    private static async Task<Response> Read(NpgsqlConnection connection, CancellationToken cancellation)
    {
        await using var command = new NpgsqlCommand("SELECT \"Id\", \"Title\", \"Done\" FROM \"Todos\" ORDER BY \"Id\"", connection);
        await using var reader = await command.ExecuteReaderAsync(cancellation);
        var todos = new List<Todo>();
        while (await reader.ReadAsync(cancellation)) todos.Add(Row(reader));
        return Response.Json(JsonSerializer.Serialize(todos.ToArray(), TodoJson.Default.TodoArray));
    }

    private static async Task<Response> Create(NpgsqlConnection connection, TodoInput input, CancellationToken cancellation)
    {
        await using var command = new NpgsqlCommand("INSERT INTO \"Todos\" (\"Title\", \"Done\") VALUES ($1, false) RETURNING \"Id\", \"Title\", \"Done\"", connection);
        command.Parameters.AddWithValue(input.Title);
        await using var reader = await command.ExecuteReaderAsync(cancellation);
        if (!await reader.ReadAsync(cancellation)) throw new InvalidOperationException("The database did not return the created TODO.");
        var todo = Row(reader);
        return Response.Http(201, JsonSerializer.SerializeToUtf8Bytes(todo, TodoJson.Default.Todo),
            [new("content-type", "application/json"), new("location", "/todos/" + todo.Id.ToString(CultureInfo.InvariantCulture))]);
    }

    private static async Task<Response> Update(NpgsqlConnection connection, int id, TodoInput input, CancellationToken cancellation)
    {
        await using var command = new NpgsqlCommand("UPDATE \"Todos\" SET \"Title\"=$1, \"Done\"=$2 WHERE \"Id\"=$3 RETURNING \"Id\", \"Title\", \"Done\"", connection);
        command.Parameters.AddWithValue(input.Title);
        command.Parameters.AddWithValue(input.Done);
        command.Parameters.AddWithValue(id);
        await using var reader = await command.ExecuteReaderAsync(cancellation);
        if (!await reader.ReadAsync(cancellation)) return Response.Text("Not found", 404);
        return Response.Json(JsonSerializer.Serialize(Row(reader), TodoJson.Default.Todo));
    }

    private static async Task<Response> Delete(NpgsqlConnection connection, int id, CancellationToken cancellation)
    {
        await using var command = new NpgsqlCommand("DELETE FROM \"Todos\" WHERE \"Id\"=$1", connection);
        command.Parameters.AddWithValue(id);
        return await command.ExecuteNonQueryAsync(cancellation) == 0 ? Response.Text("Not found", 404) : Response.Raw([], 204);
    }

    private static Todo Row(NpgsqlDataReader reader) => new(reader.GetInt32(0), reader.GetString(1), reader.GetBoolean(2));
}
