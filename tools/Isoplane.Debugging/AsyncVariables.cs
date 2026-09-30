using Mono.Debugger.Soft;
using Mono.Debugging.Client;
using Mono.Debugging.Evaluation;
using Mono.Debugging.Soft;

namespace Isoplane.Debugging;

internal sealed class AsyncVariables(FieldValueReference storage, TypeMirror stateType, TypeMirror declaringType) : SoftDebuggerAdaptor
{
    protected override IEnumerable<ValueReference> OnGetLocalVariables(EvaluationContext context) => Fields(context, parameters: false);
    protected override IEnumerable<ValueReference> OnGetParameters(EvaluationContext context) => Fields(context, parameters: true);

    protected override ValueReference? OnGetLocalVariable(EvaluationContext context, string name)
    {
        var comparison = context.CaseSensitive ? StringComparison.Ordinal : StringComparison.OrdinalIgnoreCase;
        var matches = OnGetLocalVariables(context).Where(value => string.Equals(value.Name, name, comparison)).ToArray();
        return matches.Length switch
        {
            0 => null,
            1 => matches[0],
            _ => throw new EvaluatorException("The saved local '{0}' is ambiguous in this async frame.", name)
        };
    }

    protected override ValueReference? OnGetThisReference(EvaluationContext context)
    {
        var field = stateType.GetField("<>4__this");
        return field is null ? null : Reference(context, field, "this");
    }

    /// <summary>Resolves static members in the suspended method's containing type.</summary>
    public override object GetEnclosingType(EvaluationContext context) => declaringType;

    private IEnumerable<ValueReference> Fields(EvaluationContext context, bool parameters) => stateType.GetFields()
        .Where(field => !field.IsStatic && field.Name.StartsWith('<') != parameters)
        .Select(field => (Field: field, Name: LocalName(field.Name)))
        .Where(field => field.Name is not null)
        .Select(field => Reference(context, field.Field, field.Name!));

    private FieldValueReference Reference(EvaluationContext context, FieldInfoMirror field, string name) =>
        new(context, field, storage.Value, stateType, name, ObjectValueFlags.Variable) { ParentSource = storage };

    private static string? LocalName(string name)
    {
        if (name == "<>4__this") return "this";
        if (!name.StartsWith('<')) return name;
        int end = name.IndexOf(">5__", StringComparison.Ordinal);
        return end > 1 ? name[1..end] : null;
    }
}
