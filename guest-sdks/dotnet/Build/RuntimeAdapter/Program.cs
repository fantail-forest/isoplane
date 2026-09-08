using Mono.Cecil;
using Mono.Cecil.Cil;

if (args.Length != 2)
    throw new ArgumentException("Expected the pinned CoreLib input and a separate output path.");
if (Path.GetFullPath(args[0]) == Path.GetFullPath(args[1]))
    throw new ArgumentException("The runtime package is immutable; output must be a separate file.");

using var module = ModuleDefinition.ReadModule(args[0]);
var dispatch = module.Types.Single(type => type.FullName == "System.Threading.ThreadPoolWorkQueue")
    .Methods.Single(method => method.Name == "Dispatch" && method.Parameters.Count == 0
        && method.IsStatic && method.ReturnType.FullName == "System.Boolean" && method.HasBody);
var thread = module.Types.Single(type => type.FullName == "System.Threading.Thread");
if (thread.Methods.Any(method => method.Name == "WispRunReady"))
    throw new InvalidOperationException("The input CoreLib already contains the WISP scheduler adaptation.");

var pump = new MethodDefinition("WispRunReady", MethodAttributes.Assembly | MethodAttributes.Static,
    module.TypeSystem.Void);
thread.Methods.Add(pump);
var il = pump.Body.GetILProcessor();
il.Append(Instruction.Create(OpCodes.Call, dispatch));
il.Append(Instruction.Create(OpCodes.Pop));
il.Append(Instruction.Create(OpCodes.Ret));
module.Write(args[1]);
