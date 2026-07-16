pub(crate) const EPOLL_SOURCE: &str = r#"#define _POSIX_C_SOURCE 200809L

#include "cr_backend_internal.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdint.h>
#include <string.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#define CR_EPOLL_OPERATION_MAGIC UINT64_C(0x435245504f4c4c31)

#define CR_EPOLL_OPERATION_INITIALIZED UINT32_C(1)
#define CR_EPOLL_OPERATION_SUBMITTED   UINT32_C(2)
#define CR_EPOLL_OPERATION_TERMINAL    UINT32_C(3)
#define CR_EPOLL_OPERATION_QUIESCENT   UINT32_C(4)
#define CR_EPOLL_OPERATION_DESTROYED   UINT32_C(5)

#define CR_EPOLL_INTERRUPT_TOKEN UINT64_C(1)
#define CR_EPOLL_CANCEL_TOKEN    UINT64_C(2)
#define CR_EPOLL_FIRST_OPERATION_TOKEN UINT64_C(3)
#define CR_EPOLL_BATCH_SIZE 64

#ifndef CR_BACKEND_EPOLL_FD_OPENED
#define CR_BACKEND_EPOLL_FD_OPENED(fd) ((void)(fd))
#endif

#ifndef CR_BACKEND_EPOLL_FD_CLOSED
#define CR_BACKEND_EPOLL_FD_CLOSED(fd) ((void)(fd))
#endif

#ifndef CR_BACKEND_EPOLL_SUBMIT_OBSERVED
#define CR_BACKEND_EPOLL_SUBMIT_OBSERVED(operation, generation, token) \
    ((void)(operation), (void)(generation), (void)(token))
#endif

#ifndef CR_BACKEND_EPOLL_BEFORE_RECV
#define CR_BACKEND_EPOLL_BEFORE_RECV(operation, generation, fd) \
    ((void)(operation), (void)(generation), (void)(fd))
#endif

#ifndef CR_BACKEND_EPOLL_REARMED
#define CR_BACKEND_EPOLL_REARMED(operation, generation, token) \
    ((void)(operation), (void)(generation), (void)(token))
#endif

#ifndef CR_BACKEND_EPOLL_FILTER_EVENT_TOKEN
#define CR_BACKEND_EPOLL_FILTER_EVENT_TOKEN(token) (token)
#endif

typedef struct cr_backend_epoll_state cr_backend_epoll_state;

struct cr_net_receive_operation {
    uint64_t magic;
    uint64_t generation;
    uint64_t event_token;
    uint32_t state;
    bool linked;
    bool registered;
    bool cancel_requested;
    bool cancel_pending;
    bool callback_delivered;
    cr_backend *backend;
    int socket_fd;
    void *buffer;
    uint64_t buffer_size;
    cr_net_receive_completion_fn on_completion;
    void *callback_context;
    cr_net_receive_operation *previous;
    cr_net_receive_operation *next;
    cr_net_receive_operation *cancel_previous;
    cr_net_receive_operation *cancel_next;
};

struct cr_backend_epoll_state {
    int epoll_fd;
    int interrupt_fd;
    int completion_fd;
    bool shutdown;
    bool token_exhausted;
    uint64_t next_token;
    cr_net_receive_operation *active_head;
    cr_net_receive_operation *cancel_head;
    cr_net_receive_operation *cancel_tail;
};

static const cr_net_extension_desc cr_backend_epoll_net_extension;

static void cr_epoll_clear_net_error(cr_net_error *out_error) {
    if (out_error == NULL) return;
    *out_error = (cr_net_error){
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_error),
        CR_NET_ERROR_NONE,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    };
}

static void cr_epoll_set_net_error(
    cr_net_error *out_error,
    cr_net_error_category category,
    int native_code
) {
    if (out_error == NULL) return;
    *out_error = (cr_net_error){
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_error),
        category,
        native_code == 0
            ? CR_NATIVE_ERROR_DOMAIN_NONE
            : CR_NATIVE_ERROR_DOMAIN_ERRNO,
        (int64_t)native_code
    };
}

static cr_backend_epoll_state *cr_epoll_state_from_backend(
    const cr_backend *backend
) {
    if (backend == NULL ||
        cr_backend_internal_provider(backend) !=
            &cr_backend_epoll_provider_desc) {
        return NULL;
    }
    return (cr_backend_epoll_state *)
        cr_backend_internal_provider_state(backend);
}

