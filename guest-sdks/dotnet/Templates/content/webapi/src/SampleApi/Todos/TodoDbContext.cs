using Microsoft.EntityFrameworkCore;

namespace SampleApi.Todos;

public sealed class TodoDbContext(DbContextOptions<TodoDbContext> options) : DbContext(options)
{
    public DbSet<Todo> Todos => Set<Todo>();

    protected override void OnModelCreating(ModelBuilder modelBuilder)
    {
        modelBuilder.Entity<Todo>(todo =>
        {
            todo.Property(item => item.Title).HasMaxLength(200);
            todo.HasIndex(item => item.IsComplete);
        });
    }
}
