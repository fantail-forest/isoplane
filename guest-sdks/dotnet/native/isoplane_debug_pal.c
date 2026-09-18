// Only the local debugger host may wait while a guest invocation is live.
#define __wasm_import_poll_method_pollable_block isoplane_unsupported_pollable_block
#include "isoplane_pal.c"
#undef __wasm_import_poll_method_pollable_block

void __wasm_import_poll_method_pollable_block(int32_t handle)
{
    uint32_t result[2];
    __wasm_import_poll_poll((int32_t)(uintptr_t)&handle, 1, (int32_t)(uintptr_t)result);
    free((void *)(uintptr_t)result[0]);
}
