using Isoplane.Build;
using Mono.Cecil.Cil;
using Mono.Debugger.Soft;

namespace Isoplane.Debugging;

internal sealed record CallerRemapping(StackFrame Frame, int Version, int Offset, int CallOrdinal, MethodMirror CalledMethod)
{
    internal static CallerRemapping? Capture(StackFrame frame)
    {
        int offset = frame.ReturnOffset;
        if (offset < 0) return null;
        var body = frame.GetMethodBody();
        var call = body.Instructions.SingleOrDefault(instruction => instruction.Next?.Offset == offset);
        if (call is null || !IsCall(call) || call.Operand is not MethodMirror called) return null;
        var location = frame.GetMethodLocations().LastOrDefault(location => location.ILOffset <= call.Offset);
        if (location is null) return null;
        int ordinal = body.Instructions.Count(instruction => instruction.Offset >= location.ILOffset && instruction.Offset < call.Offset && IsCall(instruction));
        return new(frame, frame.MethodVersion, frame.ILOffset, ordinal, called);
    }

    internal void Apply(ModuleUpdate[] updates)
    {
        if (Frame.IsCurrentMethodVersion) return;
        if (new ActiveStatementRemapping(Frame, Version, Offset).Resolve(updates) is not { } targetOffset) return;
        int end = Frame.Method.Locations.Where(location => location.ILOffset > targetOffset)
            .Select(location => location.ILOffset).DefaultIfEmpty(int.MaxValue).Min();
        var call = Frame.Method.GetMethodBody().Instructions
            .Where(instruction => instruction.Offset >= targetOffset && instruction.Offset < end && IsCall(instruction))
            .Skip(CallOrdinal).FirstOrDefault();
        if (call?.Operand is not MethodMirror called || called != CalledMethod || call.Next is null)
            throw new InvalidOperationException("The updated active statement no longer contains the caller's outstanding call.");
        Frame.DeferRemap(call.Next.Offset);
    }

    private static bool IsCall(ILInstruction instruction) => instruction.OpCode.Code is Code.Call or Code.Callvirt;
}
