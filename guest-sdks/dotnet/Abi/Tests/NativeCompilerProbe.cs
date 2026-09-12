using System;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

namespace Wisp.Abi;

internal static unsafe class NativeCompilerProbe
{
    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_arg_u8")]
    private static extern uint Argumentu8(byte value);
    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_return_u8")]
    private static extern byte Returnu8(uint value);
    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_callback_u8")]
    private static extern uint Callbacku8(delegate* unmanaged[Cdecl]<byte, byte> callback, byte value);
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static byte Echou8(byte value) => value;

    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_arg_i8")]
    private static extern int Argumenti8(sbyte value);
    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_return_i8")]
    private static extern sbyte Returni8(int value);
    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_callback_i8")]
    private static extern int Callbacki8(delegate* unmanaged[Cdecl]<sbyte, sbyte> callback, sbyte value);
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static sbyte Echoi8(sbyte value) => value;

    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_arg_u16")]
    private static extern uint Argumentu16(ushort value);
    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_return_u16")]
    private static extern ushort Returnu16(uint value);
    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_callback_u16")]
    private static extern uint Callbacku16(delegate* unmanaged[Cdecl]<ushort, ushort> callback, ushort value);
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static ushort Echou16(ushort value) => value;

    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_arg_i16")]
    private static extern int Argumenti16(short value);
    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_return_i16")]
    private static extern short Returni16(int value);
    [DllImport("wisp_compiler_probe", EntryPoint = "wisp_probe_callback_i16")]
    private static extern int Callbacki16(delegate* unmanaged[Cdecl]<short, short> callback, short value);
    [UnmanagedCallersOnly(CallConvs = new[] { typeof(CallConvCdecl) })]
    private static short Echoi16(short value) => value;

    internal static string Run(string kind, string width)
    {
        for (int bits = 0; bits <= ushort.MaxValue; bits += 1)
        {
            long expected;
            long actual;
            switch (width)
            {
                case "u8":
                    byte valueu8 = unchecked((byte)bits);
                    expected = valueu8;
                    actual = kind switch
                    {
                        "argument" => Argumentu8(valueu8),
                        "return" => Returnu8(unchecked((uint)(0xA5A50000u | (uint)bits))),
                        "callback" => Callbacku8(&Echou8, valueu8),
                        _ => throw new ArgumentException("unknown ABI direction")
                    };
                    break;
                case "i8":
                    sbyte valuei8 = unchecked((sbyte)bits);
                    expected = valuei8;
                    actual = kind switch
                    {
                        "argument" => Argumenti8(valuei8),
                        "return" => Returni8(unchecked((int)(0xA5A50000u | (uint)bits))),
                        "callback" => Callbacki8(&Echoi8, valuei8),
                        _ => throw new ArgumentException("unknown ABI direction")
                    };
                    break;
                case "u16":
                    ushort valueu16 = unchecked((ushort)bits);
                    expected = valueu16;
                    actual = kind switch
                    {
                        "argument" => Argumentu16(valueu16),
                        "return" => Returnu16(unchecked((uint)(0xA5A50000u | (uint)bits))),
                        "callback" => Callbacku16(&Echou16, valueu16),
                        _ => throw new ArgumentException("unknown ABI direction")
                    };
                    break;
                case "i16":
                    short valuei16 = unchecked((short)bits);
                    expected = valuei16;
                    actual = kind switch
                    {
                        "argument" => Argumenti16(valuei16),
                        "return" => Returni16(unchecked((int)(0xA5A50000u | (uint)bits))),
                        "callback" => Callbacki16(&Echoi16, valuei16),
                        _ => throw new ArgumentException("unknown ABI direction")
                    };
                    break;
                default: throw new ArgumentException("unknown ABI width");
            }
            if (actual != expected) return $"{kind}/{width}: bits={bits} expected={expected} actual={actual}";
        }
        return "ok";
    }
}
