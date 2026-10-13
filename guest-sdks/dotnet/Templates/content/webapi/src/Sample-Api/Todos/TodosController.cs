using Microsoft.AspNetCore.Mvc;
using Microsoft.EntityFrameworkCore;
using Sample_Api.Notifications;

namespace Sample_Api.Todos;

/// <summary>
/// The todo endpoints, as a controller. A new feature follows the same shape:
/// a folder with its entity, a controller like this one, and a services
/// extension that registers what the controller needs. AddControllers in
/// Program.cs finds every controller in the project, and MapControllers maps
/// their routes.
/// </summary>
// [ApiController] validates the request body before an action runs, using the
// attributes on CreateTodo and UpdateTodo, and answers 400 with a problem
// description when they fail.
[ApiController]
[Route("todos")]
[Tags("Todos")]
public sealed class TodosController(TodoDbContext db, TimeProvider clock, CompletionNotifier notifier) : ControllerBase
{
    // Action parameters are bound as in any controller: route values and query
    // strings by name, the body by [FromBody] inference, and the request's
    // CancellationToken, which is cancelled when the client goes away. Services
    // such as TodoDbContext arrive through the constructor.
    [HttpGet]
    public async Task<ActionResult<List<TodoResponse>>> List(bool? complete, CancellationToken cancellation) =>
        await db.Todos.AsNoTracking()
            .Where(todo => complete == null || todo.IsComplete == complete)
            .OrderBy(todo => todo.Id)
            .Select(todo => TodoResponse.From(todo))
            .ToListAsync(cancellation);

    [HttpGet("{id:int}", Name = nameof(Get))]
    public async Task<ActionResult<TodoResponse>> Get(int id, CancellationToken cancellation) =>
        await db.Todos.FindAsync([id], cancellation) is { } todo ? TodoResponse.From(todo) : NotFound();

    [HttpPost]
    public async Task<ActionResult<TodoResponse>> Create(CreateTodo request, CancellationToken cancellation)
    {
        var todo = new Todo { Title = request.Title, CreatedAt = clock.GetUtcNow() };
        db.Todos.Add(todo);
        await db.SaveChangesAsync(cancellation);
        return CreatedAtRoute(nameof(Get), new { id = todo.Id }, TodoResponse.From(todo));
    }

    [HttpPut("{id:int}")]
    public async Task<ActionResult<TodoResponse>> Update(int id, UpdateTodo request, CancellationToken cancellation)
    {
        if (await db.Todos.FindAsync([id], cancellation) is not { } todo) return NotFound();
        todo.Title = request.Title;
        var completed = request.IsComplete && todo.Complete(clock.GetUtcNow());
        if (!request.IsComplete) todo.Reopen();
        await db.SaveChangesAsync(cancellation);
        // Work a request starts is awaited before it responds: nothing runs in an
        // instance after its response, so a fire-and-forget task may never finish.
        if (completed) await notifier.NotifyAsync(todo, cancellation);
        return TodoResponse.From(todo);
    }

    [HttpDelete("{id:int}")]
    public async Task<IActionResult> Delete(int id, CancellationToken cancellation) =>
        await db.Todos.Where(todo => todo.Id == id).ExecuteDeleteAsync(cancellation) == 1 ? NoContent() : NotFound();

    // The response is written as rows arrive from the database, without holding the whole table in memory.
    [HttpGet("export")]
    public IAsyncEnumerable<TodoResponse> Export() =>
        db.Todos.AsNoTracking().OrderBy(todo => todo.Id).Select(todo => TodoResponse.From(todo)).AsAsyncEnumerable();
}