static bool cr_epoll_require_owner(
    cr_backend *backend,
    cr_net_error *out_error
) {
    cr_backend_error backend_error;

    if (cr_epoll_state_from_backend(backend) == NULL) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            0
        );
        return false;
    }
    if (!cr_backend_internal_require_owner(backend, &backend_error)) {
        cr_epoll_set_net_error(
            out_error,
            backend_error.category == CR_BACKEND_ERROR_WRONG_THREAD
                ? CR_NET_ERROR_WRONG_THREAD
                : CR_NET_ERROR_INVALID_ARGUMENT,
            0
        );
        return false;
    }
    cr_epoll_clear_net_error(out_error);
    return true;
}

static bool cr_epoll_operation_has_magic(
    const cr_net_receive_operation *operation
) {
    uint64_t magic = UINT64_C(0);

    if (operation == NULL) return false;
    memcpy(&magic, operation, sizeof(magic));
    return magic == CR_EPOLL_OPERATION_MAGIC;
}

static bool cr_epoll_operation_belongs_to(
    const cr_net_receive_operation *operation,
    const cr_backend *backend
) {
    return cr_epoll_operation_has_magic(operation) &&
        operation->backend == backend;
}

static void cr_epoll_link_operation(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation
) {
    operation->previous = NULL;
    operation->next = state->active_head;
    if (state->active_head != NULL) {
        state->active_head->previous = operation;
    }
    state->active_head = operation;
    operation->linked = true;
}

static void cr_epoll_unlink_operation(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation
) {
    if (!operation->linked) return;
    if (operation->previous != NULL) {
        operation->previous->next = operation->next;
    } else {
        state->active_head = operation->next;
    }
    if (operation->next != NULL) {
        operation->next->previous = operation->previous;
    }
    operation->previous = NULL;
    operation->next = NULL;
    operation->linked = false;
}

static void cr_epoll_enqueue_cancel(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation
) {
    operation->cancel_previous = state->cancel_tail;
    operation->cancel_next = NULL;
    if (state->cancel_tail != NULL) {
        state->cancel_tail->cancel_next = operation;
    } else {
        state->cancel_head = operation;
    }
    state->cancel_tail = operation;
    operation->cancel_pending = true;
}

static void cr_epoll_remove_cancel(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation
) {
    if (!operation->cancel_pending) return;
    if (operation->cancel_previous != NULL) {
        operation->cancel_previous->cancel_next = operation->cancel_next;
    } else {
        state->cancel_head = operation->cancel_next;
    }
    if (operation->cancel_next != NULL) {
        operation->cancel_next->cancel_previous =
            operation->cancel_previous;
    } else {
        state->cancel_tail = operation->cancel_previous;
    }
    operation->cancel_previous = NULL;
    operation->cancel_next = NULL;
    operation->cancel_pending = false;
}

static bool cr_epoll_buffers_overlap(
    const cr_net_receive_operation *left,
    const cr_net_receive_operation *right
) {
    uintptr_t left_start = (uintptr_t)left->buffer;
    uintptr_t right_start = (uintptr_t)right->buffer;
    uintptr_t left_size = (uintptr_t)left->buffer_size;
    uintptr_t right_size = (uintptr_t)right->buffer_size;
    uintptr_t left_end;
    uintptr_t right_end;

    if (left_size > UINTPTR_MAX - left_start ||
        right_size > UINTPTR_MAX - right_start) {
        return true;
    }
    left_end = left_start + left_size;
    right_end = right_start + right_size;
    return left_start < right_end && right_start < left_end;
}

static bool cr_epoll_resource_is_busy(
    const cr_backend_epoll_state *state,
    const cr_net_receive_operation *candidate
) {
    const cr_net_receive_operation *operation = state->active_head;

    while (operation != NULL) {
        if (operation != candidate &&
            (operation->socket_fd == candidate->socket_fd ||
             cr_epoll_buffers_overlap(operation, candidate))) {
            return true;
        }
        operation = operation->next;
    }
    return false;
}

static cr_net_receive_operation *cr_epoll_find_operation(
    cr_backend_epoll_state *state,
    uint64_t event_token
) {
    cr_net_receive_operation *operation = state->active_head;

    while (operation != NULL) {
        if (operation->event_token == event_token &&
            operation->generation != UINT64_C(0)) {
            return operation;
        }
        operation = operation->next;
    }
    return NULL;
}

static bool cr_epoll_assign_token(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    if (state->token_exhausted) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INTERNAL_BACKEND,
            EOVERFLOW
        );
        return false;
    }
    operation->event_token = state->next_token;
    if (state->next_token == UINT64_MAX) {
        state->token_exhausted = true;
    } else {
        state->next_token++;
    }
    return true;
}

static void cr_epoll_clear_borrowed_fields(
    cr_net_receive_operation *operation
) {
    operation->event_token = UINT64_C(0);
    operation->socket_fd = -1;
    operation->buffer = NULL;
    operation->buffer_size = UINT64_C(0);
    operation->on_completion = NULL;
    operation->callback_context = NULL;
    operation->registered = false;
    operation->cancel_requested = false;
}

