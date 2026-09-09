using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class Randomness
{
    internal static void Adapt(ModuleDefinition module, ModuleDefinition sdk)
    {
        var random = module.Types.Single(type => type.FullName == "System.Security.Cryptography.RandomNumberGenerator");
        var bridge = sdk.Types.Single(type => type.FullName == "Wisp.Runtime.Randomness");
        foreach (var target in bridge.Methods.Where(method => method.IsPublic && method.IsStatic))
        {
            var method = random.Methods.Single(method => method.IsStatic && method.Name == target.Name
                && method.ReturnType.FullName == target.ReturnType.FullName
                && method.Parameters.Select(parameter => parameter.ParameterType.FullName)
                    .SequenceEqual(target.Parameters.Select(parameter => parameter.ParameterType.FullName)));
            method.Body = new MethodBody(method);
            var il = method.Body.GetILProcessor();
            il.Emit(OpCodes.Ldarg_0);
            il.Emit(OpCodes.Call, module.ImportReference(target));
            il.Emit(OpCodes.Ret);
        }
    }
}
