/*
 * Native entry points for managed callbacks that SQLite calls.
 *
 * WebAssembly calls a function pointer only with the function's own static
 * type, so each managed method native code calls needs a native entry point
 * of its signature. The runtime build generates them for the methods of the
 * assemblies it compiles; SQLitePCLRaw is an application's package, so its
 * callbacks take entry points from pools generated for the callback shapes of
 * SQLite's C API, and serve whichever SQLitePCLRaw version the application
 * resolves.
 */

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include "sqlite_callbacks.h"

/* The runtime pack's embedding API, declared as mono_entry.c declares it:
 * native link inputs compile without the pack's headers. */
typedef struct MonoMethod MonoMethod;
typedef void *(*NativeToManagedEntry)(MonoMethod *method, void *target);
extern void mono_wasm_install_get_native_to_interp_tramp(NativeToManagedEntry lookup);
extern void *wasm_dl_get_native_to_interp(uint32_t token, const char *key, void *target);
extern int mono_wasm_signature_cookie(void *signature, char *cookie, int size);
extern void *mono_method_get_class(MonoMethod *method);
extern const char *mono_method_get_name(MonoMethod *method);
extern uint32_t mono_method_get_token(MonoMethod *method);
extern void *mono_method_signature(MonoMethod *method);
extern uint32_t mono_signature_get_param_count(void *signature);
extern void *mono_class_get_image(void *klass);
extern const char *mono_class_get_namespace(void *klass);
extern const char *mono_class_get_name(void *klass);
extern void *mono_image_get_assembly(void *image);
extern void *mono_assembly_get_name(void *assembly);
extern const char *mono_assembly_name_get_name(void *name);

/* The assembly that imports e_sqlite3. Other application methods get no
 * native entry point: application callbacks are unsupported, and SQLite's
 * are served because the runtime provides SQLite. */
static const char sqlite_provider[] = "SQLitePCLRaw.provider.e_sqlite3";

/* An entry point the runtime build generated, keyed as the runtime's own
 * lookup keys it; that lookup is private to the WASI driver, which
 * native_to_managed_entry replaces. */
static void *generated_entry(MonoMethod *method, void *klass, const char *assembly, void *target)
{
    uint32_t parameters = mono_signature_get_param_count(mono_method_signature(method));
    int length = snprintf(NULL, 0, "%s#%u:%s:%s:%s", mono_method_get_name(method), parameters, assembly,
                          mono_class_get_namespace(klass), mono_class_get_name(klass));
    char key[length + 1];
    snprintf(key, sizeof(key), "%s#%u:%s:%s:%s", mono_method_get_name(method), parameters, assembly,
             mono_class_get_namespace(klass), mono_class_get_name(klass));
    return wasm_dl_get_native_to_interp(mono_method_get_token(method), key, target);
}

static void *pooled_entry(MonoMethod *method, void *target)
{
    char signature[32];
    if (!mono_wasm_signature_cookie(mono_method_signature(method), signature, sizeof(signature))) return NULL;
    for (int index = 0; index < isoplane_sqlite_callback_pool_count; index++) {
        IsoplaneCallbackPool *pool = &isoplane_sqlite_callback_pools[index];
        if (strcmp(pool->signature, signature) != 0) continue;
        if (pool->used == pool->capacity) return NULL;
        pool->targets[pool->used] = *(IsoplaneCallbackTarget *)target;
        return pool->entries[pool->used++];
    }
    return NULL;
}

/* The runtime asks once per method and keeps the entry point it returns; it
 * refuses the method with PlatformNotSupportedException when there is none. */
static void *native_to_managed_entry(MonoMethod *method, void *target)
{
    void *klass = mono_method_get_class(method);
    const char *assembly = mono_assembly_name_get_name(mono_assembly_get_name(mono_image_get_assembly(mono_class_get_image(klass))));
    if (!assembly) return NULL;
    void *entry = generated_entry(method, klass, assembly, target);
    if (entry || strcmp(assembly, sqlite_provider) != 0) return entry;
    return pooled_entry(method, target);
}

void isoplane_install_sqlite_callbacks(void)
{
    mono_wasm_install_get_native_to_interp_tramp(native_to_managed_entry);
}
