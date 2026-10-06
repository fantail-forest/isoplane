using Isoplane;
using Isoplane.AspNetCore;
using Microsoft.EntityFrameworkCore;
using Npgsql;
using TodoApp;

// A PostgreSQL application with verified TLS and request-scoped database contexts.

// EF compiled models initialize synchronously because guests cannot start OS threads.
AppContext.SetSwitch("Microsoft.EntityFrameworkCore.Issue31751", true);
var builder = IsoplaneHosting.CreateBuilder(args);
builder.Services.ConfigureHttpJsonOptions(options =>
    options.SerializerOptions.TypeInfoResolverChain.Insert(0, TodoJson.Default));
builder.Services.AddDbContext<TodoDb>((services, options) =>
{
    var configured = services.GetRequiredService<IConfiguration>()["ConnectionStrings:Todo"]
        ?? throw new InvalidOperationException("Set ConnectionStrings:Todo with dotnet user-secrets.");
    var connection = new NpgsqlConnectionStringBuilder(configured)
    {
        SslMode = SslMode.VerifyFull,
        Pooling = false
    };
    options.UseModel(TodoApp.Generated.TodoDbModel.Instance).UseNpgsql(connection.ConnectionString);
});
var application = builder.Build();
TodoEndpoints.Map(application);

// Read-only database warmup, captured in the template every instance starts from.
Initialization.Initializing += () => application.WarmupAsync("/todos");

await application.RunAsync();
