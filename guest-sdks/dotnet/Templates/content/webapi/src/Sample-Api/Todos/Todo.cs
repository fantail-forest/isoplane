using System.ComponentModel.DataAnnotations;

namespace Sample_Api.Todos;

/// <summary>
/// The entity. Its rules live in its methods, where unit tests reach them
/// without a database or the sandbox (tests/Sample-Api.Tests/TodoTests.cs).
/// </summary>
public sealed class Todo
{
    public int Id { get; set; }

    public required string Title { get; set; }

    public bool IsComplete { get; private set; }

    public DateTimeOffset CreatedAt { get; init; }

    public DateTimeOffset? CompletedAt { get; private set; }

    /// <summary>Completes the todo at <paramref name="now"/>. Returns false when it already was complete.</summary>
    public bool Complete(DateTimeOffset now)
    {
        if (IsComplete) return false;
        IsComplete = true;
        CompletedAt = now;
        return true;
    }

    public void Reopen()
    {
        IsComplete = false;
        CompletedAt = null;
    }
}

// Request bodies. The attributes are enforced by AddValidation in Program.cs.
public sealed record CreateTodo([Required, StringLength(200, MinimumLength = 1)] string Title);

public sealed record UpdateTodo([Required, StringLength(200, MinimumLength = 1)] string Title, bool IsComplete);

/// <summary>A todo as the API returns it.</summary>
public sealed record TodoResponse(int Id, string Title, bool IsComplete, DateTimeOffset CreatedAt, DateTimeOffset? CompletedAt)
{
    public static TodoResponse From(Todo todo) => new(todo.Id, todo.Title, todo.IsComplete, todo.CreatedAt, todo.CompletedAt);
}
