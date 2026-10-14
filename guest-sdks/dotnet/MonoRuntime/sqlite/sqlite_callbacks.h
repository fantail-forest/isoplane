#ifndef ISOPLANE_SQLITE_CALLBACKS_H
#define ISOPLANE_SQLITE_CALLBACKS_H

/* The interpreter entry an assigned native entry point calls, as the runtime
 * describes it (InterpFtnDesc in the runtime pack's pinvoke.h). */
typedef struct {
    void *func;
    void *arg;
} IsoplaneCallbackTarget;

/* Native entry points of one interpreter-to-native signature. Each is bound to
 * one managed method, in order, on first use. */
typedef struct {
    const char *signature;
    IsoplaneCallbackTarget *targets;
    void *const *entries;
    int capacity;
    int used;
} IsoplaneCallbackPool;

extern IsoplaneCallbackPool isoplane_sqlite_callback_pools[];
extern const int isoplane_sqlite_callback_pool_count;

#endif
