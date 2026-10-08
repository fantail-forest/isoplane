#nullable enable
using System;
using System.Collections.Generic;
using System.Linq;

namespace Mono.Debugger.Soft;

public abstract partial class Connection
{
    internal int FrameMethodVersion(long thread, long frame) =>
        SendReceive(CommandSet.STACK_FRAME, 12, new PacketWriter(this).WriteId(thread).WriteId(frame)).ReadInt();
}

public partial class StackFrame
{
    private int? methodVersion;

    /// <summary>The method generation executing in this invocation, including after newer code is loaded.</summary>
    public int MethodVersion => methodVersion ??= method.DeclaringType.Assembly.ManifestModule.HasAppliedChanges
        ? vm.conn.FrameMethodVersion(thread.Id, Id) : 1;

    /// <summary>The IL body executing in this invocation.</summary>
    public MethodBodyMirror GetMethodBody() => MethodVersion == method.CurrentVersion
        ? method.GetMethodBody() : method.PreviousVersion(MethodVersion).Body;

    /// <summary>Whether this invocation executes the latest method generation.</summary>
    public bool IsCurrentMethodVersion => MethodVersion == method.CurrentVersion;

    /// <summary>The parameters and locals described by this invocation's method generation.</summary>
    public LocalVariable[] GetLocals() => MethodVersion == method.CurrentVersion
        ? method.GetLocals() : method.PreviousVersion(MethodVersion).Locals ?? throw new AbsentInformationException();

    private string? FrameSource => IsCurrentMethodVersion ? method.SourceFile : method.PreviousVersion(MethodVersion).Source;

    private int MapFrameOffset(int offset, out string? source, out byte[]? hash, out int column, out int endLine, out int endColumn)
    {
        if (MethodVersion == method.CurrentVersion)
            return method.il_offset_to_line_number(offset, out source, out hash, out column, out endLine, out endColumn);
        var snapshot = method.PreviousVersion(MethodVersion);
        var point = snapshot.Locations.LastOrDefault(location => location.ILOffset <= offset);
        source = point?.SourceFile ?? snapshot.Source;
        hash = point?.SourceFileHash;
        column = point?.ColumnNumber ?? 0;
        endLine = point?.EndLineNumber ?? -1;
        endColumn = point?.EndColumnNumber ?? -1;
        return point?.LineNumber ?? -1;
    }
}

public partial class MethodMirror
{
    internal sealed record DebugVersion(MethodBodyMirror Body, LocalVariable[]? Locals, Location[] Locations, string? Source);
    private readonly Dictionary<int, DebugVersion> previousVersions = new();
    internal int CurrentVersion { get; private set; } = 1;

    internal DebugVersion PreviousVersion(int version) => previousVersions.TryGetValue(version, out var snapshot)
        ? snapshot : throw new InvalidOperationException($"Debug information for active version {version} of '{FullName}' was not retained.");

    internal void PreserveVersion(int version)
    {
        if (previousVersions.ContainsKey(version)) return;
        if (version != CurrentVersion)
            throw new InvalidOperationException($"Cannot capture old version {version} of '{FullName}' from its current metadata.");
        LocalVariable[]? savedLocals;
        try { savedLocals = GetLocals(); }
        catch (AbsentInformationException) { savedLocals = null; }
        var savedLocations = Locations.Select(point => new Location(vm, this, 0, point.ILOffset, point.SourceFile,
            point.LineNumber, point.ColumnNumber, point.EndLineNumber, point.EndColumnNumber, point.SourceFileHash)).ToArray();
        previousVersions.Add(version, new DebugVersion(GetMethodBody(), savedLocals, savedLocations, SourceFile));
    }
}
