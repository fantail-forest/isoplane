using Sample_Api.Todos;

namespace Sample_Api.Tests;

/// <summary>Ordinary unit tests: application logic runs on the test machine, outside the sandbox.</summary>
public sealed class TodoTests
{
    private static readonly DateTimeOffset Now = new(2026, 1, 2, 3, 4, 5, TimeSpan.Zero);

    [Fact]
    public void Completing_a_todo_records_when_it_happened()
    {
        var todo = new Todo { Title = "Write tests" };

        Assert.True(todo.Complete(Now));
        Assert.True(todo.IsComplete);
        Assert.Equal(Now, todo.CompletedAt);
    }

    [Fact]
    public void Completing_a_todo_twice_keeps_the_first_completion()
    {
        var todo = new Todo { Title = "Write tests" };
        todo.Complete(Now);

        Assert.False(todo.Complete(Now.AddHours(1)));
        Assert.Equal(Now, todo.CompletedAt);
    }

    [Fact]
    public void Reopening_a_todo_clears_its_completion()
    {
        var todo = new Todo { Title = "Write tests" };
        todo.Complete(Now);

        todo.Reopen();

        Assert.False(todo.IsComplete);
        Assert.Null(todo.CompletedAt);
    }
}
