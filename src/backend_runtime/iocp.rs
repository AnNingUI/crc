pub(crate) const IOCP_SOURCE: &str = r#"#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0600
#endif

#include <winsock2.h>
#include <windows.h>

#include "cr_backend_internal.h"

#include <limits.h>
#include <string.h>

#define CR_IOCP_OPERATION_MAGIC UINT64_C(0x4352494f43504f31)

#define CR_IOCP_OPERATION_INITIALIZED UINT32_C(1)
#define CR_IOCP_OPERATION_SUBMITTED   UINT32_C(2)
#define CR_IOCP_OPERATION_TERMINAL    UINT32_C(3)
#define CR_IOCP_OPERATION_QUIESCENT   UINT32_C(4)
#define CR_IOCP_OPERATION_DESTROYED   UINT32_C(5)

#ifndef CR_BACKEND_IOCP_HANDLE_OPENED
#define CR_BACKEND_IOCP_HANDLE_OPENED(handle) ((void)(handle))
#endif

#ifndef CR_BACKEND_IOCP_HANDLE_CLOSED
#define CR_BACKEND_IOCP_HANDLE_CLOSED(handle) ((void)(handle))
#endif

#ifndef CR_BACKEND_IOCP_SUBMIT_OBSERVED
#define CR_BACKEND_IOCP_SUBMIT_OBSERVED(operation, completed_inline) \
    ((void)(operation), (void)(completed_inline))
#endif

typedef struct cr_backend_iocp_state cr_backend_iocp_state;

struct cr_net_receive_operation {
    OVERLAPPED overlapped;
    uint64_t magic;
    uint64_t generation;
    uint32_t state;
    bool linked;
    bool cancel_requested;
    bool callback_delivered;
    cr_backend *backend;
    SOCKET socket;
    void *buffer;
    uint64_t buffer_size;
    WSABUF wsabuf;
    cr_net_receive_completion_fn on_completion;
    void *callback_context;
    cr_net_receive_operation *previous;
    cr_net_receive_operation *next;
};

struct cr_backend_iocp_state {
    HANDLE port;
    volatile LONG interrupted;
    volatile LONG shutdown;
    cr_net_receive_operation *active_head;
};

static const cr_net_extension_desc cr_backend_iocp_net_extension;

static void cr_iocp_clear_net_error(cr_net_error *out_error) {
    if (out_error == NULL) return;
    *out_error = (cr_net_error){
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_error),
        CR_NET_ERROR_NONE,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    };
}

static void cr_iocp_set_net_error(
    cr_net_error *out_error,
    cr_net_error_category category,
    cr_native_error_domain native_domain,
    int64_t native_code
) {
    if (out_error == NULL) return;
    *out_error = (cr_net_error){
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_error),
        category,
        native_domain,
        native_code
    };
}

static cr_backend_iocp_state *cr_iocp_state_from_backend(
    const cr_backend *backend
) {
    if (backend == NULL ||
        cr_backend_internal_provider(backend) !=
            &cr_backend_iocp_provider_desc) {
        return NULL;
    }
    return (cr_backend_iocp_state *)
        cr_backend_internal_provider_state(backend);
}

