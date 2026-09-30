using System;
using System.Threading.Tasks;
using Microsoft.AspNetCore.Builder;

namespace Debugger.Protocol;

internal static class Application
{
    internal static void Map(WebApplication app) => app.MapGet("/protocol", Handle);

    private static bool ReleaseProtocolProbe = false;

    private sealed class ProtocolSample
    {
        public int Value = 7;
        public static int StaticValue = 0;
        public const int Constant = 12;
        [ThreadStatic] public static int ThreadValue;
    }

    private static string Handle()
    {
        var sample = new ProtocolSample();
        int[] numbers = [2, 4];
        object other = new object();
        ProtocolSample.ThreadValue = 1;
        System.Diagnostics.Debugger.Break();
        sample.Value = 9;
        while (!ReleaseProtocolProbe) { }
        GC.KeepAlive(numbers);
        GC.KeepAlive(other);
        GC.KeepAlive(typeof(ProtocolSample).GetFields());
        return sample.Value.ToString();
    }
}
