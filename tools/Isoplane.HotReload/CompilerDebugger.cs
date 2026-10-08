using System.Collections.Immutable;
using System.Reflection;
using Isoplane.Build;

namespace Isoplane.HotReload;

/// <summary>Adapts the paused guest's state to the SDK compiler's internal debugger contract.</summary>
public class CompilerDebugger : DispatchProxy
{
    private Assembly? features;
    private CompilerEdit? state;

    internal void Initialize(Assembly assembly, string[] capabilities)
    {
        features = assembly;
        state = new([], [], capabilities, []);
    }
    internal void SetState(CompilerEdit edit) => Volatile.Write(ref state, edit);

    /// <summary>Supplies active statements, loaded modules, and capabilities for the current edit.</summary>
    protected override object? Invoke(MethodInfo? targetMethod, object?[]? args)
    {
        var method = targetMethod ?? throw new InvalidOperationException("The compiler invoked an unnamed debugger operation.");
        var edit = Volatile.Read(ref state) ?? throw new InvalidOperationException("The compiler requested debugger state before an edit was supplied.");
        if (method.Name == "PrepareModuleForUpdateAsync")
        {
            if (args?[0] is not Guid module || !edit.LoadedModules.Contains(module))
                throw new InvalidOperationException("The compiler requested an update for a module that is not loaded in the paused guest.");
            return ValueTask.CompletedTask;
        }
        var result = method.ReturnType.GetGenericArguments()[0];
        object value = method.Name switch
        {
            "GetCapabilitiesAsync" => edit.Capabilities.ToImmutableArray(),
            "GetActiveStatementsAsync" => ActiveStatements(result, edit.ActiveStatements),
            "GetAvailabilityAsync" => Availability(result, args?[0], edit.LoadedModules),
            _ => throw new NotSupportedException($"The SDK requested unsupported debugger operation '{method.Name}'.")
        };
        return Activator.CreateInstance(method.ReturnType, value);
    }

    private static object Availability(Type result, object? module, Guid[] loaded)
    {
        bool available = module is Guid identity && loaded.Contains(identity);
        var status = result.GetConstructors().Single().GetParameters()[0].ParameterType;
        return Activator.CreateInstance(result, Enum.Parse(status, available ? "Available" : "ModuleNotLoaded"), null)!;
    }

    private object ActiveStatements(Type result, ActiveStatement[] statements)
    {
        var element = result.GetGenericArguments()[0];
        var values = Array.CreateInstance(element, statements.Length);
        for (int index = 0; index < statements.Length; index++)
        {
            var statement = statements[index];
            var method = Create("ManagedMethodId", statement.Module, statement.MethodToken, statement.MethodVersion);
            var instruction = Create("ManagedInstructionId", method, statement.IlOffset);
            var span = Create("SourceSpan", statement.StartLine, statement.StartColumn, statement.EndLine, statement.EndColumn);
            string flags = statement.Leaf ? "LeafFrame" : "NonLeafFrame";
            if (statement.UpToDate) flags += ", MethodUpToDate";
            var kind = ContractType("ActiveStatementFlags");
            values.SetValue(Create("ManagedActiveStatementDebugInfo", instruction, statement.Document, span, Enum.Parse(kind, flags)), index);
        }
        return typeof(ImmutableArray).GetMethods().Single(method => method.Name == "Create" && method.IsGenericMethodDefinition
                && method.GetParameters().Length == 1 && method.GetParameters()[0].ParameterType.IsArray)
            .MakeGenericMethod(element).Invoke(null, [values])!;
    }

    private Type ContractType(string name) => (features ?? throw new InvalidOperationException("The compiler contract has not been initialized."))
        .GetType("Microsoft.CodeAnalysis.Contracts.EditAndContinue." + name, throwOnError: true)!;
    private object Create(string name, params object[] arguments) => Activator.CreateInstance(ContractType(name), arguments)!;
}