static void cr_epoll_make_quiescent(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation
) {
    cr_epoll_remove_cancel(state, operation);
    cr_epoll_unlink_operation(state, operation);
    cr_epoll_clear_borrowed_fields(operation);
    operation->state = CR_EPOLL_OPERATION_QUIESCENT;
}

static void cr_epoll_deliver_completion(
    cr_net_receive_operation *operation,
    cr_net_receive_terminal_kind terminal_kind,
    uint64_t bytes_transferred,
    cr_net_error_category error_category,
    int native_code
) {
    const cr_net_receive_completion completion = {
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_receive_completion),
        terminal_kind,
        error_category,
        bytes_transferred,
        native_code == 0
            ? CR_NATIVE_ERROR_DOMAIN_NONE
            : CR_NATIVE_ERROR_DOMAIN_ERRNO,
        UINT32_C(0),
        (int64_t)native_code
    };
    cr_net_receive_completion_fn callback = operation->on_completion;
    void *callback_context = operation->callback_context;

    operation->state = CR_EPOLL_OPERATION_TERMINAL;
    operation->callback_delivered = true;
    callback(callback_context, &completion);
}

static bool cr_epoll_signal_fd(int fd, int *out_error) {
    const uint64_t one = UINT64_C(1);
    ssize_t written;

    do {
        written = write(fd, &one, sizeof(one));
    } while (written < 0 && errno == EINTR);
    if (written == (ssize_t)sizeof(one) ||
        (written < 0 && errno == EAGAIN)) {
        return true;
    }
    *out_error = written < 0 ? errno : EIO;
    return false;
}

static bool cr_epoll_drain_fd(int fd, int *out_error) {
    uint64_t value;
    ssize_t received;

    do {
        received = read(fd, &value, sizeof(value));
    } while (received < 0 && errno == EINTR);
    if (received == (ssize_t)sizeof(value) ||
        (received < 0 && errno == EAGAIN)) {
        return true;
    }
    *out_error = received < 0 ? errno : EIO;
    return false;
}

static bool cr_epoll_remove_interest(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    if (!operation->registered) {
        cr_epoll_clear_net_error(out_error);
        return true;
    }
    if (epoll_ctl(
            state->epoll_fd,
            EPOLL_CTL_DEL,
            operation->socket_fd,
            NULL
        ) != 0 && errno != ENOENT) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_NETWORK_FAILURE,
            errno
        );
        return false;
    }
    operation->registered = false;
    cr_epoll_clear_net_error(out_error);
    return true;
}

static void cr_epoll_dispatch_one_cancel(
    cr_backend_epoll_state *state
) {
    cr_net_receive_operation *operation = state->cancel_head;

    if (operation == NULL) return;
    cr_epoll_remove_cancel(state, operation);
    cr_epoll_deliver_completion(
        operation,
        CR_NET_RECEIVE_CANCELED,
        UINT64_C(0),
        CR_NET_ERROR_NONE,
        0
    );
}

static bool cr_epoll_rearm(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation,
    int *out_error
) {
    struct epoll_event event;

    memset(&event, 0, sizeof(event));
    event.events = EPOLLIN | EPOLLRDHUP | EPOLLONESHOT;
    event.data.u64 = operation->event_token;
    if (epoll_ctl(
            state->epoll_fd,
            EPOLL_CTL_MOD,
            operation->socket_fd,
            &event
        ) != 0) {
        *out_error = errno;
        return false;
    }
    CR_BACKEND_EPOLL_REARMED(
        operation,
        operation->generation,
        operation->event_token
    );
    return true;
}

