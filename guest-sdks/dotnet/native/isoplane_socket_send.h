#include <sys/socket.h>
#include <sys/uio.h>

// libc's errno for a wasi:sockets/network error code.
extern int __wasi_sockets_utils__map_error(uint8_t error);

ssize_t __wrap_send(int fd, const void *buffer, size_t length, int flags) {
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

ssize_t __wrap_sendmsg(int fd, const struct msghdr *message, int flags) {
    if (message->msg_name != NULL || message->msg_controllen != 0) {
        errno = EOPNOTSUPP;
        return -1;
    }
    ssize_t sent = 0;
    for (size_t index = 0; index < message->msg_iovlen; ++index) {
        const struct iovec *buffer = &message->msg_iov[index];
        ssize_t count = __wrap_send(fd, buffer->iov_base, buffer->iov_len, flags);
        if (count < 0) return sent == 0 ? -1 : sent;
        sent += count;
        if ((size_t)count != buffer->iov_len) break;
    }
    return sent;
}
