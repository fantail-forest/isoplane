using Microsoft.AspNetCore.Builder;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Configuration;
using Npgsql;
using Isoplane.AspNetCore;

namespace TodoApp;

/// <summary>A PostgreSQL application with verified TLS and request-scoped database contexts.</summary>
public static class Application
{
    /// <summary>Configures the application and registers read-only database warmup before template capture.</summary>
    public static WebApplication Create()
    {
        var builder = IsoplaneHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        builder.Services.ConfigureHttpJsonOptions(options =>
            options.SerializerOptions.TypeInfoResolverChain.Insert(0, TodoJson.Default));
        builder.Services.AddDbContext<TodoDb>((services, options) =>
            ConfigureDatabase(options, services.GetRequiredService<IConfiguration>()));
        var application = builder.Build();
        TodoEndpoints.Map(application);
        Isoplane.Initialization.Initializing += () => Warmup(application);
        return application;
    }

    private static void ConfigureDatabase(DbContextOptionsBuilder options, IConfiguration settings)
    {
        var configured = settings["ConnectionStrings:Todo"]
            ?? throw new InvalidOperationException("Set ConnectionStrings:Todo with dotnet user-secrets.");
        var connection = new NpgsqlConnectionStringBuilder(configured)
        {
            SslMode = SslMode.VerifyFull,
            Pooling = false
        };
        options.UseModel(Generated.TodoDbModel.Instance).UseNpgsql(connection.ConnectionString);
    }

    private static Task Warmup(WebApplication application) => new IsoplaneApplication(application).WarmupAsync("/todos");
}
