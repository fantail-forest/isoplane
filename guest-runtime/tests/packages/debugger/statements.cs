using System;
using System.Threading.Tasks;
using Microsoft.AspNetCore.Builder;

namespace Debugger.Statements;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/statements", Handle);

    private static bool finished;

    private static string Handle()
    {
        int result = Protected(Compute());
        return finished ? result.ToString() : "missing cleanup";
    }

    private static int Protected(int answer)
    {
        try
        {
            int saved = answer;
            saved += 100;
            return saved;
        }
        finally { finished = true; }
    }

    private static int Compute()
    {
        int answer = 1;
        answer += 10;
        answer *= 2;
        return answer;
    }
}
