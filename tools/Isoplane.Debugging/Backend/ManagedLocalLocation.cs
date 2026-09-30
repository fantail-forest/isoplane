using System;
using Mono.Debugger.Soft;

namespace Mono.Debugging.Soft;

public partial class VariableValueReference
{
    /// <summary>Resolves a local to storage that expires with its scope or invocation.</summary>
    /// <exception cref="NotSupportedException">Parameters, pointers and by-reference locals are unsupported.</exception>
    public ManagedDataBreakpoint GetManagedDataBreakpoint(int id)
    {
        if (variable.IsArg || variable.Type.IsByRef || variable.Type.IsPointer)
            throw new NotSupportedException("Select an ordinary local, a managed field or an array element.");
        return new ManagedLocalBreakpoint(id, ((SoftEvaluationContext)Context).Frame, variable);
    }
}
