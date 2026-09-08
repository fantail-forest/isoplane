#include <sys/socket.h>
#include <sys/uio.h>

ssize_t __wrap_send(int fd, const void *buffer, size_t length, int flags) {
    if ((flags & ~MSG_NOSIGNAL) != 0) {
        errno = EOPNOTSUPP;
        return -1;
    }
    uint32_t *pollables;
    int32_t count;
    int error = wisp_socket_pollables(fd, &pollables, &count);
    if (error != 0) {
        errno = error;
        return -1;
    }
    uint64_t result = (uint64_t)__wasm_import_wisp_socket_try_send(
        (int32_t)(uintptr_t)pollables, count, (int32_t)(uintptr_t)buffer,
        (int32_t)length);
    wisp_socket_pollables_free(pollables);
    if (result == WISP_SEND_PENDING || result == WISP_SEND_FAILED) {
        errno = result == WISP_SEND_PENDING ? EAGAIN : EIO;
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
