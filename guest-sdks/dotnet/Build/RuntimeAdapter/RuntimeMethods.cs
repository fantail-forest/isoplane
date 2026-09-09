using Mono.Cecil;

internal static class RuntimeMethods
{
    internal static MethodDefinition Require(TypeDefinition type, string name, bool isStatic,
        string returnType, params string[] parameters)
    {
        var matches = type.Methods.Where(method => method.Name == name && method.IsStatic == isStatic
            && !method.HasGenericParameters && method.ReturnType.FullName == returnType
            && method.Parameters.Select(parameter => parameter.ParameterType.FullName).SequenceEqual(parameters)).ToArray();
        if (matches.Length != 1)
            throw new InvalidOperationException($"Expected exactly one pinned method: {returnType} {type.FullName}.{name}({string.Join(", ", parameters)}), static={isStatic}.");
        return matches[0];
    }
}
