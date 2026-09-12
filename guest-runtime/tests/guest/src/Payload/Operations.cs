namespace DynamicPayload;

/// <summary>A separately compiled payload for guest assembly-loading tests.</summary>
public static class Operations
{
    /// <summary>Returns the next integer to verify reflective invocation.</summary>
    public static int AddOne(int value) => value + 1;
}