static bool cr_iocp_require_owner(
    cr_backend *backend,
    cr_net_error *out_error
) {
    cr_backend_error backend_error;

    if (cr_iocp_state_from_backend(backend) == NULL) {
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (!cr_backend_internal_require_owner(backend, &backend_error)) {
        cr_iocp_set_net_error(
            out_error,
            backend_error.category == CR_BACKEND_ERROR_WRONG_THREAD
                ? CR_NET_ERROR_WRONG_THREAD
                : CR_NET_ERROR_INVALID_ARGUMENT,
            backend_error.native_domain,
            backend_error.native_code
        );
        return false;
    }
    cr_iocp_clear_net_error(out_error);
    return true;
}

static bool cr_iocp_operation_has_magic(
    const cr_net_receive_operation *operation
) {
    uint64_t magic = UINT64_C(0);

    if (operation == NULL) return false;
    memcpy(
        &magic,
        (const unsigned char *)operation +
            offsetof(cr_net_receive_operation, magic),
        sizeof(magic)
    );
    return magic == CR_IOCP_OPERATION_MAGIC;
}

static bool cr_iocp_operation_belongs_to(
    const cr_net_receive_operation *operation,
    const cr_backend *backend
) {
    return cr_iocp_operation_has_magic(operation) &&
        operation->backend == backend;
}

static void cr_iocp_link_operation(
    cr_backend_iocp_state *state,
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

static void cr_iocp_unlink_operation(
    cr_backend_iocp_state *state,
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

static bool cr_iocp_buffers_overlap(
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

static bool cr_iocp_resource_is_busy(
    const cr_backend_iocp_state *state,
    const cr_net_receive_operation *candidate
) {
    const cr_net_receive_operation *operation = state->active_head;

    while (operation != NULL) {
        if (operation != candidate &&
            (operation->socket == candidate->socket ||
             cr_iocp_buffers_overlap(operation, candidate))) {
            return true;
        }
        operation = operation->next;
    }
    return false;
}

static void cr_iocp_clear_borrowed_fields(
    cr_net_receive_operation *operation
) {
    operation->socket = INVALID_SOCKET;
    operation->buffer = NULL;
    operation->buffer_size = UINT64_C(0);
    operation->wsabuf.buf = NULL;
    operation->wsabuf.len = 0;
    operation->on_completion = NULL;
    operation->callback_context = NULL;
    operation->cancel_requested = false;
}

static void cr_iocp_make_quiescent(
    cr_backend_iocp_state *state,
    cr_net_receive_operation *operation
) {
    cr_iocp_unlink_operation(state, operation);
    cr_iocp_clear_borrowed_fields(operation);
    operation->state = CR_IOCP_OPERATION_QUIESCENT;
}

static void cr_iocp_deliver_completion(
    cr_net_receive_operation *operation,
    cr_net_receive_terminal_kind terminal_kind,
    uint64_t bytes_transferred,
    cr_net_error_category error_category,
    cr_native_error_domain native_domain,
    int64_t native_code
) {
    const cr_net_receive_completion completion = {
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_receive_completion),
        terminal_kind,
        error_category,
        bytes_transferred,
        native_domain,
        UINT32_C(0),
        native_code
    };
    cr_net_receive_completion_fn callback = operation->on_completion;
    void *callback_context = operation->callback_context;

    operation->state = CR_IOCP_OPERATION_TERMINAL;
    operation->callback_delivered = true;
    callback(callback_context, &completion);
}

static bool cr_iocp_dispatch_packet(
    cr_backend_iocp_state *state,
    DWORD bytes_transferred,
    ULONG_PTR completion_key,
    OVERLAPPED *overlapped,
    DWORD native_error,
    bool *out_operation_event,
    bool *out_interrupt_event,
    cr_backend_error *out_error
) {
    cr_net_receive_operation *operation;

    *out_operation_event = false;
    *out_interrupt_event = false;
    cr_backend_internal_clear_error(out_error);
    if (overlapped == NULL) {
        if (completion_key != (ULONG_PTR)state) {
            cr_backend_internal_set_error(
                out_error,
                CR_BACKEND_ERROR_INTERNAL,
                CR_NATIVE_ERROR_DOMAIN_WIN32,
                (int64_t)ERROR_INVALID_DATA
            );
            return false;
        }
        InterlockedExchange(&state->interrupted, 0);
        *out_interrupt_event = true;
        return true;
    }
    if (completion_key != (ULONG_PTR)0) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INTERNAL,
            CR_NATIVE_ERROR_DOMAIN_WIN32,
            (int64_t)ERROR_INVALID_DATA
        );
        return false;
    }
    operation = (cr_net_receive_operation *)(
        (unsigned char *)overlapped -
        offsetof(cr_net_receive_operation, overlapped)
    );
    if (!cr_iocp_operation_has_magic(operation) ||
        operation->state != CR_IOCP_OPERATION_SUBMITTED ||
        cr_iocp_state_from_backend(operation->backend) != state) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INTERNAL,
            CR_NATIVE_ERROR_DOMAIN_WIN32,
            (int64_t)ERROR_INVALID_DATA
        );
        return false;
    }
    *out_operation_event = true;
    if (native_error == ERROR_SUCCESS) {
        cr_iocp_deliver_completion(
            operation,
            CR_NET_RECEIVE_READY,
            (uint64_t)bytes_transferred,
            CR_NET_ERROR_NONE,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
    } else if (operation->cancel_requested &&
               native_error == ERROR_OPERATION_ABORTED) {
        cr_iocp_deliver_completion(
            operation,
            CR_NET_RECEIVE_CANCELED,
            UINT64_C(0),
            CR_NET_ERROR_NONE,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
    } else {
        cr_iocp_deliver_completion(
            operation,
            CR_NET_RECEIVE_ERROR,
            UINT64_C(0),
            CR_NET_ERROR_NETWORK_FAILURE,
            CR_NATIVE_ERROR_DOMAIN_WINSOCK,
            (int64_t)native_error
        );
    }
    return true;
}

static bool cr_iocp_wait_and_dispatch(
    cr_backend_iocp_state *state,
    DWORD timeout_ms,
    bool *out_timed_out,
    bool *out_operation_event,
    bool *out_interrupt_event,
    cr_backend_error *out_error
) {
    DWORD bytes_transferred = 0;
    ULONG_PTR completion_key = (ULONG_PTR)0;
    OVERLAPPED *overlapped = NULL;
    BOOL success;
    DWORD native_error;

    *out_timed_out = false;
    success = GetQueuedCompletionStatus(
        state->port,
        &bytes_transferred,
        &completion_key,
        &overlapped,
        timeout_ms
    );
    if (!success && overlapped == NULL) {
        native_error = GetLastError();
        if (native_error == WAIT_TIMEOUT) {
            *out_timed_out = true;
            cr_backend_internal_clear_error(out_error);
            return true;
        }
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INTERNAL,
            CR_NATIVE_ERROR_DOMAIN_WIN32,
            (int64_t)native_error
        );
        return false;
    }
    native_error = success ? ERROR_SUCCESS : GetLastError();
    return cr_iocp_dispatch_packet(
        state,
        bytes_transferred,
        completion_key,
        overlapped,
        native_error,
        out_operation_event,
        out_interrupt_event,
        out_error
    );
}

