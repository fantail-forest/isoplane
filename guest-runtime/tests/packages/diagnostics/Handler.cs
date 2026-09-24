using System;
using System.Reflection.Emit;
using System.Runtime.CompilerServices;
using System.Threading.Tasks;
using Isoplane;
using Isoplane.Durable;

namespace Diagnostics;

/// <summary>Each path makes the Mono runtime report through its own logger.</summary>
public sealed class Handler : IIsoplaneHandler
{
    /// <summary>The package's declared factory.</summary>
    public static IIsoplaneHandler Create() => new Handler();

    /// <summary>Runs the report the request path names.</summary>
    public Task<Response> HandleAsync(Request req, Ctx ctx)
    {
        switch (req.Path)
        {
            case "/fail-fast":
                FailFast();
                break;
            case "/invalid-il":
                RunInvalidIl();
                break;
        }
        return Task.FromResult(Response.Text("survived:" + req.Path));
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void FailFast() => Environment.FailFast("the diagnostics package gave up");

    /// <summary>
    /// Stores a double into an int local, IL the interpreter reports as a
    /// fatal error when it compiles the method.
    /// </summary>
    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void RunInvalidIl()
    {
        var method = new DynamicMethod("StoreMismatch", typeof(void), Type.EmptyTypes);
        ILGenerator il = method.GetILGenerator();
        LocalBuilder local = il.DeclareLocal(typeof(int));
        il.Emit(OpCodes.Ldc_R8, 1.5);
        il.Emit(OpCodes.Stloc, local);
        il.Emit(OpCodes.Ret);
        method.CreateDelegate<Action>()();
    }
}
