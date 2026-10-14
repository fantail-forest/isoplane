using System;
using System.Collections.Generic;
using System.Globalization;
using System.Reflection;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace Isoplane.Runtime;

/// <summary>
/// Names the timers the runtime holds. Every <see cref="Timer"/> and delay shares the one wake-up
/// timer the host sees, so only the guest can say whose they are.
/// </summary>
internal static class RunningTimers
{
    // A delay passed to a combinator such as Task.WhenAny is awaited by whatever awaits the combinator.
    private const int CombinatorDepth = 8;

    private const uint NoPeriod = unchecked((uint)Timeout.Infinite);

    /// <summary>
    /// A count of the running timers and a line naming each, or null when none is running. Throws
    /// <see cref="MissingMemberException"/> or <see cref="TypeLoadException"/> when the runtime's
    /// timer queue is not shaped as <see cref="Internals"/> reads it.
    /// </summary>
    internal static string? Describe()
    {
        long running = Timer.ActiveCount;
        // Reflection would leave its caches in every template, so it is kept to the failing case.
        if (running == 0) return null;
        var report = new StringBuilder("initialization left ").Append(running).Append(running == 1 ? " timer running:" : " timers running:");
        foreach (string timer in Timers(new Internals())) report.Append("\n    ").Append(timer);
        return report.ToString();
    }

    private static IEnumerable<string> Timers(Internals runtime)
    {
        foreach (object queue in (Array)runtime.Queues.GetValue(null)!)
            foreach (FieldInfo list in runtime.Lists)
                for (object? timer = list.GetValue(queue); timer is not null; timer = runtime.Next.GetValue(timer))
                    yield return DescribeTimer(runtime, (Delegate)runtime.Callback.GetValue(timer)!, runtime.State.GetValue(timer),
                        (uint)runtime.DueTime.GetValue(timer)!, (uint)runtime.Period.GetValue(timer)!);
    }

    private static string DescribeTimer(Internals runtime, Delegate callback, object? state, uint dueTimeMs, uint periodMs)
    {
        // A delay's timer calls back into Task and carries the delay's own task, so the code that
        // asked for it is whatever awaits that task.
        if (state is Task delay && callback.Method.DeclaringType?.DeclaringType == typeof(Task))
        {
            var awaiting = new List<string>();
            AddAwaiters(runtime, delay, awaiting, CombinatorDepth);
            return "a delay of " + Duration(dueTimeMs)
                + (awaiting.Count == 0 ? " that nothing awaits" : " awaited by " + string.Join(" and ", awaiting));
        }
        return (periodMs == NoPeriod ? "a timer set for " + Duration(dueTimeMs) : "a timer every " + Duration(periodMs))
            + " calling " + SourceNames.Method(callback);
    }

    private static void AddAwaiters(Internals runtime, Task awaited, List<string> awaiting, int depth)
    {
        object? continuation = runtime.Continuation.GetValue(awaited);
        foreach (object? single in continuation as List<object?> ?? [continuation])
            AddAwaiter(runtime, single, awaiting, depth);
    }

    private static void AddAwaiter(Internals runtime, object? continuation, List<string> awaiting, int depth)
    {
        // A task that has completed holds a bare object in place of its continuations.
        if (continuation is null || continuation.GetType() == typeof(object)) return;
        int known = awaiting.Count;
        if (SourceNames.AsyncMethod(continuation) is { } method)
            awaiting.Add(method);
        else if (continuation is Delegate resumed)
            awaiting.Add(SourceNames.Method(resumed));
        else if (continuation is Task combinator && depth > 0)
            AddAwaiters(runtime, combinator, awaiting, depth - 1);
        else if (runtime.TaskContinuation.IsInstanceOfType(continuation))
            foreach (Delegate resumes in (Delegate[]?)runtime.ContinuationDelegates.Invoke(continuation, null) ?? [])
                AddAwaiter(runtime, resumes, awaiting, depth);
        if (awaiting.Count == known) awaiting.Add(SourceNames.Type(continuation.GetType()));
    }

    private static string Duration(uint milliseconds) => milliseconds < 1000
        ? milliseconds.ToString(CultureInfo.InvariantCulture) + " ms"
        : (milliseconds / 1000d).ToString("0.###", CultureInfo.InvariantCulture) + " s";

    /// <summary>
    /// The private members of the runtime that hold its timers and a task's continuations. The
    /// runtime publishes neither, so they are found by name, and constructing this throws when
    /// one is missing.
    /// </summary>
    private sealed class Internals
    {
        private const BindingFlags Declared = BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance
            | BindingFlags.Static | BindingFlags.DeclaredOnly;

        internal readonly PropertyInfo Queues;
        internal readonly FieldInfo[] Lists;
        internal readonly FieldInfo Next, Callback, State, DueTime, Period, Continuation;
        internal readonly Type TaskContinuation;
        internal readonly MethodInfo ContinuationDelegates;

        internal Internals()
        {
            Type queue = RuntimeType("System.Threading.TimerQueue"), timer = RuntimeType("System.Threading.TimerQueueTimer");
            Queues = queue.GetProperty("Instances", Declared) ?? throw new MissingMemberException(queue.FullName, "Instances");
            Lists = [Field(queue, "_shortTimers"), Field(queue, "_longTimers")];
            Next = Field(timer, "_next");
            Callback = Field(timer, "_timerCallback");
            State = Field(timer, "_state");
            DueTime = Field(timer, "_dueTime");
            Period = Field(timer, "_period");
            Continuation = Field(typeof(Task), "m_continuationObject");
            TaskContinuation = RuntimeType("System.Threading.Tasks.TaskContinuation");
            // The runtime keeps this method for debuggers, which use it to show what a task resumes.
            ContinuationDelegates = TaskContinuation.GetMethod("GetDelegateContinuationsForDebugger", Declared)
                ?? throw new MissingMethodException(TaskContinuation.FullName, "GetDelegateContinuationsForDebugger");
        }

        private static Type RuntimeType(string name) =>
            typeof(Timer).Assembly.GetType(name) ?? throw new TypeLoadException("the runtime has no type " + name);

        private static FieldInfo Field(Type type, string name) =>
            type.GetField(name, Declared) ?? throw new MissingFieldException(type.FullName, name);
    }
}
