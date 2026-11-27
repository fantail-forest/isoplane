using System.Diagnostics;

namespace DebugFixture;

/// <summary>The code the debugger tests pause in. Their driver finds lines by their text.</summary>
internal static class Application
{
    internal static async Task<string> Handle(bool detached)
    {
        int answer = 42;
        Task delay = Task.Delay(100);
        if (delay.IsCompleted) throw new InvalidOperationException("host timer must be incomplete before awaiting");
        await delay;
        bool resumed = true;
        return resumed && !(detached && Debugger.IsAttached) ? answer.ToString() : throw new InvalidOperationException("continuation did not run");
    }

    internal static string Library() => Array.ConvertAll(new[] { 41 }, FixtureLibrary.Calculator.Work)[0].ToString();
}
