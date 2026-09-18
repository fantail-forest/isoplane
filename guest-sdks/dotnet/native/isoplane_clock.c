#include <stdint.h>

#define ISOPLANE_CLOCK_QUANTUM_NS 1000000LL

__attribute__((import_module("isoplane"), import_name("clock_now_coarse")))
extern int64_t isoplane_clock_now_coarse(void);

/* ---- live: monotonic clock ---------------------------------------------- */

/* Convert the host's coarse millisecond clock to WASI nanoseconds. The
 * host owns the clock sample for the serving slice. */
int64_t __wasm_import_monotonic_clock_now(void)
{
    return isoplane_clock_now_coarse() * 1000000LL;
}

/* ---- live: wall clock --------------------------------------------------- */

/* The canonical datetime result uses a 16-byte, 8-aligned area: seconds at
 * offset 0 and nanoseconds at offset 8. Both adapters derive from the host's
 * coarse millisecond clock. A serving slice observes its fixed clock sample;
 * a later slice can observe a later sample. */
void __wasm_import_wall_clock_now(int32_t retptr)
{
    int64_t ms = isoplane_clock_now_coarse();
    if (ms < 0) {
        /* Epoch milliseconds cannot be negative; a clamp keeps the unsigned
         * division below from producing a date the guest would persist. */
        ms = 0;
    }
    uint64_t *seconds = (uint64_t *)(uintptr_t)retptr;
    uint32_t *nanoseconds = (uint32_t *)(uintptr_t)(retptr + 8);
    *seconds = (uint64_t)ms / 1000u;
    *nanoseconds = (uint32_t)((uint64_t)ms % 1000u) * 1000000u;
}

int64_t __wasm_import_monotonic_clock_resolution(void)
{
    return ISOPLANE_CLOCK_QUANTUM_NS;
}

void __wasm_import_wall_clock_resolution(int32_t retptr)
{
    uint64_t *seconds = (uint64_t *)(uintptr_t)retptr;
    uint32_t *nanoseconds = (uint32_t *)(uintptr_t)(retptr + 8);
    *seconds = ISOPLANE_CLOCK_QUANTUM_NS / 1000000000LL;
    *nanoseconds = ISOPLANE_CLOCK_QUANTUM_NS % 1000000000LL;
}
