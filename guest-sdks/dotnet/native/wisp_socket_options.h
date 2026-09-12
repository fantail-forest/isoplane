#include <netinet/in.h>
#include <netinet/tcp.h>
#include <sys/socket.h>
#include <unistd.h>

struct wisp_socket_descriptor {
    int fd;
    uint32_t resource;
    int domain;
    int ipv6_only;
    int connect_started;
    struct wisp_socket_descriptor *next;
};

static struct wisp_socket_descriptor *wisp_socket_descriptors;
static uint32_t wisp_created_socket;

static void wisp_capture_socket(const uint8_t *result) {
    if (result[0] == 0) memcpy(&wisp_created_socket, result + 4, sizeof(uint32_t));
}

int __real_socket(int domain, int type, int protocol);
int __real_close(int fd);
int __real_connect(int fd, const struct sockaddr *address, socklen_t length);
int __real_getsockopt(int fd, int level, int option, void *value, socklen_t *length);
int __real_setsockopt(int fd, int level, int option, const void *value, socklen_t length);

/* Construction returns before managed code runs, so the canonical result can
 * be paired with libc's public fd without inspecting its private descriptor. */
int __wrap_socket(int domain, int type, int protocol) {
    wisp_created_socket = 0;
    int fd = __real_socket(domain, type, protocol);
    if (fd < 0 || wisp_created_socket == 0) return fd;
    struct wisp_socket_descriptor *entry = malloc(sizeof(*entry));
    if (entry == NULL) {
        if (__real_close(fd) < 0) return -1;
        errno = ENOMEM;
        return -1;
    }
    *entry = (struct wisp_socket_descriptor){
        .fd = fd,
        .resource = wisp_created_socket,
        .domain = domain,
        .ipv6_only = 1,
        .connect_started = 0,
        .next = wisp_socket_descriptors,
    };
    wisp_socket_descriptors = entry;
    return fd;
}

int __wrap_close(int fd) {
    int result = __real_close(fd);
    if (result != 0) return result;
    struct wisp_socket_descriptor **entry = &wisp_socket_descriptors;
    while (*entry != NULL) {
        if ((*entry)->fd == fd) {
            struct wisp_socket_descriptor *closed = *entry;
            *entry = closed->next;
            free(closed);
            break;
        }
        entry = &(*entry)->next;
    }
    return result;
}

static struct wisp_socket_descriptor *wisp_socket_descriptor_for(int fd) {
    for (struct wisp_socket_descriptor *entry = wisp_socket_descriptors; entry != NULL; entry = entry->next) {
        if (entry->fd == fd) return entry;
    }
    return NULL;
}

static int wisp_no_delay(int fd, int enabled) {
    struct wisp_socket_descriptor *entry = wisp_socket_descriptor_for(fd);
    if (entry == NULL) {
        errno = EBADF;
        return -1;
    }
    int result = __wasm_import_wisp_socket_no_delay(entry->resource, enabled);
    if (result < 0) errno = EIO;
    return result;
}

int __wrap_connect(int fd, const struct sockaddr *address, socklen_t length) {
    struct wisp_socket_descriptor *entry = wisp_socket_descriptor_for(fd);
    if (entry != NULL && entry->domain == AF_INET6 && entry->ipv6_only &&
        address != NULL && length >= sizeof(struct sockaddr_in6) && address->sa_family == AF_INET6) {
        const struct sockaddr_in6 *ipv6 = (const struct sockaddr_in6 *)address;
        if (IN6_IS_ADDR_V4MAPPED(&ipv6->sin6_addr)) {
            errno = ENETUNREACH;
            return -1;
        }
    }
    int result = __real_connect(fd, address, length);
    if (entry != NULL && (result == 0 || errno == EINPROGRESS || errno == EALREADY))
        entry->connect_started = 1;
    return result;
}

int __wrap_getsockopt(int fd, int level, int option, void *value, socklen_t *length) {
    int is_no_delay = level == IPPROTO_TCP && option == TCP_NODELAY;
    struct wisp_socket_descriptor *entry = wisp_socket_descriptor_for(fd);
    int is_ipv6_only = level == IPPROTO_IPV6 && option == IPV6_V6ONLY && entry != NULL && entry->domain == AF_INET6;
    if (!is_no_delay && !is_ipv6_only)
        return __real_getsockopt(fd, level, option, value, length);
    if (value == NULL || length == NULL || *length < sizeof(int)) {
        errno = EINVAL;
        return -1;
    }
    int result = is_ipv6_only ? entry->ipv6_only : wisp_no_delay(fd, -1);
    if (result < 0) return -1;
    memcpy(value, &result, sizeof(result));
    *length = sizeof(result);
    return 0;
}

int __wrap_setsockopt(int fd, int level, int option, const void *value, socklen_t length) {
    int is_no_delay = level == IPPROTO_TCP && option == TCP_NODELAY;
    struct wisp_socket_descriptor *entry = wisp_socket_descriptor_for(fd);
    int is_ipv6_only = level == IPPROTO_IPV6 && option == IPV6_V6ONLY && entry != NULL && entry->domain == AF_INET6;
    if (!is_no_delay && !is_ipv6_only)
        return __real_setsockopt(fd, level, option, value, length);
    if (value == NULL || length < sizeof(int)) {
        errno = EINVAL;
        return -1;
    }
    int enabled;
    memcpy(&enabled, value, sizeof(enabled));
    if (is_ipv6_only) {
        if (entry->connect_started) {
            errno = EINVAL;
            return -1;
        }
        entry->ipv6_only = enabled != 0;
        return 0;
    }
    return wisp_no_delay(fd, enabled != 0) < 0 ? -1 : 0;
}

/* The PAL's LingerOption: non-zero OnOff enables lingering for Seconds. */
struct wisp_pal_linger_option {
    int32_t on_off;
    int32_t seconds;
};

int32_t __real_SystemNative_SetLingerOption(intptr_t socket, struct wisp_pal_linger_option *option);

/* The host closes a socket without lingering and never with a reset, so
 * disabled linger is the only state a socket can be in. The PAL, built
 * against a libc with no SO_LINGER, reports every linger setting
 * unsupported; setting the state the socket is already in succeeds here as
 * the no-op it is, and enabling keeps the PAL's refusal. */
int32_t __wrap_SystemNative_SetLingerOption(intptr_t socket, struct wisp_pal_linger_option *option) {
    if (option != NULL && option->on_off == 0) return 0;
    return __real_SystemNative_SetLingerOption(socket, option);
}
