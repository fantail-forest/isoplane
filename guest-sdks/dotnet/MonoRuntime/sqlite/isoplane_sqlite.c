/*
 * SQLite as the shared runtime's native library `e_sqlite3`, the module
 * SQLitePCLRaw's provider imports, configured for databases held in memory.
 *
 * An instance keeps no storage beyond its own memory, so a database file would
 * vanish with the instance that wrote it. The operating-system layer is
 * therefore this file's VFS, which refuses every file and serves randomness and
 * the current time from the runtime's libc. The feature options follow the
 * e_sqlite3 builds SQLitePCLRaw ships for other platforms.
 *
 * Compiling the amalgamation in this translation unit lets the open functions
 * replace the generic "unable to open database file" with a message that says
 * why a file cannot be opened and what to use instead.
 */

#define SQLITE_OS_OTHER 1
#define SQLITE_THREADSAFE 0
#define SQLITE_TEMP_STORE 3
#define SQLITE_OMIT_LOAD_EXTENSION 1

#define SQLITE_DEFAULT_FOREIGN_KEYS 1
#define SQLITE_ENABLE_COLUMN_METADATA 1
#define SQLITE_ENABLE_FTS3_PARENTHESIS 1
#define SQLITE_ENABLE_FTS4 1
#define SQLITE_ENABLE_FTS5 1
#define SQLITE_ENABLE_MATH_FUNCTIONS 1
#define SQLITE_ENABLE_RTREE 1
#define SQLITE_ENABLE_SNAPSHOT 1

#define sqlite3_open isoplane_sqlite3_open_unchecked
#define sqlite3_open16 isoplane_sqlite3_open16_unchecked
#define sqlite3_open_v2 isoplane_sqlite3_open_v2_unchecked
#include "sqlite3.c"
#undef sqlite3_open
#undef sqlite3_open16
#undef sqlite3_open_v2

#include <sys/random.h>
#include <time.h>

static const char isoplane_sqlite_file_refused[] =
    "file-backed databases are not available, because no storage outlasts an instance; "
    "open an in-memory database instead, such as ':memory:' or "
    "'file:name?mode=memory&cache=shared' (Microsoft.Data.Sqlite: "
    "'Data Source=:memory:' or 'Data Source=name;Mode=Memory;Cache=Shared')";

/* Set when the VFS refuses a file during the open in progress. */
static int isoplane_sqlite_refused_file;

static int isoplane_vfs_open(sqlite3_vfs *vfs, sqlite3_filename name, sqlite3_file *file, int flags, int *out_flags)
{
    (void)vfs;
    (void)name;
    (void)flags;
    (void)out_flags;
    file->pMethods = 0;
    isoplane_sqlite_refused_file = 1;
    return SQLITE_CANTOPEN;
}

static int isoplane_vfs_delete(sqlite3_vfs *vfs, const char *name, int sync_directory)
{
    (void)vfs;
    (void)name;
    (void)sync_directory;
    return SQLITE_IOERR_DELETE;
}

static int isoplane_vfs_access(sqlite3_vfs *vfs, const char *name, int flags, int *result)
{
    (void)vfs;
    (void)name;
    (void)flags;
    *result = 0;
    return SQLITE_OK;
}

static int isoplane_vfs_full_pathname(sqlite3_vfs *vfs, const char *name, int size, char *out)
{
    (void)vfs;
    if ((int)strlen(name) >= size) return SQLITE_CANTOPEN;
    memcpy(out, name, strlen(name) + 1);
    return SQLITE_OK;
}

static int isoplane_vfs_randomness(sqlite3_vfs *vfs, int size, char *out)
{
    (void)vfs;
    /* getentropy draws at most 256 bytes per call. */
    for (int offset = 0; offset < size; offset += 256) {
        size_t length = (size_t)(size - offset < 256 ? size - offset : 256);
        if (getentropy(out + offset, length) != 0) return offset;
    }
    return size;
}

/* Nothing else runs in the instance while a call is in progress, so waiting
 * could not release the lock a busy handler waits for, and a guest call must
 * not block. Reporting no time slept lets SQLite's busy handler count down its
 * timeout without waiting. */
static int isoplane_vfs_sleep(sqlite3_vfs *vfs, int microseconds)
{
    (void)vfs;
    (void)microseconds;
    return 0;
}

static int isoplane_vfs_current_time_int64(sqlite3_vfs *vfs, sqlite3_int64 *julian_day_ms)
{
    (void)vfs;
    /* Milliseconds from the Julian day epoch to the Unix epoch. */
    static const sqlite3_int64 unix_epoch_ms = 24405875 * (sqlite3_int64)8640000;
    struct timespec now;
    if (clock_gettime(CLOCK_REALTIME, &now) != 0) return SQLITE_ERROR;
    *julian_day_ms = unix_epoch_ms + (sqlite3_int64)now.tv_sec * 1000 + now.tv_nsec / 1000000;
    return SQLITE_OK;
}

static int isoplane_vfs_current_time(sqlite3_vfs *vfs, double *julian_day)
{
    sqlite3_int64 julian_day_ms;
    int rc = isoplane_vfs_current_time_int64(vfs, &julian_day_ms);
    if (rc == SQLITE_OK) *julian_day = julian_day_ms / 86400000.0;
    return rc;
}

static int isoplane_vfs_get_last_error(sqlite3_vfs *vfs, int size, char *out)
{
    (void)vfs;
    (void)size;
    (void)out;
    return 0;
}

static sqlite3_vfs isoplane_vfs = {
    .iVersion = 2,
    .szOsFile = sizeof(sqlite3_file),
    .mxPathname = 1024,
    .zName = "isoplane",
    .xOpen = isoplane_vfs_open,
    .xDelete = isoplane_vfs_delete,
    .xAccess = isoplane_vfs_access,
    .xFullPathname = isoplane_vfs_full_pathname,
    .xRandomness = isoplane_vfs_randomness,
    .xSleep = isoplane_vfs_sleep,
    .xCurrentTime = isoplane_vfs_current_time,
    .xGetLastError = isoplane_vfs_get_last_error,
    .xCurrentTimeInt64 = isoplane_vfs_current_time_int64,
};

int sqlite3_os_init(void)
{
    return sqlite3_vfs_register(&isoplane_vfs, 1);
}

int sqlite3_os_end(void)
{
    return SQLITE_OK;
}

static int isoplane_sqlite_explain_refusal(int rc, sqlite3 *db)
{
    if (isoplane_sqlite_refused_file && (rc & 0xff) == SQLITE_CANTOPEN && db)
        sqlite3ErrorWithMsg(db, rc, "%s", isoplane_sqlite_file_refused);
    isoplane_sqlite_refused_file = 0;
    return rc;
}

SQLITE_API int sqlite3_open(const char *filename, sqlite3 **db)
{
    isoplane_sqlite_refused_file = 0;
    return isoplane_sqlite_explain_refusal(isoplane_sqlite3_open_unchecked(filename, db), *db);
}

SQLITE_API int sqlite3_open16(const void *filename, sqlite3 **db)
{
    isoplane_sqlite_refused_file = 0;
    return isoplane_sqlite_explain_refusal(isoplane_sqlite3_open16_unchecked(filename, db), *db);
}

SQLITE_API int sqlite3_open_v2(const char *filename, sqlite3 **db, int flags, const char *vfs)
{
    isoplane_sqlite_refused_file = 0;
    return isoplane_sqlite_explain_refusal(isoplane_sqlite3_open_v2_unchecked(filename, db, flags, vfs), *db);
}