static void cr_epoll_dispatch_readiness(
    cr_backend_epoll_state *state,
    uint64_t event_token,
    uint32_t event_flags
) {
    cr_net_receive_operation *operation =
        cr_epoll_find_operation(state, event_token);
    ssize_t received;
    int native_error;
    int socket_error = 0;
    socklen_t socket_error_size = (socklen_t)sizeof(socket_error);

    if (operation == NULL ||
        operation->state != CR_EPOLL_OPERATION_SUBMITTED ||
        operation->event_token != event_token ||
        operation->cancel_requested) {
        return;
    }
    CR_BACKEND_EPOLL_BEFORE_RECV(
        operation,
        operation->generation,
        operation->socket_fd
    );
    do {
        received = recv(
            operation->socket_fd,
            operation->buffer,
            (size_t)operation->buffer_size,
            0
        );
    } while (received < 0 && errno == EINTR);
    if (received > 0) {
        cr_epoll_deliver_completion(
            operation,
            CR_NET_RECEIVE_READY,
            (uint64_t)received,
            CR_NET_ERROR_NONE,
            0
        );
        return;
    }
    if (received == 0) {
        cr_epoll_deliver_completion(
            operation,
            CR_NET_RECEIVE_READY,
            UINT64_C(0),
            CR_NET_ERROR_NONE,
            0
        );
        return;
    }
    native_error = errno;
    if (native_error == EAGAIN || native_error == EWOULDBLOCK) {
        if ((event_flags & EPOLLERR) != 0u &&
            getsockopt(
                operation->socket_fd,
                SOL_SOCKET,
                SO_ERROR,
                &socket_error,
                &socket_error_size
            ) == 0 && socket_error != 0) {
            cr_epoll_deliver_completion(
                operation,
                CR_NET_RECEIVE_ERROR,
                UINT64_C(0),
                CR_NET_ERROR_NETWORK_FAILURE,
                socket_error
            );
            return;
        }
        if (!cr_epoll_rearm(state, operation, &native_error)) {
            cr_epoll_deliver_completion(
                operation,
                CR_NET_RECEIVE_ERROR,
                UINT64_C(0),
                CR_NET_ERROR_NETWORK_FAILURE,
                native_error
            );
        }
        return;
    }
    cr_epoll_deliver_completion(
        operation,
        CR_NET_RECEIVE_ERROR,
        UINT64_C(0),
        CR_NET_ERROR_NETWORK_FAILURE,
        native_error
    );
}

static bool cr_epoll_request_cancel(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    int native_error;

    if (operation->cancel_requested) {
        cr_epoll_clear_net_error(out_error);
        return true;
    }
    if (!cr_epoll_remove_interest(state, operation, out_error)) {
        return false;
    }
    operation->cancel_requested = true;
    cr_epoll_enqueue_cancel(state, operation);
    if (!cr_epoll_signal_fd(state->completion_fd, &native_error)) {
        cr_epoll_remove_cancel(state, operation);
        operation->cancel_requested = false;
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INTERNAL_BACKEND,
            native_error
        );
        return false;
    }
    cr_epoll_clear_net_error(out_error);
    return true;
}

static bool cr_epoll_quiesce_operation(
    cr_backend_epoll_state *state,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    if (operation->state == CR_EPOLL_OPERATION_QUIESCENT) {
        cr_epoll_clear_net_error(out_error);
        return true;
    }
    if (operation->state == CR_EPOLL_OPERATION_INITIALIZED) {
        cr_epoll_make_quiescent(state, operation);
        cr_epoll_clear_net_error(out_error);
        return true;
    }
    if (operation->state == CR_EPOLL_OPERATION_SUBMITTED &&
        !cr_epoll_request_cancel(state, operation, out_error)) {
        return false;
    }
    while (operation->state == CR_EPOLL_OPERATION_SUBMITTED) {
        int native_error;

        if (!cr_epoll_drain_fd(state->completion_fd, &native_error)) {
            cr_epoll_set_net_error(
                out_error,
                CR_NET_ERROR_INTERNAL_BACKEND,
                native_error
            );
            return false;
        }
        if (state->cancel_head == NULL) {
            cr_epoll_set_net_error(
                out_error,
                CR_NET_ERROR_INTERNAL_BACKEND,
                EIO
            );
            return false;
        }
        while (state->cancel_head != NULL &&
               operation->state == CR_EPOLL_OPERATION_SUBMITTED) {
            cr_epoll_dispatch_one_cancel(state);
        }
        if (state->cancel_head != NULL &&
            !cr_epoll_signal_fd(state->completion_fd, &native_error)) {
            cr_epoll_set_net_error(
                out_error,
                CR_NET_ERROR_INTERNAL_BACKEND,
                native_error
            );
            return false;
        }
    }
    if (operation->state != CR_EPOLL_OPERATION_TERMINAL ||
        !operation->callback_delivered ||
        !cr_epoll_remove_interest(state, operation, out_error)) {
        if (out_error == NULL || out_error->category == CR_NET_ERROR_NONE) {
            cr_epoll_set_net_error(
                out_error,
                CR_NET_ERROR_INTERNAL_BACKEND,
                EIO
            );
        }
        return false;
    }
    cr_epoll_make_quiescent(state, operation);
    cr_epoll_clear_net_error(out_error);
    return true;
}

