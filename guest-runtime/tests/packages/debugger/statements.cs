using System;
using System.Runtime.CompilerServices;
using System.Threading;
using Microsoft.AspNetCore.Builder;

namespace Debugger.Statements;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/statements", Handle);

    private static bool finished;

    private static string Handle()
    {
        int result = Protected(Compute());
        int crossed = Crossed();
        var sample = new Sample();
        int synchronized = sample.Run();
        return finished && !Monitor.IsEntered(sample) ? $"{result}:{crossed}:{synchronized}" : "missing cleanup";
    }

    private sealed class Sample
    {
        private readonly int value;

        internal Sample()
        {
            int constructed = 3;
            constructed += 100;
            value = constructed;
        }

        [MethodImpl(MethodImplOptions.Synchronized)]
        internal int Run()
        {
            int synchronized = value;
            synchronized += 100;
            synchronized += 10;
            return Monitor.IsEntered(this) ? synchronized : -1;
        }
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

    private static int Crossed()
    {
        int crossings = 0;
        try
        {
            crossings += 1;
            crossings += 100;
        }
        finally
        {
            crossings += 10;
        }
        crossings += 1000;
        return crossings;
    }
}
