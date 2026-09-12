using Microsoft.AspNetCore.Builder;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.DependencyInjection;
using Npgsql;
using Wisp.AspNetCore;

namespace TodoApp;

/// <summary>A PostgreSQL application with verified TLS and request-scoped database contexts.</summary>
public static class Application
{
    /// <summary>Configures services without contacting the database during template initialization.</summary>
    public static WebApplication Create()
    {
        var builder = WispHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        builder.Services.ConfigureHttpJsonOptions(options =>
            options.SerializerOptions.TypeInfoResolverChain.Insert(0, TodoJson.Default));
        builder.Services.AddDbContext<TodoDb>((services, options) =>
        {
            var settings = services.GetRequiredService<Microsoft.Extensions.Configuration.IConfiguration>();
            var configured = settings["ConnectionStrings:Todo"]
                ?? throw new InvalidOperationException("Set ConnectionStrings:Todo with dotnet user-secrets.");
            var connection = new NpgsqlConnectionStringBuilder(configured)
            {
                SslMode = SslMode.VerifyFull,
                Pooling = false
            };
            options.UseModel(Generated.TodoDbModel.Instance).UseNpgsql(connection.ConnectionString);
        });
        var application = builder.Build();
        TodoEndpoints.Map(application);
        return application;
    }
}
