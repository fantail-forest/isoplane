using Microsoft.EntityFrameworkCore;

namespace Sample_Api.Todos;

/// <summary>
/// The application's database. To change the schema, change the entities or the
/// model below, then add a migration:
/// <c>dotnet ef migrations add NAME --project src/Sample-Api</c>.
/// </summary>
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
