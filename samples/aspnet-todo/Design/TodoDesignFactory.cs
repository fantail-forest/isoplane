using Microsoft.EntityFrameworkCore;
using Microsoft.EntityFrameworkCore.Design;

namespace TodoApp;

internal sealed class TodoDesignFactory : IDesignTimeDbContextFactory<TodoDb>
{
    public TodoDb CreateDbContext(string[] args) => new(
        new DbContextOptionsBuilder<TodoDb>()
            .UseNpgsql("Host=localhost;Database=todo;Username=todo;SSL Mode=VerifyFull;Pooling=false")
            .Options);
}
