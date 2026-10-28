namespace Isoplane.Cli;

/// <summary>
/// The tools the agent server offers, by name and description. The tool methods take their text from
/// here, and the CLI reference lists them from here, so the two cannot differ.
/// </summary>
internal static class AgentToolCatalogue
{
    internal const string CheckMachine = "check_machine";
    internal const string CheckMachineDescription = "Checks that this machine can build and run Isoplane applications: the .NET SDK, the native host library and the project templates. Returns the ready message, or an error saying what to install or run.";

    internal const string StartSession = "start_session";
    internal const string StartSessionDescription = "Builds an Isoplane application project and runs it in the sandbox on this machine, as `isoplane dev` does. Returns the session's id and the address the application answers at. With watching on, a saved source change rebuilds and reloads it. A build that fails returns its errors and starts nothing.";

    internal const string SessionEvents = "session_events";
    internal const string SessionEventsDescription = "Returns what a session has reported since a given event number: ready and reloaded with the application's address, reload_pending, reload_failed and build_failed with their detail, and diagnostic for each line the application logged. Pass the last number seen to read only newer events; a wait of some seconds returns as soon as one arrives.";

    internal const string StopSession = "stop_session";
    internal const string StopSessionDescription = "Stops a session started by start_session and frees its port.";

    internal const string ListSessions = "list_sessions";
    internal const string ListSessionsDescription = "Lists the sessions started by start_session that are still running, with the project and address of each.";

    internal static readonly (string Name, string Description)[] All =
    [
        (CheckMachine, CheckMachineDescription), (StartSession, StartSessionDescription), (SessionEvents, SessionEventsDescription),
        (StopSession, StopSessionDescription), (ListSessions, ListSessionsDescription),
    ];
}
