#include <sys/socket.h>

// libc's errno for a wasi:sockets/network error code.
extern int __wasi_sockets_utils__map_error(uint8_t error);

ssize_t isoplane_socket_send(int fd, const void *buffer, size_t length, int flags) {
    if ((flags & ~MSG_NOSIGNAL) != 0) {
        errno = EOPNOTSUPP;
        return -1;
    }
    uint32_t *pollables;
    int32_t count;
    int error = isoplane_socket_pollables(fd, &pollables, &count);
    if (error != 0) {
        errno = error;
        return -1;
    }
    uint64_t result = (uint64_t)__wasm_import_isoplane_socket_try_send_with_error_code(
        (int32_t)(uintptr_t)pollables, count, (int32_t)(uintptr_t)buffer,
        (int32_t)length);
    isoplane_socket_pollables_free(pollables);
    if (result == ISOPLANE_SEND_PENDING) {
        errno = EAGAIN;
        return -1;
    }
    if (ISOPLANE_SEND_IS_FAILURE(result)) {
        // The host names the failure; one it cannot name stays EIO.
        uint8_t code = ISOPLANE_SEND_FAILURE_CODE(result);
        errno = code == 0 ? EIO : __wasi_sockets_utils__map_error(code);
        return -1;
    }
    return (ssize_t)result;
}
