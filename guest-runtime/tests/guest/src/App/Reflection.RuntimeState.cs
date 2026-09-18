using System;
using System.Linq.Expressions;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Threading.Tasks;
#if ISOPLANE_MONO
using System.Reflection.Emit;
#endif

namespace Acceptance;

internal sealed partial class Reflection
{
    private static async Task<string> ExpressionAfterPark(bool preferInterpretation)
    {
        int increment = 2;
        Expression<Func<int, int>> expression = value => value + increment;
        Func<int, int> compiled = expression.Compile(preferInterpretation);
        int before = compiled(40);
        await Task.Delay(50).ConfigureAwait(false);
        increment = 3;
        return before + "/" + compiled(40);
    }

    private static string DisposeSafeHandle()
    {
        var counter = new ReleaseCounter();
        var handle = new CountingHandle(counter);
        handle.Dispose();
        handle.Dispose();
        return counter.Count.ToString();
    }

    private static async Task<string> FinalizeSafeHandleAfterPark()
    {
        var counter = new ReleaseCounter();
        WeakReference handle = CreateUnrootedHandle(counter);
        GC.Collect();
        GC.WaitForPendingFinalizers();
        int before = counter.Count;
        await Task.Delay(50).ConfigureAwait(false);
        GC.Collect();
        GC.WaitForPendingFinalizers();
        GC.KeepAlive(handle);
        return before + "/" + counter.Count;
    }

    [MethodImpl(MethodImplOptions.NoInlining)]
    private static WeakReference CreateUnrootedHandle(ReleaseCounter counter) => new(new CountingHandle(counter));

    private sealed class ReleaseCounter
    {
        public int Count;
    }

    private sealed class CountingHandle : SafeHandle
    {
        private readonly ReleaseCounter _counter;

        public CountingHandle(ReleaseCounter counter) : base(IntPtr.Zero, ownsHandle: true)
        {
            _counter = counter;
            SetHandle(new IntPtr(42));
        }

        public override bool IsInvalid => handle == IntPtr.Zero;

        protected override bool ReleaseHandle()
        {
            _counter.Count++;
            handle = IntPtr.Zero;
            return true;
        }
    }

#if ISOPLANE_MONO
    private static async Task<string> EmittedDelegateAfterPark()
    {
        var method = new DynamicMethod("AddOne", typeof(int), new[] { typeof(int) });
        ILGenerator code = method.GetILGenerator();
        code.Emit(OpCodes.Ldarg_0);
        code.Emit(OpCodes.Ldc_I4_1);
        code.Emit(OpCodes.Add);
        code.Emit(OpCodes.Ret);
        Func<int, int> emitted = method.CreateDelegate<Func<int, int>>();
        int before = emitted(41);
        await Task.Delay(50).ConfigureAwait(false);
        return before + "/" + emitted(42);
    }
#endif
}
