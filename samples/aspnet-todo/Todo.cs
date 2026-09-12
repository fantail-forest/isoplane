using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Http;
using Microsoft.EntityFrameworkCore;

namespace TodoApp;

internal static class TodoEndpoints
{
    internal static void Map(WebApplication application)
    {
        application.Use(async (context, next) =>
        {
            context.Response.OnStarting(() =>
            {
                context.Response.Headers["X-Todo-App"] = "host-server";
                return Task.CompletedTask;
            });
            await next(context);
        });
        application.MapPost("/todos", CreateAsync);
        application.MapGet("/todos", ReadAsync);
        application.MapPut("/todos/{id:int}", UpdateAsync);
        application.MapDelete("/todos/{id:int}", DeleteAsync);
    }

    private static async Task<IResult> CreateAsync(TodoInput input, TodoDb db)
    {
        var todo = new Todo { Title = input.Title };
        db.Add(todo);
        await db.SaveChangesAsync();
        return Results.Created($"/todos/{todo.Id}", todo);
    }

    private static Task<Todo[]> ReadAsync(TodoDb db)
    {
        var context = db;
        return context.Todos.OrderBy(todo => todo.Id).ToArrayAsync();
    }

    private static async Task<IResult> UpdateAsync(int id, TodoInput input, TodoDb db)
    {
        var context = db;
        int todoId = id;
        var todo = await context.Todos.SingleAsync(todo => todo.Id == todoId);
        todo.Title = input.Title;
        todo.Done = input.Done;
        await db.SaveChangesAsync();
        return Results.Ok(todo);
    }

    private static async Task<IResult> DeleteAsync(int id, TodoDb db)
    {
        var context = db;
        int todoId = id;
        var todo = await context.Todos.SingleAsync(todo => todo.Id == todoId);
        db.Remove(todo);
        await db.SaveChangesAsync();
        return Results.NoContent();
    }
}

internal sealed class TodoDb(DbContextOptions<TodoDb> options) : DbContext(options)
{
    public DbSet<Todo> Todos { get; set; }
}

/// <summary>A persisted TODO row.</summary>
public class Todo
{
    /// <summary>The database-generated identifier.</summary>
    public int Id { get; set; }
    /// <summary>The task title.</summary>
    public string Title { get; set; } = "";
    /// <summary>Whether the task is complete.</summary>
    public bool Done { get; set; }
}

/// <summary>The input used when creating or updating a TODO.</summary>
/// <param name="Title">The task title.</param>
/// <param name="Done">The completion state applied by an update.</param>
public sealed record TodoInput(string Title, bool Done);
