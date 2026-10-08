using System.Reflection;
using System.Runtime.Loader;
using System.Text;
using Isoplane.Build;

namespace Isoplane.HotReload;

internal static class Program
{
    private static async Task<int> Main()
    {
        Console.InputEncoding = new UTF8Encoding(false);
        Console.OutputEncoding = new UTF8Encoding(false);
        try
        {
            var start = await HotReloadProtocol.ReadAsync<CompilerStart>(Console.In)
                ?? throw new IOException("The debugger closed before starting the Hot Reload compiler.");
            string tools = Path.Combine(start.SdkDirectory, "DotnetTools", "dotnet-format");
            var resolver = new AssemblyDependencyResolver(Path.Combine(tools, "dotnet-format.dll"));
            AssemblyLoadContext.Default.Resolving += (context, name) =>
            {
                string? path = resolver.ResolveAssemblyToPath(name);
                if (path is null)
                {
                    string candidate = Path.Combine(tools, name.Name + ".dll");
                    if (File.Exists(candidate)) path = candidate;
                }
                return path is null ? null : context.LoadFromAssemblyPath(path);
            };
            return await CompilerServer.RunAsync(start);
        }
        catch (Exception error)
        {
            await HotReloadProtocol.WriteAsync(Console.Out, new CompilerReply([], [], false, error.GetBaseException().Message));
            await Console.Error.WriteLineAsync(error.ToString());
            return 1;
        }
    }
}
