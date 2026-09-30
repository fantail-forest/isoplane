#nullable enable
using System;
using System.Collections.Generic;

namespace Mono.Debugger.Soft;

/// <summary>A managed storage location whose changes stop the interpreter.</summary>
/// <param name="Id">The editor's identifier for the breakpoint.</param>
public abstract record ManagedDataBreakpoint(int Id);

/// <summary>Watches an instance field, or a static field when the receiver is null.</summary>
/// <param name="Id">The editor's identifier for the breakpoint.</param>
/// <param name="Receiver">The owning object, or null for a static field.</param>
/// <param name="Field">The field to observe.</param>
public sealed record ManagedFieldBreakpoint(int Id, ObjectMirror? Receiver, FieldInfoMirror Field) : ManagedDataBreakpoint(Id);

/// <summary>Watches one flattened element of a managed array.</summary>
/// <param name="Id">The editor's identifier for the breakpoint.</param>
/// <param name="Array">The owning array.</param>
/// <param name="Index">The zero-based flattened element offset.</param>
public sealed record ManagedArrayBreakpoint(int Id, ArrayMirror Array, int Index) : ManagedDataBreakpoint(Id);

/// <summary>Watches a local until its lexical scope or interpreter invocation ends.</summary>
/// <param name="Id">The editor's identifier for the breakpoint.</param>
/// <param name="Frame">The paused invocation owning the local.</param>
/// <param name="Variable">The local storage to observe.</param>
public sealed record ManagedLocalBreakpoint(int Id, StackFrame Frame, LocalVariable Variable) : ManagedDataBreakpoint(Id);

/// <summary>Changes and expired locations consumed from the current runtime stop.</summary>
/// <param name="Hits">Locations whose values changed.</param>
/// <param name="Expired">Locals whose scope or invocation ended.</param>
public sealed record ManagedDataBreakpointEvents(int[] Hits, int[] Expired);

public partial class VirtualMachine
{
    /// <summary>Replaces the session's managed data breakpoints atomically. The guest must be paused.</summary>
    public void SetManagedDataBreakpoints(IReadOnlyList<ManagedDataBreakpoint> breakpoints) => conn.SetManagedDataBreakpoints(breakpoints);

    /// <summary>Consumes the changed and expired data breakpoint identifiers at the current stop.</summary>
    public ManagedDataBreakpointEvents TakeManagedDataBreakpointHits() => conn.TakeManagedDataBreakpointHits();
}

public abstract partial class Connection
{
    internal const int ManagedDataBreakpointCommand = 24;
    internal const byte ManagedDataBreakpointSet = 1;
    internal const byte ManagedDataBreakpointTakeHits = 2;
    internal const byte ManagedDataBreakpointField = 1;
    internal const byte ManagedDataBreakpointArray = 2;
    internal const byte ManagedDataBreakpointLocal = 3;

    internal void SetManagedDataBreakpoints(IReadOnlyList<ManagedDataBreakpoint> breakpoints)
    {
        var packet = new PacketWriter(this).WriteByte(ManagedDataBreakpointSet).WriteInt(breakpoints.Count);
        foreach (var breakpoint in breakpoints)
        {
            packet.WriteInt(breakpoint.Id);
            switch (breakpoint)
            {
                case ManagedFieldBreakpoint field:
                    packet.WriteByte(ManagedDataBreakpointField).WriteId(field.Receiver?.Id ?? 0).WriteId(field.Field.Id);
                    break;
                case ManagedArrayBreakpoint array:
                    packet.WriteByte(ManagedDataBreakpointArray).WriteId(array.Array.Id).WriteInt(array.Index);
                    break;
                case ManagedLocalBreakpoint local:
                    packet.WriteByte(ManagedDataBreakpointLocal).WriteId(local.Frame.Thread.Id).WriteId(local.Frame.Id).WriteInt(local.Variable.GetValueIndex);
                    break;
                default:
                    throw new ArgumentException("Unsupported managed storage location.", nameof(breakpoints));
            }
        }
        SendReceive(CommandSet.VM, ManagedDataBreakpointCommand, packet);
    }

    internal ManagedDataBreakpointEvents TakeManagedDataBreakpointHits()
    {
        var reply = SendReceive(CommandSet.VM, ManagedDataBreakpointCommand, new PacketWriter(this).WriteByte(ManagedDataBreakpointTakeHits));
        var result = new int[reply.ReadInt()];
        for (int index = 0; index < result.Length; index++) result[index] = reply.ReadInt();
        var expired = new int[reply.ReadInt()];
        for (int index = 0; index < expired.Length; index++) expired[index] = reply.ReadInt();
        return new ManagedDataBreakpointEvents(result, expired);
    }
}
