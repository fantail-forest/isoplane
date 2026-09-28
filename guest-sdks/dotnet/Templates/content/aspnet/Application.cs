using Microsoft.AspNetCore.Builder;
using Isoplane.AspNetCore;

namespace SampleApp;

/// <summary>Builds the application's routes and services during preparation.</summary>
public static class Application
{
    /// <summary>Creates the application without opening a listener.</summary>
    public static WebApplication Create()
    {
        var builder = IsoplaneHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        var app = builder.Build();
        app.MapGet("/", () => "Hello from Isoplane!");
        Isoplane.Initialization.Initializing += () => new IsoplaneApplication(app).WarmupAsync("/");
        return app;
    }
}
