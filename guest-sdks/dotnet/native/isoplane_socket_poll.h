#include <poll.h>

struct isoplane_poll_capture {
    uint32_t *handles;
    uint32_t count;
    int error;
};

/* A poll is synchronous and cannot reenter managed code. Capturing its actual
 * canonical handles avoids depending on wasi-libc's private descriptor layout. */
static struct isoplane_poll_capture *isoplane_active_poll_capture;

static void isoplane_capture_pollables(const uint32_t *handles, uint32_t count) {
    struct isoplane_poll_capture *capture = isoplane_active_poll_capture;
    if (capture == NULL || capture->error != 0) return;
    if (count > UINT32_MAX / sizeof(uint32_t)) {
        capture->error = ENOMEM;
        return;
    }
    uint32_t *copy = malloc((size_t)count * sizeof(uint32_t));
    if (count != 0 && copy == NULL) {
        capture->error = ENOMEM;
        return;
    }
    if (count != 0) memcpy(copy, handles, (size_t)count * sizeof(uint32_t));
    free(capture->handles);
    capture->handles = copy;
    capture->count = count;
}

static void isoplane_forget_pollable(uint32_t handle) {
    struct isoplane_poll_capture *capture = isoplane_active_poll_capture;
    if (capture == NULL) return;
    uint32_t retained = 0;
    for (uint32_t index = 0; index < capture->count; ++index) {
        if (capture->handles[index] != handle)
            capture->handles[retained++] = capture->handles[index];
    }
    capture->count = retained;
}

int32_t isoplane_socket_pollables(int32_t fd, uint32_t **handles, int32_t *count) {
    *handles = NULL;
    *count = 0;
    if (isoplane_active_poll_capture != NULL) return EBUSY;
    struct isoplane_poll_capture capture = {0};
    struct pollfd descriptor = {.fd = fd, .events = POLLIN | POLLOUT};
    isoplane_active_poll_capture = &capture;
    int result = poll(&descriptor, 1, 0);
    int error = result < 0 ? errno : 0;
    isoplane_active_poll_capture = NULL;
    if ((descriptor.revents & POLLNVAL) != 0) error = EBADF;
    if (capture.error != 0) error = capture.error;
    if (capture.count > INT32_MAX) error = ENOMEM;
    if (error != 0) {
        free(capture.handles);
        return error;
    }
    *handles = capture.handles;
    *count = (int32_t)capture.count;
    return 0;
}

void isoplane_socket_pollables_free(void *handles) {
    free(handles);
}
