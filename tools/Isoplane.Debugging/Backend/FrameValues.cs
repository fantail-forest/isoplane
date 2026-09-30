namespace Mono.Debugging.Client
{
    /// <summary>Connects manually evaluated values to the debugger's child inspection and update machinery.</summary>
    public static class FrameValues
    {
        /// <summary>Associates an unbound value with a session frame while retaining its evaluation source.</summary>
        public static void Attach(StackFrame frame, ObjectValue value)
        {
            if (value.DebuggerSession == null)
                ObjectValue.ConnectCallbacks(frame, value);
        }
    }
}
