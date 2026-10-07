using Microsoft.EntityFrameworkCore;
using Npgsql;

namespace Sample_Api.Todos;

public static class TodoServices
{
    public static IServiceCollection AddTodos(this IServiceCollection services, IConfiguration configuration)
    {
        services.AddSingleton(TimeProvider.System);
        services.AddDbContext<TodoDbContext>(options =>
        {
            // The connection string comes from configuration: appsettings.Development.json
            // while developing, User Secrets for a real database (see Program.cs).
            var connection = new NpgsqlConnectionStringBuilder(configuration.GetConnectionString("Todos")
                ?? throw new InvalidOperationException("Set ConnectionStrings:Todos. See README.md for the local database."))
            {
                // Isoplane decides how many instances run and may capture or discard any of them
                // between requests, so a pool inside one instance cannot keep a connection open.
                // Use the database provider's own pooler to share connections across instances.
                Pooling = false
            };
            options.UseNpgsql(connection.ConnectionString);
        });
        return services;
    }
}