static bool cr_epoll_receive_initialize(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    uint64_t operation_storage_size,
    cr_native_socket_handle socket,
    void *buffer,
    uint64_t buffer_size,
    cr_net_receive_completion_fn on_completion,
    void *callback_context,
    cr_net_error *out_error
) {
    cr_backend_epoll_state *state;
    uint64_t generation = UINT64_C(1);
    int descriptor_flags;
    struct sockaddr_storage peer;
    socklen_t peer_size = (socklen_t)sizeof(peer);

    if (!cr_epoll_require_owner(backend, out_error)) return false;
    state = cr_epoll_state_from_backend(backend);
    if (cr_backend_internal_is_closed(backend) || state->shutdown) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_CLOSED_BACKEND,
            0
        );
        return false;
    }
    if (operation == NULL ||
        operation_storage_size < sizeof(cr_net_receive_operation) ||
        ((uintptr_t)operation % _Alignof(cr_net_receive_operation)) != 0u ||
        socket.kind != CR_NATIVE_SOCKET_POSIX_FD ||
        socket.reserved != UINT32_C(0) ||
        socket.value > (uintptr_t)INT_MAX ||
        buffer == NULL || buffer_size == UINT64_C(0) ||
        buffer_size > (uint64_t)(SIZE_MAX >> 1) ||
        on_completion == NULL) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            0
        );
        return false;
    }
    descriptor_flags = fcntl((int)socket.value, F_GETFL, 0);
    if (descriptor_flags < 0) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            errno
        );
        return false;
    }
    if ((descriptor_flags & O_NONBLOCK) == 0) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            EINVAL
        );
        return false;
    }
    if (getpeername(
            (int)socket.value,
            (struct sockaddr *)&peer,
            &peer_size
        ) != 0) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            errno
        );
        return false;
    }
    if (cr_epoll_operation_has_magic(operation)) {
        if (operation->state != CR_EPOLL_OPERATION_QUIESCENT) {
            cr_epoll_set_net_error(out_error, CR_NET_ERROR_BUSY, 0);
            return false;
        }
        generation = operation->generation + UINT64_C(1);
        if (generation == UINT64_C(0)) generation = UINT64_C(1);
    }
    memset(operation, 0, sizeof(*operation));
    operation->magic = CR_EPOLL_OPERATION_MAGIC;
    operation->generation = generation;
    operation->state = CR_EPOLL_OPERATION_INITIALIZED;
    operation->backend = backend;
    operation->socket_fd = (int)socket.value;
    operation->buffer = buffer;
    operation->buffer_size = buffer_size;
    operation->on_completion = on_completion;
    operation->callback_context = callback_context;
    cr_epoll_clear_net_error(out_error);
    return true;
}

static bool cr_epoll_receive_submit(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    cr_backend_epoll_state *state;
    struct epoll_event event;

    if (!cr_epoll_require_owner(backend, out_error)) return false;
    state = cr_epoll_state_from_backend(backend);
    if (!cr_epoll_operation_belongs_to(operation, backend) ||
        operation->state != CR_EPOLL_OPERATION_INITIALIZED) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            0
        );
        return false;
    }
    if (cr_backend_internal_is_closed(backend) || state->shutdown) {
        cr_epoll_make_quiescent(state, operation);
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_CLOSED_BACKEND,
            0
        );
        return false;
    }
    if (cr_epoll_resource_is_busy(state, operation)) {
        cr_epoll_make_quiescent(state, operation);
        cr_epoll_set_net_error(out_error, CR_NET_ERROR_BUSY, 0);
        return false;
    }
    if (!cr_epoll_assign_token(state, operation, out_error)) {
        cr_epoll_make_quiescent(state, operation);
        return false;
    }
    memset(&event, 0, sizeof(event));
    event.events = EPOLLIN | EPOLLRDHUP | EPOLLONESHOT;
    event.data.u64 = operation->event_token;
    if (epoll_ctl(
            state->epoll_fd,
            EPOLL_CTL_ADD,
            operation->socket_fd,
            &event
        ) != 0) {
        int native_error = errno;
        cr_epoll_make_quiescent(state, operation);
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_NETWORK_FAILURE,
            native_error
        );
        return false;
    }
    operation->registered = true;
    cr_epoll_link_operation(state, operation);
    operation->state = CR_EPOLL_OPERATION_SUBMITTED;
    CR_BACKEND_EPOLL_SUBMIT_OBSERVED(
        operation,
        operation->generation,
        operation->event_token
    );
    cr_epoll_clear_net_error(out_error);
    return true;
}

static bool cr_epoll_receive_cancel(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    cr_backend_epoll_state *state;

    if (!cr_epoll_require_owner(backend, out_error)) return false;
    state = cr_epoll_state_from_backend(backend);
    if (!cr_epoll_operation_belongs_to(operation, backend) ||
        operation->state != CR_EPOLL_OPERATION_SUBMITTED) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            0
        );
        return false;
    }
    return cr_epoll_request_cancel(state, operation, out_error);
}

static bool cr_epoll_receive_quiesce(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    cr_backend_epoll_state *state;

    if (!cr_epoll_require_owner(backend, out_error)) return false;
    state = cr_epoll_state_from_backend(backend);
    if (!cr_epoll_operation_belongs_to(operation, backend)) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            0
        );
        return false;
    }
    return cr_epoll_quiesce_operation(state, operation, out_error);
}

