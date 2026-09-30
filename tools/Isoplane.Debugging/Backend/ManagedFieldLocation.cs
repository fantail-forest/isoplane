using System;
using System.Linq;
using Mono.Debugger.Soft;

namespace Mono.Debugging.Soft;

public partial class FieldValueReference
{
    /// <summary>Resolves this field to a storage location retained by the guest debugger.</summary>
    /// <exception cref="NotSupportedException">The field is constant or belongs to a temporary value-type copy.</exception>
    public ManagedDataBreakpoint GetManagedDataBreakpoint(int id)
    {
        if (field.IsLiteral) throw new NotSupportedException("A constant does not have writable managed storage.");
        if (field.IsStatic) return new ManagedFieldBreakpoint(id, null, field);
        if (obj is ObjectMirror receiver) return new ManagedFieldBreakpoint(id, receiver, field);
        if (obj is StructMirror && ParentSource is FieldValueReference parent)
            return parent.GetManagedDataBreakpoint(id) switch
            {
                ManagedFieldBreakpoint root => new ManagedFieldPathBreakpoint(id, root, new[] { field }),
                ManagedFieldPathBreakpoint path => path with { Fields = path.Fields.Append(field).ToArray() },
                _ => throw new NotSupportedException("The value-type field has no retained object storage.")
            };
        throw new NotSupportedException("Select a field on a managed object, rather than a temporary value-type copy.");
    }
}
