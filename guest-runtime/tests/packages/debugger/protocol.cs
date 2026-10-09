using System;
using System.Threading.Tasks;
using Microsoft.AspNetCore.Builder;

namespace Debugger.Protocol;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/protocol", Handle);

    private static bool ReleaseProtocolProbe = false;
    private static bool ReleaseEvaluationProbe = false;

    private static int EvaluationProbe()
    {
        while (!ReleaseEvaluationProbe) { }
        return 42;
    }

    private sealed class ProtocolSample
    {
        public int Value = 7;
        public ProtocolNested Nested = new() { Leaf = new() { Value = 3, Name = "before" } };
        public static int StaticValue = 0;
        public const int Constant = 12;
        [ThreadStatic] public static int ThreadValue;
    }

    private struct ProtocolNested
    {
        public ProtocolLeaf Leaf;
    }

    private struct ProtocolLeaf
    {
        public int Value;
        public string Name;
    }

    private static string Handle()
    {
        var sample = new ProtocolSample();
        int[] numbers = [2, 4];
        object other = new object();
        ProtocolSample.ThreadValue = 1;
        System.Diagnostics.Debugger.Break();
        sample.Value = 9;
        GC.Collect();
        sample.Nested.Leaf.Value = 11;
        sample.Nested.Leaf.Name = "after";
        while (!ReleaseProtocolProbe) { }
        GC.KeepAlive(numbers);
        GC.KeepAlive(other);
        GC.KeepAlive(typeof(ProtocolSample).GetFields());
        return sample.Value.ToString();
    }
}
