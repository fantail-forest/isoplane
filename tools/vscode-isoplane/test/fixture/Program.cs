using Isoplane;
using Isoplane.AspNetCore;

// The editor tests' application. It takes the place of Program.cs in a project created
// from the isoplane-web template, so the project file and launch settings are the customer's.
var builder = IsoplaneHosting.CreateBuilder(args);
var app = builder.Build();

app.MapGet("/", EditorDebug.Application.Handle);
app.MapGet("/count", EditorDebug.Application.Count);
app.MapGet("/hotreload", EditorDebug.Application.Running);
app.MapGet("/warmup", () => "warm");

Template.Initializing += () => app.WarmupAsync("/warmup");

await app.RunAsync();

namespace EditorDebug
{
    internal static class Application
    {
        internal static string Handle()
        {
            int answer = 42;
            return answer.ToString();
        }

        internal static string Running()
        {
            System.Diagnostics.Debugger.Break();
            int revision;
            do { revision = Revision(); } while (revision == 0);
            return revision.ToString();
        }

        [System.Runtime.CompilerServices.MethodImpl(System.Runtime.CompilerServices.MethodImplOptions.NoInlining)]
        private static int Revision() => 0;

        internal static string Count()
        {
            int[] values = [1, 2, 3];
            return values.Count(value => value > 2).ToString();
        }
    }
}
