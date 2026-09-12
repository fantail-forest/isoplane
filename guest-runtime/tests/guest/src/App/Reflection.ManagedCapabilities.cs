using System;
using System.Collections.Generic;
using System.Diagnostics.CodeAnalysis;
using System.Diagnostics.Tracing;
using System.Dynamic;
using System.Reflection;
using System.Threading.Tasks;

namespace Acceptance;

internal sealed partial class Reflection
{
#if WISP_MONO
    private static async Task<string> LoadedAssemblyAfterPark()
    {
        using var stream = typeof(Reflection).Assembly.GetManifestResourceStream("dynamic-payload")
            ?? throw new InvalidOperationException("embedded dynamic payload is missing");
        byte[] bytes = new byte[checked((int)stream.Length)];
        stream.ReadExactly(bytes);
        Assembly assembly = Assembly.Load(bytes);
        MethodInfo method = assembly.GetType("DynamicPayload.Operations", throwOnError: true)!
            .GetMethod("AddOne") ?? throw new MissingMethodException("payload AddOne");
        int before = (int)method.Invoke(null, new object[] { 41 })!;
        await Task.Delay(50).ConfigureAwait(false);
        return before + "/" + method.Invoke(null, new object[] { 42 });
    }

    [DynamicDependency(DynamicallyAccessedMemberTypes.PublicMethods, typeof(DynamicModel))]
    private static async Task<string> DynamicBindingAfterPark(string operation)
    {
        dynamic target = operation == "expando" ? new ExpandoObject() : new DynamicModel();
        int Bind(int value)
        {
            switch (operation)
            {
                case "member":
                    return (int)target.AddOne(value);
                case "expando":
                    target.Value = value;
                    return (int)(target.Value + 1);
                default:
                    throw new ArgumentException("unknown dynamic operation " + operation);
            }
        }
        int before = Bind(41);
        await Task.Delay(50).ConfigureAwait(false);
        return before + "/" + Bind(42);
    }

    private sealed class DynamicModel
    {
        public int AddOne(int value) => value + 1;
    }
#endif

    private static async Task<string> EventsAfterPark()
    {
        using var source = new CapabilityEvents();
        using var listener = new CapabilityListener(source, EventLevel.Informational);
        source.Value(42);
        await Task.Delay(50).ConfigureAwait(false);
        source.Value(43);
        listener.Dispose();
        source.Value(44);
        return string.Join(",", listener.Values);
    }

    [EventSource(Name = "Wisp-Acceptance-Capabilities")]
    private sealed class CapabilityEvents : EventSource
    {
        [Event(1, Level = EventLevel.Informational)]
        public void Value(int value) => WriteEvent(1, value);
    }

    private sealed class CapabilityListener : EventListener
    {
        public List<int> Values { get; } = new();

        public CapabilityListener(EventSource source, EventLevel level) => EnableEvents(source, level);

        protected override void OnEventWritten(EventWrittenEventArgs args)
        {
            if (args.EventId == 1 && args.Payload is { Count: 1 } && args.Payload[0] is int value)
                Values.Add(value);
            else
                throw new InvalidOperationException("unexpected diagnostic event payload: " + args.EventId);
        }
    }
}
