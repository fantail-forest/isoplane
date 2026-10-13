using System;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
using Isoplane;

namespace Native;

/// <summary>
/// Attempts one native transition per request path, before and after a host
/// timer, and answers the outcome of each: the exception type, or <c>called</c>.
/// </summary>
public static class Handler
{
    /// <summary>Answers <c>before/after</c> for the transition the path names.</summary>
    [HttpHandler]
    public static async Task<Response> HandleAsync(Request req)
    {
        string before = Attempt(req.Path);
        await Task.Delay(TimeSpan.FromMilliseconds(50)).ConfigureAwait(false);
        return Response.Text(before + "/" + Attempt(req.Path));
    }

    /// <summary>
    /// Each transition lives in its own method: the interpreter compiles a
    /// method whole, so a shared body would resolve every declaration at once.
    /// </summary>
    private static string Attempt(string path)
    {
        try
        {
            switch (path)
            {
                case "/declared": CallDeclared(); break;
                case "/mismatched": CallMismatched(); break;
                case "/unbridged": CallUnbridged(); break;
                case "/internal-call": CallInternal(); break;
                case "/callback-pointer": TakeCallbackPointer(); break;
                case "/delegate-pointer": TakeDelegatePointer(); break;
                default: throw new ArgumentException("unknown native probe");
            }
            return "called";
        }
        catch (Exception error) when (error is not ArgumentException)
        {
            return error.GetType().Name;
        }
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void CallDeclared() => GC.KeepAlive(Timestamp());

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void CallMismatched() => GC.KeepAlive(TimestampWithArgument(1));

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void CallUnbridged() => GC.KeepAlive(TimestampWithDoubles(1, 2, 3, 4, 5, 6, 7, 8, 9));

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void CallInternal() => GC.KeepAlive(MissingInternalCall());

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static unsafe void TakeCallbackPointer() => GC.KeepAlive((nint)(delegate* unmanaged<int, int>)&Twice);

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void TakeDelegatePointer() => GC.KeepAlive(Marshal.GetFunctionPointerForDelegate<Transform>(Triple));

    /// <summary>A delegate an application might hand to native code.</summary>
    public delegate int Transform(int value);

    [UnmanagedCallersOnly]
    private static int Twice(int value) => value * 2;

    private static int Triple(int value) => value * 3;


    [DllImport("libSystem.Native", EntryPoint = "SystemNative_GetTimestamp")]
    private static extern long Timestamp();

    [DllImport("libSystem.Native", EntryPoint = "SystemNative_GetTimestamp")]
    private static extern int TimestampWithArgument(int value);

    [DllImport("libSystem.Native", EntryPoint = "SystemNative_GetTimestamp")]
    private static extern double TimestampWithDoubles(double a, double b, double c, double d, double e, double f, double g, double h, double i);

    [MethodImpl(MethodImplOptions.InternalCall)]
    private static extern int MissingInternalCall();
}
