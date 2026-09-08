/* Generated from wisp_abi::native_io::canonical. */
__attribute__((import_module("wisp"), import_name("poll_call")))
extern int64_t wisp_poll_call(int32_t operation, const uint64_t *arguments, int32_t count);
int32_t __wasm_import_monotonic_clock_subscribe_duration(int64_t a0) { const uint64_t args[] = { (uint64_t)a0 }; return (int32_t)wisp_poll_call(0, args, 1); }
void __wasm_import_poll_pollable_drop(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; wisp_poll_call(1, args, 1); }
void __wasm_import_poll_poll(int32_t a0, int32_t a1, int32_t a2) { const uint64_t args[] = { (uint32_t)a0, (uint32_t)a1, (uint32_t)a2 }; wisp_poll_call(2, args, 3); }
int32_t __wasm_import_monotonic_clock_subscribe_instant(int64_t a0) { const uint64_t args[] = { (uint64_t)a0 }; return (int32_t)wisp_poll_call(57, args, 1); }
int32_t __wasm_import_poll_method_pollable_ready(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; return (int32_t)wisp_poll_call(58, args, 1); }
void __wasm_import_poll_method_pollable_block(int32_t a0) { const uint64_t args[] = { (uint32_t)a0 }; wisp_poll_call(59, args, 1); }