static bool cr_epoll_receive_destroy(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    if (!cr_epoll_require_owner(backend, out_error)) return false;
    if (!cr_epoll_operation_belongs_to(operation, backend) ||
        operation->state != CR_EPOLL_OPERATION_QUIESCENT) {
        cr_epoll_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            0
        );
        return false;
    }
    operation->state = CR_EPOLL_OPERATION_DESTROYED;
    operation->backend = NULL;
    cr_epoll_clear_net_error(out_error);
    return true;
}

static bool cr_epoll_add_control_fd(
    cr_backend_epoll_state *state,
    int fd,
    uint64_t token,
    cr_backend_error *out_error
) {
    struct epoll_event event;

    memset(&event, 0, sizeof(event));
    event.events = EPOLLIN;
    event.data.u64 = token;
    if (epoll_ctl(state->epoll_fd, EPOLL_CTL_ADD, fd, &event) != 0) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INTERNAL,
            CR_NATIVE_ERROR_DOMAIN_ERRNO,
            (int64_t)errno
        );
        return false;
    }
    return true;
}

static void cr_epoll_close_fd(int *fd) {
    if (*fd < 0) return;
    if (close(*fd) == 0) {
        CR_BACKEND_EPOLL_FD_CLOSED(*fd);
    }
    *fd = -1;
}

static bool cr_epoll_provider_create(
    const cr_backend_provider_desc *provider,
    void **out_provider_state,
    cr_backend_error *out_error
) {
    cr_backend_epoll_state *state;

    cr_backend_internal_clear_error(out_error);
    if (provider != &cr_backend_epoll_provider_desc ||
        out_provider_state == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    *out_provider_state = NULL;
    state = (cr_backend_epoll_state *)
        CR_BACKEND_EPOLL_CALLOC(1u, sizeof(*state));
    if (state == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_OUT_OF_MEMORY,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    state->epoll_fd = -1;
    state->interrupt_fd = -1;
    state->completion_fd = -1;
    state->next_token = CR_EPOLL_FIRST_OPERATION_TOKEN;
    state->epoll_fd = epoll_create1(EPOLL_CLOEXEC);
    if (state->epoll_fd >= 0) {
        CR_BACKEND_EPOLL_FD_OPENED(state->epoll_fd);
    }
    state->interrupt_fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (state->interrupt_fd >= 0) {
        CR_BACKEND_EPOLL_FD_OPENED(state->interrupt_fd);
    }
    state->completion_fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
    if (state->completion_fd >= 0) {
        CR_BACKEND_EPOLL_FD_OPENED(state->completion_fd);
    }
    if (state->epoll_fd < 0 || state->interrupt_fd < 0 ||
        state->completion_fd < 0 ||
        !cr_epoll_add_control_fd(
            state,
            state->interrupt_fd,
            CR_EPOLL_INTERRUPT_TOKEN,
            out_error
        ) ||
        !cr_epoll_add_control_fd(
            state,
            state->completion_fd,
            CR_EPOLL_CANCEL_TOKEN,
            out_error
        )) {
        int native_error = errno;
        cr_epoll_close_fd(&state->completion_fd);
        cr_epoll_close_fd(&state->interrupt_fd);
        cr_epoll_close_fd(&state->epoll_fd);
        CR_BACKEND_EPOLL_FREE(state);
        if (out_error == NULL || out_error->category == CR_BACKEND_ERROR_NONE) {
            cr_backend_internal_set_error(
                out_error,
                CR_BACKEND_ERROR_INTERNAL,
                CR_NATIVE_ERROR_DOMAIN_ERRNO,
                (int64_t)native_error
            );
        }
        return false;
    }
    *out_provider_state = state;
    return true;
}

static const cr_backend_extension_desc *cr_epoll_provider_query_extension(
    void *provider_state,
    cr_extension_id extension_id,
    uint32_t requested_abi_version,
    cr_backend_error *out_error
) {
    const cr_extension_id expected = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    cr_backend_epoll_state *state =
        (cr_backend_epoll_state *)provider_state;

    cr_backend_internal_clear_error(out_error);
    if (state == NULL || state->shutdown) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_CLOSED,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return NULL;
    }
    if (!cr_extension_id_equal(extension_id, expected) ||
        requested_abi_version > CR_NET_EXPERIMENTAL_ABI_VERSION) {
        return NULL;
    }
    return &cr_backend_epoll_net_extension.base;
}

static int cr_epoll_timeout_ms(uint64_t timeout_ns) {
    uint64_t milliseconds;

    if (timeout_ns == UINT64_MAX) return -1;
    milliseconds = timeout_ns / UINT64_C(1000000);
    if (timeout_ns % UINT64_C(1000000) != UINT64_C(0)) {
        milliseconds++;
    }
    if (milliseconds > (uint64_t)INT_MAX) return INT_MAX;
    return (int)milliseconds;
}

static uint64_t cr_epoll_elapsed_ns(
    const struct timespec *start,
    const struct timespec *now
) {
    uint64_t seconds;
    int64_t nanoseconds;

    if (now->tv_sec < start->tv_sec) return UINT64_C(0);
    seconds = (uint64_t)(now->tv_sec - start->tv_sec);
    nanoseconds = (int64_t)now->tv_nsec - (int64_t)start->tv_nsec;
    if (nanoseconds < INT64_C(0)) {
        if (seconds == UINT64_C(0)) return UINT64_C(0);
        seconds--;
        nanoseconds += INT64_C(1000000000);
    }
    if (seconds > UINT64_MAX / UINT64_C(1000000000)) {
        return UINT64_MAX;
    }
    seconds *= UINT64_C(1000000000);
    if (seconds > UINT64_MAX - (uint64_t)nanoseconds) {
        return UINT64_MAX;
    }
    return seconds + (uint64_t)nanoseconds;
}

static int cr_epoll_wait_retry(
    int epoll_fd,
    struct epoll_event *events,
    int capacity,
    uint64_t timeout_ns,
    int *out_native_error
) {
    struct timespec start;
    bool finite_wait = timeout_ns != UINT64_MAX &&
        timeout_ns != UINT64_C(0);
    int timeout_ms = cr_epoll_timeout_ms(timeout_ns);

    if (finite_wait && clock_gettime(CLOCK_MONOTONIC, &start) != 0) {
        *out_native_error = errno;
        return -1;
    }
    for (;;) {
        int event_count = epoll_wait(
            epoll_fd,
            events,
            capacity,
            timeout_ms
        );
        if (event_count >= 0) return event_count;
        if (errno != EINTR) {
            *out_native_error = errno;
            return -1;
        }
        if (finite_wait) {
            struct timespec now;
            uint64_t elapsed;

            if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
                *out_native_error = errno;
                return -1;
            }
            elapsed = cr_epoll_elapsed_ns(&start, &now);
            if (elapsed >= timeout_ns) return 0;
            timeout_ms = cr_epoll_timeout_ms(timeout_ns - elapsed);
        }
    }
}

