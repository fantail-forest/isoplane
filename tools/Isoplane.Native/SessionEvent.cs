using System.Text.Json;
using System.Text.Json.Nodes;
using System.Text.Json.Serialization;
using Isoplane.Native.Interop;

namespace Isoplane.Native;

/// <summary>
/// An event on a development session's stream: reported by the native host, or raised by the tool that owns
/// the session. <c>isoplane dev --json</c> writes each as a JSON object that names it in <c>event</c>.
/// </summary>
[JsonPolymorphic(TypeDiscriminatorPropertyName = "event")]
[JsonDerivedType(typeof(Ready), "ready")]
[JsonDerivedType(typeof(Reloaded), "reloaded")]
[JsonDerivedType(typeof(DebuggerDetached), "debugger_detached")]
[JsonDerivedType(typeof(DebuggerDetachPrepared), "debugger_detach_prepared")]
[JsonDerivedType(typeof(DebuggerDetachFailed), "debugger_detach_failed")]
[JsonDerivedType(typeof(ReloadFailed), "reload_failed")]
[JsonDerivedType(typeof(ApplicationLog), "application_log")]
[JsonDerivedType(typeof(Diagnostic), "diagnostic")]
[JsonDerivedType(typeof(ReloadPending), "reload_pending")]
[JsonDerivedType(typeof(BuildFailed), "build_failed")]
[JsonDerivedType(typeof(DebuggerReady), "debugger_ready")]
internal abstract record SessionEvent
{
    private static readonly JsonSerializerOptions Json = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        RespectRequiredConstructorParameters = true,
        RespectNullableAnnotations = true,
        AllowOutOfOrderMetadataProperties = true,
    };

    /// <summary>The event as <c>isoplane dev --json</c> writes it.</summary>
    internal JsonObject ToJson() => JsonSerializer.SerializeToNode(this, Json)!.AsObject();

    /// <summary>
    /// Reads an event <see cref="ToJson"/> wrote. Throws <see cref="JsonException"/> when <paramref name="written"/>
    /// names an event this type does not declare or lacks one of its event's members.
    /// </summary>
    internal static SessionEvent Parse(JsonObject written) =>
        written.Deserialize<SessionEvent>(Json) ?? throw new JsonException("A session event is an object.");

    /// <summary>The event the native host reported, copied out of the memory it lends for the callback.</summary>
    internal static unsafe SessionEvent Read(NativeEvent* reported)
    {
        var details = &reported->details;
        return reported->kind switch
        {
            NativeEventKind.Ready => new Ready(NativeRuntime.Text(details->serving.app), NativeRuntime.Text(details->serving.url)),
            NativeEventKind.Reloaded => new Reloaded(NativeRuntime.Text(details->serving.app), NativeRuntime.Text(details->serving.url)),
            NativeEventKind.DebuggerDetached => new DebuggerDetached(NativeRuntime.Text(details->serving.app), NativeRuntime.Text(details->serving.url)),
            NativeEventKind.DebuggerDetachPrepared => new DebuggerDetachPrepared(),
            NativeEventKind.DebuggerDetachFailed => new DebuggerDetachFailed(NativeRuntime.Text(details->text)),
            NativeEventKind.ReloadFailed => new ReloadFailed(NativeRuntime.Text(details->text)),
            NativeEventKind.ApplicationLog => new ApplicationLog(details->application_log.level, NativeRuntime.Text(details->application_log.fields)),
            NativeEventKind.Diagnostic => new Diagnostic(NativeRuntime.Text(details->text)),
            var kind => throw new System.Diagnostics.UnreachableException($"The native host reported the event kind {kind}, which its bindings do not declare."),
        };
    }

    /// <summary>The application serves requests at <paramref name="Url"/>.</summary>
    internal sealed record Ready(string App, string Url) : SessionEvent;

    /// <summary>A reloaded application replaced the one serving at <paramref name="Url"/>.</summary>
    internal sealed record Reloaded(string App, string Url) : SessionEvent;

    /// <summary>The application left its debugger and continues at <paramref name="Url"/>.</summary>
    internal sealed record DebuggerDetached(string App, string Url) : SessionEvent;

    /// <summary>The application that will continue without the debugger is prepared. <see cref="LocalSession"/> consumes it.</summary>
    internal sealed record DebuggerDetachPrepared : SessionEvent;

    /// <summary>The application that would continue without the debugger could not be prepared. <see cref="LocalSession"/> consumes it.</summary>
    internal sealed record DebuggerDetachFailed(string Detail) : SessionEvent;

    /// <summary>A reload could not be prepared or published; the previous application keeps serving.</summary>
    internal sealed record ReloadFailed(string Detail) : SessionEvent;

    /// <summary>
    /// A line the application logged: its text, <paramref name="Fields"/>, at the severity it gave it,
    /// <paramref name="Level"/>, from 0 for trace to 4 for error.
    /// </summary>
    internal sealed record ApplicationLog(ulong Level, string Fields) : SessionEvent;

    /// <summary>A host log record the session's filter admitted, as one line of <paramref name="Text"/>.</summary>
    internal sealed record Diagnostic(string Text) : SessionEvent;

    /// <summary>Raised by the session's owner: a rebuilt application was published and replaces the running one once its current request completes.</summary>
    internal sealed record ReloadPending : SessionEvent;

    /// <summary>Raised by the session's owner: a rebuild failed, and the last working application keeps serving.</summary>
    internal sealed record BuildFailed(string Detail) : SessionEvent;

    /// <summary>
    /// Raised by the session's owner: a debug adapter reaches the session at <paramref name="Endpoint"/>, which
    /// runs <paramref name="Package"/> on the runtime <paramref name="Module"/>.
    /// </summary>
    internal sealed record DebuggerReady(string Endpoint, string Module, string Package, string Runtime) : SessionEvent;
}