static bool cr_iocp_request_cancel(
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    BOOL canceled;
    DWORD native_error;

    if (operation->cancel_requested) {
        cr_iocp_clear_net_error(out_error);
        return true;
    }
    canceled = CancelIoEx(
        (HANDLE)(uintptr_t)operation->socket,
        &operation->overlapped
    );
    if (!canceled) {
        native_error = GetLastError();
        if (native_error != ERROR_NOT_FOUND) {
            cr_iocp_set_net_error(
                out_error,
                CR_NET_ERROR_NETWORK_FAILURE,
                CR_NATIVE_ERROR_DOMAIN_WIN32,
                (int64_t)native_error
            );
            return false;
        }
    }
    operation->cancel_requested = true;
    cr_iocp_clear_net_error(out_error);
    return true;
}

static bool cr_iocp_quiesce_operation(
    cr_backend_iocp_state *state,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    if (operation->state == CR_IOCP_OPERATION_QUIESCENT) {
        cr_iocp_clear_net_error(out_error);
        return true;
    }
    if (operation->state == CR_IOCP_OPERATION_INITIALIZED) {
        cr_iocp_make_quiescent(state, operation);
        cr_iocp_clear_net_error(out_error);
        return true;
    }
    if (operation->state == CR_IOCP_OPERATION_SUBMITTED &&
        !cr_iocp_request_cancel(operation, out_error)) {
        return false;
    }
    while (operation->state == CR_IOCP_OPERATION_SUBMITTED) {
        bool timed_out;
        bool operation_event;
        bool interrupt_event;
        cr_backend_error backend_error;

        if (!cr_iocp_wait_and_dispatch(
                state,
                INFINITE,
                &timed_out,
                &operation_event,
                &interrupt_event,
                &backend_error
            )) {
            cr_iocp_set_net_error(
                out_error,
                CR_NET_ERROR_INTERNAL_BACKEND,
                backend_error.native_domain,
                backend_error.native_code
            );
            return false;
        }
        (void)timed_out;
        (void)operation_event;
        (void)interrupt_event;
    }
    if (operation->state != CR_IOCP_OPERATION_TERMINAL ||
        !operation->callback_delivered) {
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_INTERNAL_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_WIN32,
            (int64_t)ERROR_INVALID_STATE
        );
        return false;
    }
    cr_iocp_make_quiescent(state, operation);
    cr_iocp_clear_net_error(out_error);
    return true;
}