static bool cr_epoll_provider_pump(
    void *provider_state,
    uint64_t timeout_ns,
    uint32_t max_events,
    cr_backend_pump_result *out_result
) {
    cr_backend_epoll_state *state =
        (cr_backend_epoll_state *)provider_state;
    struct epoll_event events[CR_EPOLL_BATCH_SIZE];
    uint32_t dispatched = UINT32_C(0);
    uint32_t operation_events = UINT32_C(0);
    uint32_t interrupt_events = UINT32_C(0);

    if (state == NULL || state->shutdown) {
        *out_result = (cr_backend_pump_result){
            CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
            sizeof(cr_backend_pump_result),
            CR_BACKEND_PUMP_ERROR,
            UINT32_C(0),
            CR_BACKEND_ERROR_CLOSED,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        };
        return false;
    }
    while (dispatched < max_events) {
        uint32_t remaining = max_events - dispatched;
        int capacity = remaining < CR_EPOLL_BATCH_SIZE
            ? (int)remaining
            : CR_EPOLL_BATCH_SIZE;
        int event_count;
        int index;

        int wait_error = 0;

        event_count = cr_epoll_wait_retry(
            state->epoll_fd,
            events,
            capacity,
            dispatched == UINT32_C(0)
                ? timeout_ns
                : UINT64_C(0),
            &wait_error
        );
        if (event_count < 0) {
            *out_result = (cr_backend_pump_result){
                CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
                sizeof(cr_backend_pump_result),
                CR_BACKEND_PUMP_ERROR,
                dispatched,
                CR_BACKEND_ERROR_INTERNAL,
                CR_NATIVE_ERROR_DOMAIN_ERRNO,
                (int64_t)wait_error
            };
            return false;
        }
        if (event_count == 0) break;
        for (index = 0; index < event_count; index++) {
            uint64_t token = CR_BACKEND_EPOLL_FILTER_EVENT_TOKEN(
                events[index].data.u64
            );

            dispatched++;
            if (token == CR_EPOLL_INTERRUPT_TOKEN) {
                int native_error;
                if (!cr_epoll_drain_fd(state->interrupt_fd, &native_error)) {
                    *out_result = (cr_backend_pump_result){
                        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
                        sizeof(cr_backend_pump_result),
                        CR_BACKEND_PUMP_ERROR,
                        dispatched,
                        CR_BACKEND_ERROR_INTERNAL,
                        CR_NATIVE_ERROR_DOMAIN_ERRNO,
                        (int64_t)native_error
                    };
                    return false;
                }
                interrupt_events++;
            } else if (token == CR_EPOLL_CANCEL_TOKEN) {
                int native_error;
                if (!cr_epoll_drain_fd(state->completion_fd, &native_error)) {
                    *out_result = (cr_backend_pump_result){
                        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
                        sizeof(cr_backend_pump_result),
                        CR_BACKEND_PUMP_ERROR,
                        dispatched,
                        CR_BACKEND_ERROR_INTERNAL,
                        CR_NATIVE_ERROR_DOMAIN_ERRNO,
                        (int64_t)native_error
                    };
                    return false;
                }
                cr_epoll_dispatch_one_cancel(state);
                if (state->cancel_head != NULL &&
                    !cr_epoll_signal_fd(state->completion_fd, &native_error)) {
                    *out_result = (cr_backend_pump_result){
                        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
                        sizeof(cr_backend_pump_result),
                        CR_BACKEND_PUMP_ERROR,
                        dispatched,
                        CR_BACKEND_ERROR_INTERNAL,
                        CR_NATIVE_ERROR_DOMAIN_ERRNO,
                        (int64_t)native_error
                    };
                    return false;
                }
                operation_events++;
            } else {
                operation_events++;
                cr_epoll_dispatch_readiness(
                    state,
                    token,
                    events[index].events
                );
            }
        }
    }
    *out_result = (cr_backend_pump_result){
        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_backend_pump_result),
        operation_events != UINT32_C(0)
            ? CR_BACKEND_PUMP_PROGRESS
            : interrupt_events != UINT32_C(0)
                ? CR_BACKEND_PUMP_INTERRUPTED
                : CR_BACKEND_PUMP_TIMEOUT,
        dispatched,
        CR_BACKEND_ERROR_NONE,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    };
    return true;
}

