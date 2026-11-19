using System;
using System.Reflection;
using System.Reflection.Emit;
using System.Runtime.CompilerServices;
using System.Threading.Tasks;
using Isoplane;

namespace Diagnostics;

/// <summary>
/// Each path makes the Mono runtime report through its own logger, writes to
/// its native console, or asks it for more memory than a limited instance may
/// have.
/// </summary>
public static class Handler
{
    /// <summary>Runs the report the request path names.</summary>
    [HttpHandler]
    public static Task<Response> HandleAsync(Request req)
    {
        switch (req.Path)
        {
            case "/fail-fast":
                FailFast();
                break;
            case "/invalid-il":
                RunInvalidIl();
                break;
            case "/native-console":
                WriteToNativeConsole();
                break;
            case "/allocate":
                return Task.FromResult(Response.Text(Allocate()));
        }
        return Task.FromResult(Response.Text("survived:" + req.Path));
    }

    /// <summary>
    /// Allocates 40 MiB, which fits within the runtime's own memory maximum but
    /// not within a limit a little above what the prepared application uses.
    /// </summary>
    [MethodImpl(MethodImplOptions.NoInlining)]
    private static string Allocate()
    {
        try
        {
            var block = new byte[40 << 20];
            block[^1] = 1;
            GC.KeepAlive(block);
            return "allocated";
        }
        catch (OutOfMemoryException)
        {
            return "refused:OutOfMemoryException";
        }
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void FailFast() => Environment.FailFast("the diagnostics package gave up");

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static void WriteToNativeConsole()
    {
        // The runtime library's own console writer, which its reference
        // assembly leaves out.
        Type console = typeof(object).Assembly.GetType("Internal.Console", throwOnError: true)!;
        Action<string> output = WriterOf(console);
        Action<string> error = WriterOf(console.GetNestedType("Error")
            ?? throw new MissingMemberException("Internal.Console", "Error"));
        output("standard\0output\n");
        error("standard\0error\n\n");
        output("");
        output("\n");
        output("end");
    }

    private static Action<string> WriterOf(Type console)
    {
        MethodInfo write = console.GetMethod("Write", [typeof(string)])
            ?? throw new MissingMethodException(console.FullName, "Write");
        return write.CreateDelegate<Action<string>>();
    }

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
