using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class StaticMethods
{
    internal static void Forward(ModuleDefinition module, ModuleDefinition sdk,
        string runtimeType, string bridgeType)
    {
        var runtime = module.Types.Single(type => type.FullName == runtimeType);
        var bridge = sdk.Types.Single(type => type.FullName == bridgeType);
        foreach (var target in bridge.Methods.Where(method => method.IsPublic && method.IsStatic))
        {
            var method = runtime.Methods.Single(method => method.IsStatic && method.Name == target.Name
                && method.ReturnType.FullName == target.ReturnType.FullName
                && method.Parameters.Select(parameter => parameter.ParameterType.FullName)
                    .SequenceEqual(target.Parameters.Select(parameter => parameter.ParameterType.FullName)));
            method.Body = new MethodBody(method);
            var il = method.Body.GetILProcessor();
            foreach (var parameter in method.Parameters)
                il.Emit(OpCodes.Ldarg, parameter);
            il.Emit(OpCodes.Call, module.ImportReference(target));
            il.Emit(OpCodes.Ret);
        }
    }
}
