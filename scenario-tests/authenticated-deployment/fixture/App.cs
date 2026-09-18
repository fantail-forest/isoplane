using System.Globalization;
using System.Resources;
using Microsoft.AspNetCore.Builder;
using Microsoft.Extensions.Logging;
using Isoplane.AspNetCore;
namespace Example;
public static class App
{
    private static void VerifyData()
    {
        if (File.ReadAllText("/data/greeting.txt") != "immutable application data")
            throw new InvalidDataException("Immutable application asset was lost.");
        var resources = new ResourceManager("App.Messages", typeof(App).Assembly);
        if (resources.GetString("Greeting", CultureInfo.GetCultureInfo("fr-CA")) != "bonjour")
            throw new MissingManifestResourceException("Satellite culture fallback was lost.");
    }
    public static WebApplication Create()
    {
        var builder = IsoplaneHosting.CreateBuilder(typeof(App).Assembly.GetName().Name!);
        VerifyData();
        var app = builder.Build();
        app.MapGet("/", (ILoggerFactory logging) =>
        {
            VerifyData();
            logging.CreateLogger("DeploymentJourney").LogInformation("Source-free request received");
            Console.WriteLine("source-free-console");
            return "source free";
        });
        return app;
    }
}
