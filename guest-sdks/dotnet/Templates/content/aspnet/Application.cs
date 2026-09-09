using Microsoft.AspNetCore.Builder;
using Wisp.AspNetCore;

namespace IsoplaneApp;

/// <summary>Builds the application's routes and services during preparation.</summary>
public static class Application
{
    /// <summary>Creates the application without opening a listener.</summary>
    public static WebApplication Create()
    {
        var builder = WispHosting.CreateBuilder(typeof(Application).Assembly.GetName().Name!);
        var app = builder.Build();
        app.MapGet("/", () => "Hello from Isoplane!");
        return app;
    }
}
