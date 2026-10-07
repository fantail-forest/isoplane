using Microsoft.AspNetCore.Http.HttpResults;
using Microsoft.EntityFrameworkCore;
using Sample_Api.Notifications;

namespace Sample_Api.Todos;

/// <summary>
/// The todo endpoints. A new feature follows the same shape: a folder with its
/// entity, an endpoints class like this one mapped from Program.cs, and a
/// services extension that registers what the endpoints need.
/// </summary>
public static class TodoEndpoints
{
    public static RouteGroupBuilder MapTodos(this IEndpointRouteBuilder routes)
    {
        var todos = routes.MapGroup("/todos").WithTags("Todos");
        todos.MapGet("/", List);
        todos.MapGet("/{id:int}", Get).WithName(nameof(Get));
        todos.MapPost("/", Create);
        todos.MapPut("/{id:int}", Update);
        todos.MapDelete("/{id:int}", Delete);
        todos.MapGet("/export", Export);
        return todos;
    }

    // Handler parameters are bound as in any minimal API: route values and query
    // strings by name, registered services such as TodoDbContext by type, and the
    // request's CancellationToken, which is cancelled when the client goes away.
    private static async Task<Ok<List<TodoResponse>>> List(TodoDbContext db, bool? complete, CancellationToken cancellation) =>
        TypedResults.Ok(await db.Todos.AsNoTracking()
            .Where(todo => complete == null || todo.IsComplete == complete)
            .OrderBy(todo => todo.Id)
            .Select(todo => TodoResponse.From(todo))
            .ToListAsync(cancellation));

    private static async Task<Results<Ok<TodoResponse>, NotFound>> Get(int id, TodoDbContext db, CancellationToken cancellation) =>
        await db.Todos.FindAsync([id], cancellation) is { } todo ? TypedResults.Ok(TodoResponse.From(todo)) : TypedResults.NotFound();

    // AddValidation in Program.cs checks the attributes on CreateTodo before this
    // runs, and answers 400 with a problem description when they fail.
    private static async Task<CreatedAtRoute<TodoResponse>> Create(CreateTodo request, TodoDbContext db, TimeProvider clock, CancellationToken cancellation)
    {
        var todo = new Todo { Title = request.Title, CreatedAt = clock.GetUtcNow() };
        db.Todos.Add(todo);
        await db.SaveChangesAsync(cancellation);
        return TypedResults.CreatedAtRoute(TodoResponse.From(todo), nameof(Get), new { id = todo.Id });
    }

    private static async Task<Results<Ok<TodoResponse>, NotFound>> Update(int id, UpdateTodo request, TodoDbContext db, TimeProvider clock,
        CompletionNotifier notifier, CancellationToken cancellation)
    {
        if (await db.Todos.FindAsync([id], cancellation) is not { } todo) return TypedResults.NotFound();
        todo.Title = request.Title;
        var completed = request.IsComplete && todo.Complete(clock.GetUtcNow());
        if (!request.IsComplete) todo.Reopen();
        await db.SaveChangesAsync(cancellation);
        // Work a request starts is awaited before it responds: nothing runs in an
        // instance after its response, so a fire-and-forget task may never finish.
        if (completed) await notifier.NotifyAsync(todo, cancellation);
        return TypedResults.Ok(TodoResponse.From(todo));
    }

    private static async Task<Results<NoContent, NotFound>> Delete(int id, TodoDbContext db, CancellationToken cancellation) =>
        await db.Todos.Where(todo => todo.Id == id).ExecuteDeleteAsync(cancellation) == 1 ? TypedResults.NoContent() : TypedResults.NotFound();

    // The response is written as rows arrive from the database, without holding the whole table in memory.
    private static IAsyncEnumerable<TodoResponse> Export(TodoDbContext db) =>
        db.Todos.AsNoTracking().OrderBy(todo => todo.Id).Select(todo => TodoResponse.From(todo)).AsAsyncEnumerable();
}
