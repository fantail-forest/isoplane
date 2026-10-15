using System.Text.Json;
using System.Text.Json.Serialization;
using Isoplane;
using Microsoft.EntityFrameworkCore;
using Npgsql;

namespace PostgresFixture;

/// <summary>Creates, reads, updates and deletes TODO rows through EF Core, with a context per request.</summary>
public static class Handler
{
    /// <summary>
    /// Reads the table once, so the model is built before capture and a database the application
    /// cannot reach, or whose certificate it does not trust, stops it from starting.
    /// </summary>
    [Initializer]
    public static async Task InitializeAsync()
    {
        await using var db = TodoDb.Open();
        await db.Todos.AsNoTracking().ToListAsync();
    }

    [HttpHandler]
    public static async Task<Response> HandleAsync(Request request)
    {
        var segments = request.Path.Split('/', StringSplitOptions.RemoveEmptyEntries);
        if (segments is not ["todos", ..])
            return Response.Text("Not found", 404);
        await using var db = TodoDb.Open();
        return (request.Method, segments) switch
        {
            ("GET", [_]) => Json(await db.Todos.OrderBy(todo => todo.Id).ToListAsync(), TodoJson.Default.ListTodo),
            ("POST", [_]) => await CreateAsync(db, Input(request)),
            ("PUT", [_, var id]) => await UpdateAsync(db, int.Parse(id), Input(request)),
            ("DELETE", [_, var id]) => await DeleteAsync(db, int.Parse(id)),
            _ => Response.Text("Not found", 404)
        };
    }

    private static async Task<Response> CreateAsync(TodoDb db, TodoInput input)
    {
        var todo = new Todo { Title = input.Title, Done = input.Done };
        db.Add(todo);
        await db.SaveChangesAsync();
        return Json(todo, TodoJson.Default.Todo, 201);
    }

    private static async Task<Response> UpdateAsync(TodoDb db, int id, TodoInput input)
    {
        var todo = await db.Todos.SingleAsync(todo => todo.Id == id);
        todo.Title = input.Title;
        todo.Done = input.Done;
        await db.SaveChangesAsync();
        return Json(todo, TodoJson.Default.Todo);
    }

    private static async Task<Response> DeleteAsync(TodoDb db, int id)
    {
        db.Remove(await db.Todos.SingleAsync(todo => todo.Id == id));
        await db.SaveChangesAsync();
        return Response.Http(204, [], []);
    }

    private static TodoInput Input(Request request) =>
        JsonSerializer.Deserialize(request.Body.Span, TodoJson.Default.TodoInput)
        ?? throw new JsonException("The request body is not a TODO.");

    private static Response Json<T>(T value, System.Text.Json.Serialization.Metadata.JsonTypeInfo<T> type, ushort status = 200) =>
        Response.Json(JsonSerializer.Serialize(value, type), status);
}

internal sealed class TodoDb(DbContextOptions<TodoDb> options) : DbContext(options)
{
    public DbSet<Todo> Todos => Set<Todo>();

    public static TodoDb Open()
    {
        var configured = RuntimeConfiguration.ReadValues().GetValueOrDefault("ConnectionStrings:Todo")
            ?? throw new InvalidOperationException("Set ConnectionStrings:Todo with dotnet user-secrets.");
        var connection = new NpgsqlConnectionStringBuilder(configured) { Pooling = false };
        return new TodoDb(new DbContextOptionsBuilder<TodoDb>().UseNpgsql(connection.ConnectionString).Options);
    }
}

public sealed class Todo
{
    public int Id { get; set; }
    public string Title { get; set; } = "";
    public bool Done { get; set; }
}

public sealed record TodoInput(string Title, bool Done);

[JsonSerializable(typeof(List<Todo>))]
[JsonSerializable(typeof(TodoInput))]
[JsonSourceGenerationOptions(JsonSerializerDefaults.Web)]
internal sealed partial class TodoJson : JsonSerializerContext;
