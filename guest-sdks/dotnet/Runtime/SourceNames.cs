using System;
using System.Runtime.CompilerServices;

namespace Isoplane.Runtime;

/// <summary>
/// Names types and methods as the person who wrote them would recognise them, in place of the
/// names the C# compiler gives the closures, lambdas and state machines it generates from them.
/// </summary>
internal static class SourceNames
{
    /// <summary>The type's namespace and name, without generic arity; a closure class is named as the type that wrote it.</summary>
    internal static string Type(Type type)
    {
        while (type.DeclaringType is { } writer && type.Name.StartsWith("<>", StringComparison.Ordinal)) type = writer;
        string name = WithoutArity(type.Name);
        if (type.DeclaringType is { } outer) return Type(outer) + "." + name;
        return string.IsNullOrEmpty(type.Namespace) ? name : type.Namespace + "." + name;
    }

    /// <summary>
    /// The async method whose state machine an object is, or is boxed in: a method builder keeps
    /// the state machine in a box generic over its type. Null for any other object.
    /// </summary>
    internal static string? AsyncMethod(object continuation)
    {
        Type? machine = continuation is IAsyncStateMachine ? continuation.GetType() : null;
        foreach (Type argument in continuation.GetType().GetGenericArguments())
            if (typeof(IAsyncStateMachine).IsAssignableFrom(argument)) machine = argument;
        if (machine is null) return null;
        return machine.DeclaringType is { } owner ? Method(Type(owner), WithoutArity(machine.Name)) : Type(machine);
    }

    /// <summary>The method a delegate calls, or the async method it resumes.</summary>
    internal static string Method(Delegate called) =>
        called.Target is { } target && AsyncMethod(target) is { } resumed ? resumed
        : Method(called.Method.DeclaringType is { } owner ? Type(owner) : "", called.Method.Name);

    /// <summary>
    /// The method named <paramref name="name"/> on <paramref name="type"/>. The compiler names what
    /// it generates after the method it came from: <c>&lt;Run&gt;d__1</c> is the state machine of
    /// an async <c>Run</c>, <c>&lt;Run&gt;b__1_0</c> a lambda in it, <c>&lt;Run&gt;g__Step|1_0</c>
    /// its local function <c>Step</c> and <c>&lt;Main&gt;$</c> a program's top-level statements.
    /// A generated name of any other form is returned unchanged.
    /// </summary>
    internal static string Method(string type, string name)
    {
        if (name == ".ctor") return "the constructor of " + type;
        if (name == ".cctor") return "the static constructor of " + type;
        string unchanged = type.Length == 0 ? name : type + "." + name;
        if (!name.StartsWith('<') || ClosingBracket(name) is not (> 1 and int close)) return unchanged;
        string written = name[1..close], generated = name[(close + 1)..];
        if (written == "Main" && generated == "$") return "the program's top-level statements";
        string enclosing = Method(type, written);
        if (generated == "d" || generated.StartsWith("d__", StringComparison.Ordinal)) return enclosing;
        if (generated.StartsWith("b__", StringComparison.Ordinal)) return "a lambda in " + enclosing;
        if (generated.StartsWith("g__", StringComparison.Ordinal) && generated.IndexOf('|') is > 3 and int end)
            return "the local function " + generated[3..end] + " in " + enclosing;
        return unchanged;
    }

    private static string WithoutArity(string name) => name.IndexOf('`') is >= 0 and int arity ? name[..arity] : name;

    private static int ClosingBracket(string name)
    {
        int depth = 0;
        for (int index = 0; index < name.Length; index++)
        {
            if (name[index] == '<') depth++;
            else if (name[index] == '>' && --depth == 0) return index;
        }
        return -1;
    }
}
