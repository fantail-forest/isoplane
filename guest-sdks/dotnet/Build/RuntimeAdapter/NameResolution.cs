using Mono.Cecil;
using Mono.Cecil.Cil;

internal static class NameResolution
{
    internal static void Adapt(ModuleDefinition module, ModuleDefinition sdk)
    {
        var dns = module.Types.Single(type => type.FullName == "System.Net.Dns");
        var target = module.ImportReference(sdk.Types.Single(type => type.FullName == "Wisp.Runtime.NameResolution")
            .Methods.Single(method => method.Name == "GetHostAddressesAsync"));
        foreach (var method in dns.Methods.Where(method => method.Name == "GetHostAddressesAsync"))
        {
            if (method.Parameters.Count is < 1 or > 3 || method.Parameters[0].ParameterType.FullName != "System.String")
                throw new InvalidOperationException("Unexpected pinned DNS overload: " + method.FullName);
            method.Body = new MethodBody(method) { InitLocals = true };
            var il = method.Body.GetILProcessor();
            il.Emit(OpCodes.Ldarg_0);
            var family = method.Parameters.SingleOrDefault(parameter => parameter.ParameterType.FullName == "System.Net.Sockets.AddressFamily");
            if (family is null) il.Emit(OpCodes.Ldc_I4_0);
            else il.Emit(OpCodes.Ldarg, family);
            var cancellation = method.Parameters.SingleOrDefault(parameter => parameter.ParameterType.FullName == "System.Threading.CancellationToken");
            if (cancellation is null)
            {
                var empty = new VariableDefinition(module.ImportReference(target.Parameters[2].ParameterType));
                method.Body.Variables.Add(empty);
                il.Emit(OpCodes.Ldloc, empty);
            }
            else il.Emit(OpCodes.Ldarg, cancellation);
            il.Emit(OpCodes.Call, target);
            il.Emit(OpCodes.Ret);
        }
    }
}
