using System;
using System.Linq;
using System.Reflection;
using System.Threading.Tasks;
using Isoplane;
using Isoplane.Durable;

namespace Alpha;

/// <summary>Answers after one host timer, so each request parks once.</summary>
public sealed class Handler : IIsoplaneHandler
{
    private static bool libraryLoadedDuringInitialization;
    /// <summary>The package's declared factory; warms the response path before template capture.</summary>
    public static IIsoplaneHandler Create()
    {
        Initialization.Initializing += () =>
        {
            libraryLoadedDuringInitialization = AppDomain.CurrentDomain.GetAssemblies()
                .Any(assembly => assembly.GetName().Name == "Beta.Library");
            GC.KeepAlive(Answer("/warmup"));
            return Task.CompletedTask;
        };
        return new Handler();
    }

    /// <summary>Answers <c>alpha:</c> and the request path.</summary>
    public async Task<Response> HandleAsync(Request req, Ctx ctx)
    {
        if (req.Path == "/load-library")
        {
            Assembly library = Assembly.Load("Beta.Library");
            await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
            return Response.Text($"initialized={libraryLoadedDuringInitialization};loaded={library.GetName().Name}");
        }
        await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
        return Answer(req.Path);
    }

    private static Response Answer(string path) => Response.Text("alpha:" + path);

    /// <summary>A public static method of the wrong shape to be named as a factory.</summary>
    public static string Describe() => "not a handler";
}
