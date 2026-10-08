using System.Threading.Tasks;
using Microsoft.AspNetCore.Builder;

namespace Debugger.StatementAwaits;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/statement-awaits", Handle);

    private static async Task<string> Handle()
    {
        int value = 11;
        int completed = 0;
        value += 1;
        await Task.Delay(1);
        completed++;
        value += 10;
        return $"{value}:{completed}";
    }
}
