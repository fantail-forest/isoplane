using System;
using System.Threading.Tasks;
using Microsoft.AspNetCore.Builder;

namespace Debugger.Features;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/features", Handle);

    private static bool ReleasePause = false;

    private sealed class DebugSample
    {
        public int Value = 7;
    }

    private static async Task<string> Handle()
    {
        int answer = 40;
        int[] numbers = [2, 4, 6];
        var sample = new DebugSample();
        for (int index = 0; index < 3; index++)
        {
            answer += 1;
            int logged = answer;
        }
        int inspected = answer;
        int stepped = Compute(inspected);
        await Task.Delay(100);
        stepped = PauseForEditor(stepped);
        try { ThrowForDebugger(); }
        catch (InvalidOperationException) { }
        return $"{answer}:{numbers[1]}:{sample.Value}:{stepped}";
    }

    private static int Compute(int input)
    {
        int doubled = input * 2;
        return doubled;
    }

    private static void ThrowForDebugger()
    {
        throw new InvalidOperationException("debug-exception");
    }

    private static int PauseForEditor(int answer)
    {
        int iteration = 0;
        while (!ReleasePause)
        {
            iteration += 1;
        }
        return answer;
    }
}
