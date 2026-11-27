using System;
using System.Runtime.InteropServices;

namespace Isoplane.Abi;

/// <summary>
/// The managed declarations of the host imports the runtime module takes from
/// wasm module <c>"isoplane"</c>.
/// </summary>
internal static unsafe partial class H
{
    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "runtime_config_get")]
    internal static extern int RuntimeConfigGet(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "dispatch_ready")]
    internal static extern long DispatchReady(long dispatch);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "abort")]
    internal static extern void Abort(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "log_emit")]
    internal static extern void LogEmit(int level, byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "future_cancel")]
    internal static extern void FutureCancel(long future);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "req_body_read")]
    internal static extern long ReqBodyRead(int maxBytes);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "req_body_can_have_body")]
    internal static extern int ReqBodyCanHaveBody();

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "resp_new")]
    internal static extern int RespNew(byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "resp_write")]
    internal static extern long RespWrite(int handle, byte* ptr, int len);

    [WasmImportLinkage]
    [DllImport("isoplane", EntryPoint = "resp_finish")]
    internal static extern void RespFinish(int handle);
}