static bool cr_epoll_provider_interrupt(
    void *provider_state,
    cr_backend_error *out_error
) {
    cr_backend_epoll_state *state =
        (cr_backend_epoll_state *)provider_state;
    int native_error;

    cr_backend_internal_clear_error(out_error);
    if (state == NULL || state->shutdown) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_CLOSED,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (!cr_epoll_signal_fd(state->interrupt_fd, &native_error)) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INTERNAL,
            CR_NATIVE_ERROR_DOMAIN_ERRNO,
            (int64_t)native_error
        );
        return false;
    }
    return true;
}

static bool cr_epoll_provider_shutdown(
    void *provider_state,
    cr_backend_error *out_error
) {
    cr_backend_epoll_state *state =
        (cr_backend_epoll_state *)provider_state;

    cr_backend_internal_clear_error(out_error);
    if (state == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (state->shutdown) return true;
    while (state->active_head != NULL) {
        cr_net_error operation_error;
        if (!cr_epoll_quiesce_operation(
                state,
                state->active_head,
                &operation_error
            )) {
            cr_backend_internal_set_error(
                out_error,
                CR_BACKEND_ERROR_INTERNAL,
                operation_error.native_domain,
                operation_error.native_code
            );
            return false;
        }
    }
    state->shutdown = true;
    return true;
}

static void cr_epoll_provider_destroy(void *provider_state) {
    cr_backend_epoll_state *state =
        (cr_backend_epoll_state *)provider_state;

    if (state == NULL) return;
    cr_epoll_close_fd(&state->completion_fd);
    cr_epoll_close_fd(&state->interrupt_fd);
    cr_epoll_close_fd(&state->epoll_fd);
    CR_BACKEND_EPOLL_FREE(state);
}

static const cr_net_extension_desc cr_backend_epoll_net_extension = {
    {
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_extension_desc),
        UINT64_C(0),
        CR_NET_RECEIVE_EXTENSION_ID_INIT
    },
    {
        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_storage_layout),
        sizeof(cr_net_receive_operation),
        _Alignof(cr_net_receive_operation)
    },
    cr_epoll_receive_initialize,
    cr_epoll_receive_submit,
    cr_epoll_receive_cancel,
    cr_epoll_receive_quiesce,
    cr_epoll_receive_destroy
};

const cr_backend_provider_desc cr_backend_epoll_provider_desc = {
    CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
    sizeof(cr_backend_provider_desc),
    UINT64_C(0),
    CR_BACKEND_CORE_ID_INIT,
    cr_epoll_provider_create,
    cr_epoll_provider_query_extension,
    cr_epoll_provider_pump,
    cr_epoll_provider_interrupt,
    cr_epoll_provider_shutdown,
    cr_epoll_provider_destroy
};
"#;
