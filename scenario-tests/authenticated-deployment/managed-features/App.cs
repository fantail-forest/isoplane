using System.Linq.Expressions;
using System.Reflection;
using System.Reflection.Emit;
using Microsoft.AspNetCore.Builder;
using Microsoft.EntityFrameworkCore;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Isoplane.AspNetCore;

namespace ManagedFeatures;

/// <summary>Exercises interpreted managed code and EF query translation after deployment.</summary>
public static class App
{
    /// <summary>Builds the application and warms its managed-feature read route before template capture.</summary>
    public static WebApplication Create()
    {
        var builder = IsoplaneHosting.CreateBuilder(typeof(App).Assembly.GetName().Name!);
        builder.Services.AddScoped<QueryService>();
        var app = builder.Build();
        app.Use(async (context, next) =>
        {
            try
            {
                await next(context);
            }
            catch (Exception error)
            {
                Console.WriteLine("managed-feature-failure: " + error);
                throw;
            }
        });
        app.MapGet("/", async (QueryService service, ILoggerFactory logging) =>
        {
            service.Verify();
            await Task.Delay(1);
            service.Verify();
            logging.CreateLogger("DeploymentJourney").LogInformation("Source-free request received");
            Console.WriteLine("source-free-console");
            return "source free";
        });
        Isoplane.Initialization.Initializing += () => new IsoplaneApplication(app).WarmupAsync("/");
        return app;
    }

    public sealed class QueryService
    {
        public void Verify()
        {
            Expression<Func<int, int>> expression = value => value + 1;
            if (expression.Compile()(41) != 42)
                throw new InvalidOperationException("A compiled managed expression returned the wrong value.");

            var method = new DynamicMethod("AddOne", typeof(int), [typeof(int)]);
            var il = method.GetILGenerator();
            il.Emit(OpCodes.Ldarg_0);
            il.Emit(OpCodes.Ldc_I4_1);
            il.Emit(OpCodes.Add);
            il.Emit(OpCodes.Ret);
            if (method.CreateDelegate<Func<int, int>>()(41) != 42)
                throw new InvalidOperationException("Generated managed IL returned the wrong value.");

            var selected = Type.GetType("System.Int32", throwOnError: true)!;
            var generic = typeof(QueryService).GetMethod(nameof(Echo))!.MakeGenericMethod(selected);
            if (!Equals(generic.Invoke(null, [42]), 42))
                throw new InvalidOperationException("The runtime-selected generic lost its invocation target.");

            using var database = new ModelContext();
            var sql = database.Items.Where(item => item.Id == 42).ToQueryString();
            if (!sql.Contains("42", StringComparison.Ordinal) || !sql.Contains("Items", StringComparison.Ordinal))
                throw new InvalidOperationException("EF failed to translate the runtime query using its model.");
        }

        public static T Echo<T>(T value) => value;
    }

    private sealed class ModelContext : DbContext
    {
        public DbSet<Item> Items => Set<Item>();
        protected override void OnConfiguring(DbContextOptionsBuilder options)
            => options.UseNpgsql("Host=localhost;Database=translation_only;Pooling=false");
    }

    private sealed class Item
    {
        public int Id { get; set; }
    }
}