static bool cr_iocp_receive_initialize(
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
    cr_backend_iocp_state *state;
    uint64_t generation = UINT64_C(1);

    if (!cr_iocp_require_owner(backend, out_error)) return false;
    state = cr_iocp_state_from_backend(backend);
    if (cr_backend_internal_is_closed(backend) ||
        InterlockedCompareExchange(&state->shutdown, 0, 0) != 0) {
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_CLOSED_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (operation == NULL ||
        operation_storage_size < sizeof(cr_net_receive_operation) ||
        ((uintptr_t)operation % _Alignof(cr_net_receive_operation)) != 0u ||
        socket.kind != CR_NATIVE_SOCKET_WINSOCK ||
        socket.reserved != UINT32_C(0) ||
        socket.value == (uintptr_t)INVALID_SOCKET ||
        buffer == NULL || buffer_size == UINT64_C(0) ||
        buffer_size > (uint64_t)ULONG_MAX ||
        on_completion == NULL) {
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (cr_iocp_operation_has_magic(operation)) {
        if (operation->state != CR_IOCP_OPERATION_QUIESCENT) {
            cr_iocp_set_net_error(
                out_error,
                CR_NET_ERROR_BUSY,
                CR_NATIVE_ERROR_DOMAIN_NONE,
                INT64_C(0)
            );
            return false;
        }
        generation = operation->generation + UINT64_C(1);
        if (generation == UINT64_C(0)) generation = UINT64_C(1);
    }
    memset(operation, 0, sizeof(*operation));
    operation->magic = CR_IOCP_OPERATION_MAGIC;
    operation->generation = generation;
    operation->state = CR_IOCP_OPERATION_INITIALIZED;
    operation->backend = backend;
    operation->socket = (SOCKET)socket.value;
    operation->buffer = buffer;
    operation->buffer_size = buffer_size;
    operation->wsabuf.buf = (CHAR *)buffer;
    operation->wsabuf.len = (ULONG)buffer_size;
    operation->on_completion = on_completion;
    operation->callback_context = callback_context;
    cr_iocp_clear_net_error(out_error);
    return true;
}

static bool cr_iocp_receive_submit(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    cr_backend_iocp_state *state;
    HANDLE associated;
    DWORD flags = 0;
    DWORD bytes_received = 0;
    int receive_result;
    int native_error;

    if (!cr_iocp_require_owner(backend, out_error)) return false;
    state = cr_iocp_state_from_backend(backend);
    if (!cr_iocp_operation_belongs_to(operation, backend) ||
        operation->state != CR_IOCP_OPERATION_INITIALIZED) {
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (cr_backend_internal_is_closed(backend) ||
        InterlockedCompareExchange(&state->shutdown, 0, 0) != 0) {
        cr_iocp_make_quiescent(state, operation);
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_CLOSED_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (cr_iocp_resource_is_busy(state, operation)) {
        cr_iocp_make_quiescent(state, operation);
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_BUSY,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    associated = CreateIoCompletionPort(
        (HANDLE)(uintptr_t)operation->socket,
        state->port,
        (ULONG_PTR)0,
        0
    );
    if (associated != state->port) {
        DWORD association_error = GetLastError();
        cr_iocp_make_quiescent(state, operation);
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_NETWORK_FAILURE,
            CR_NATIVE_ERROR_DOMAIN_WIN32,
            (int64_t)association_error
        );
        return false;
    }
    memset(&operation->overlapped, 0, sizeof(operation->overlapped));
    cr_iocp_link_operation(state, operation);
    operation->state = CR_IOCP_OPERATION_SUBMITTED;
    receive_result = WSARecv(
        operation->socket,
        &operation->wsabuf,
        1,
        &bytes_received,
        &flags,
        &operation->overlapped,
        NULL
    );
    CR_BACKEND_IOCP_SUBMIT_OBSERVED(operation, receive_result == 0);
    if (receive_result == SOCKET_ERROR) {
        native_error = WSAGetLastError();
        if (native_error != WSA_IO_PENDING) {
            cr_iocp_make_quiescent(state, operation);
            cr_iocp_set_net_error(
                out_error,
                CR_NET_ERROR_NETWORK_FAILURE,
                CR_NATIVE_ERROR_DOMAIN_WINSOCK,
                (int64_t)native_error
            );
            return false;
        }
    }
    cr_iocp_clear_net_error(out_error);
    return true;
}

static bool cr_iocp_receive_cancel(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    if (!cr_iocp_require_owner(backend, out_error)) return false;
    if (!cr_iocp_operation_belongs_to(operation, backend) ||
        operation->state != CR_IOCP_OPERATION_SUBMITTED) {
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    return cr_iocp_request_cancel(operation, out_error);
}

static bool cr_iocp_receive_quiesce(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    cr_backend_iocp_state *state;

    if (!cr_iocp_require_owner(backend, out_error)) return false;
    state = cr_iocp_state_from_backend(backend);
    if (!cr_iocp_operation_belongs_to(operation, backend)) {
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    return cr_iocp_quiesce_operation(state, operation, out_error);
}

static bool cr_iocp_receive_destroy(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    if (!cr_iocp_require_owner(backend, out_error)) return false;
    if (!cr_iocp_operation_belongs_to(operation, backend) ||
        operation->state != CR_IOCP_OPERATION_QUIESCENT) {
        cr_iocp_set_net_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    operation->state = CR_IOCP_OPERATION_DESTROYED;
    operation->backend = NULL;
    cr_iocp_clear_net_error(out_error);
    return true;
}

static bool cr_iocp_provider_create(
    const cr_backend_provider_desc *provider,
    void **out_provider_state,
    cr_backend_error *out_error
) {
    cr_backend_iocp_state *state;

    cr_backend_internal_clear_error(out_error);
    if (provider != &cr_backend_iocp_provider_desc ||
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
    state = (cr_backend_iocp_state *)
        CR_BACKEND_IOCP_CALLOC(1u, sizeof(*state));
    if (state == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_OUT_OF_MEMORY,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    state->port = CreateIoCompletionPort(
        INVALID_HANDLE_VALUE,
        NULL,
        (ULONG_PTR)0,
        1
    );
    if (state->port == NULL) {
        DWORD native_error = GetLastError();
        CR_BACKEND_IOCP_FREE(state);
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INTERNAL,
            CR_NATIVE_ERROR_DOMAIN_WIN32,
            (int64_t)native_error
        );
        return false;
    }
    CR_BACKEND_IOCP_HANDLE_OPENED(state->port);
    *out_provider_state = state;
    return true;
}

static const cr_backend_extension_desc *cr_iocp_provider_query_extension(
    void *provider_state,
    cr_extension_id extension_id,
    uint32_t requested_abi_version,
    cr_backend_error *out_error
) {
    const cr_extension_id expected = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    cr_backend_iocp_state *state =
        (cr_backend_iocp_state *)provider_state;

    cr_backend_internal_clear_error(out_error);
    if (state == NULL ||
        InterlockedCompareExchange(&state->shutdown, 0, 0) != 0) {
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
    return &cr_backend_iocp_net_extension.base;
}

static DWORD cr_iocp_timeout_ms(uint64_t timeout_ns) {
    uint64_t milliseconds;

    if (timeout_ns == UINT64_MAX) return INFINITE;
    milliseconds = timeout_ns / UINT64_C(1000000);
    if (timeout_ns % UINT64_C(1000000) != UINT64_C(0)) {
        milliseconds++;
    }
    if (milliseconds >= (uint64_t)INFINITE) return INFINITE - 1u;
    return (DWORD)milliseconds;
}

static bool cr_iocp_provider_pump(
    void *provider_state,
    uint64_t timeout_ns,
    uint32_t max_events,
    cr_backend_pump_result *out_result
) {
    cr_backend_iocp_state *state =
        (cr_backend_iocp_state *)provider_state;
    uint32_t dispatched = UINT32_C(0);
    uint32_t operation_events = UINT32_C(0);
    uint32_t interrupt_events = UINT32_C(0);
    DWORD timeout_ms = cr_iocp_timeout_ms(timeout_ns);

    if (state == NULL ||
        InterlockedCompareExchange(&state->shutdown, 0, 0) != 0) {
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
        bool timed_out;
        bool operation_event;
        bool interrupt_event;
        cr_backend_error error;

        if (!cr_iocp_wait_and_dispatch(
                state,
                dispatched == UINT32_C(0) ? timeout_ms : 0,
                &timed_out,
                &operation_event,
                &interrupt_event,
                &error
            )) {
            *out_result = (cr_backend_pump_result){
                CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
                sizeof(cr_backend_pump_result),
                CR_BACKEND_PUMP_ERROR,
                dispatched,
                error.category,
                error.native_domain,
                error.native_code
            };
            return false;
        }
        if (timed_out) break;
        dispatched++;
        if (operation_event) operation_events++;
        if (interrupt_event) interrupt_events++;
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

static bool cr_iocp_provider_interrupt(
    void *provider_state,
    cr_backend_error *out_error
) {
    cr_backend_iocp_state *state =
        (cr_backend_iocp_state *)provider_state;

    cr_backend_internal_clear_error(out_error);
    if (state == NULL ||
        InterlockedCompareExchange(&state->shutdown, 0, 0) != 0) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_CLOSED,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (InterlockedCompareExchange(&state->interrupted, 1, 0) != 0) {
        return true;
    }
    if (!PostQueuedCompletionStatus(
            state->port,
            0,
            (ULONG_PTR)state,
            NULL
        )) {
        DWORD native_error = GetLastError();
        InterlockedExchange(&state->interrupted, 0);
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INTERNAL,
            CR_NATIVE_ERROR_DOMAIN_WIN32,
            (int64_t)native_error
        );
        return false;
    }
    return true;
}

static bool cr_iocp_provider_shutdown(
    void *provider_state,
    cr_backend_error *out_error
) {
    cr_backend_iocp_state *state =
        (cr_backend_iocp_state *)provider_state;

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
    if (InterlockedCompareExchange(&state->shutdown, 0, 0) != 0) {
        return true;
    }
    while (state->active_head != NULL) {
        cr_net_error operation_error;
        if (!cr_iocp_quiesce_operation(
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
    InterlockedExchange(&state->shutdown, 1);
    return true;
}

static void cr_iocp_provider_destroy(void *provider_state) {
    cr_backend_iocp_state *state =
        (cr_backend_iocp_state *)provider_state;

    if (state == NULL) return;
    if (state->port != NULL) {
        HANDLE port = state->port;
        state->port = NULL;
        if (CloseHandle(port)) {
            CR_BACKEND_IOCP_HANDLE_CLOSED(port);
        }
    }
    CR_BACKEND_IOCP_FREE(state);
}

static const cr_net_extension_desc cr_backend_iocp_net_extension = {
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
    cr_iocp_receive_initialize,
    cr_iocp_receive_submit,
    cr_iocp_receive_cancel,
    cr_iocp_receive_quiesce,
    cr_iocp_receive_destroy
};

const cr_backend_provider_desc cr_backend_iocp_provider_desc = {
    CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
    sizeof(cr_backend_provider_desc),
    UINT64_C(0),
    CR_BACKEND_CORE_ID_INIT,
    cr_iocp_provider_create,
    cr_iocp_provider_query_extension,
    cr_iocp_provider_pump,
    cr_iocp_provider_interrupt,
    cr_iocp_provider_shutdown,
    cr_iocp_provider_destroy
};
"#;
