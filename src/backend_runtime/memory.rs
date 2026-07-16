pub(crate) const MEMORY_SOURCE: &str = r#"#include "cr_backend_internal.h"

#include <stdatomic.h>
#include <string.h>

#define CR_MEMORY_OPERATION_MAGIC UINT64_C(0x43524d454d4f5031)

#define CR_MEMORY_OPERATION_INITIALIZED UINT32_C(1)
#define CR_MEMORY_OPERATION_SUBMITTED   UINT32_C(2)
#define CR_MEMORY_OPERATION_TERMINAL    UINT32_C(3)
#define CR_MEMORY_OPERATION_QUIESCENT   UINT32_C(4)
#define CR_MEMORY_OPERATION_DESTROYED   UINT32_C(5)

typedef struct cr_backend_memory_state cr_backend_memory_state;

struct cr_net_receive_operation {
    uint64_t magic;
    uint64_t generation;
    uint32_t state;
    bool linked;
    bool event_pending;
    bool cancel_requested;
    bool callback_delivered;
    cr_backend *backend;
    cr_native_socket_handle socket;
    void *buffer;
    uint64_t buffer_size;
    cr_net_receive_completion_fn on_completion;
    void *callback_context;
    cr_net_receive_completion pending_completion;
    cr_net_receive_operation *previous;
    cr_net_receive_operation *next;
};

struct cr_backend_memory_state {
    atomic_bool interrupted;
    bool shutdown;
    cr_net_receive_operation *active_head;
    cr_backend_memory_trace_fn trace;
    void *trace_context;
};

static const cr_net_extension_desc cr_backend_memory_net_extension;

static void cr_memory_clear_error(cr_net_error *out_error) {
    if (out_error == NULL) return;
    *out_error = (cr_net_error){
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_error),
        CR_NET_ERROR_NONE,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    };
}

static void cr_memory_set_error(
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

static cr_backend_memory_state *cr_memory_state_from_backend(
    const cr_backend *backend
) {
    if (backend == NULL ||
        cr_backend_internal_provider(backend) !=
            &cr_backend_memory_provider_desc) {
        return NULL;
    }
    return (cr_backend_memory_state *)
        cr_backend_internal_provider_state(backend);
}

static bool cr_memory_require_owner(
    cr_backend *backend,
    cr_net_error *out_error
) {
    cr_backend_error backend_error;

    if (cr_memory_state_from_backend(backend) == NULL) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (!cr_backend_internal_require_owner(backend, &backend_error)) {
        cr_memory_set_error(
            out_error,
            backend_error.category == CR_BACKEND_ERROR_WRONG_THREAD
                ? CR_NET_ERROR_WRONG_THREAD
                : CR_NET_ERROR_INVALID_ARGUMENT,
            backend_error.native_domain,
            backend_error.native_code
        );
        return false;
    }
    cr_memory_clear_error(out_error);
    return true;
}

static void cr_memory_trace(
    cr_backend_memory_state *state,
    cr_backend_memory_trace_event event,
    const cr_net_receive_operation *operation
) {
    if (state->trace != NULL) {
        state->trace(
            state->trace_context,
            event,
            operation,
            operation != NULL ? operation->generation : UINT64_C(0)
        );
    }
}

static bool cr_memory_operation_has_magic(
    const cr_net_receive_operation *operation
) {
    uint64_t magic = UINT64_C(0);

    if (operation == NULL) return false;
    memcpy(&magic, operation, sizeof(magic));
    return magic == CR_MEMORY_OPERATION_MAGIC;
}

static bool cr_memory_operation_belongs_to(
    const cr_net_receive_operation *operation,
    const cr_backend *backend
) {
    return cr_memory_operation_has_magic(operation) &&
        operation->backend == backend;
}

static void cr_memory_link_operation(
    cr_backend_memory_state *state,
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

static void cr_memory_unlink_operation(
    cr_backend_memory_state *state,
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

static bool cr_memory_buffers_overlap(
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

static bool cr_memory_resource_is_busy(
    const cr_backend_memory_state *state,
    const cr_net_receive_operation *candidate
) {
    const cr_net_receive_operation *operation = state->active_head;

    while (operation != NULL) {
        if (operation != candidate &&
            ((operation->socket.kind == candidate->socket.kind &&
              operation->socket.value == candidate->socket.value) ||
             cr_memory_buffers_overlap(operation, candidate))) {
            return true;
        }
        operation = operation->next;
    }
    return false;
}

static void cr_memory_clear_borrowed_fields(
    cr_net_receive_operation *operation
) {
    operation->socket = (cr_native_socket_handle){
        CR_NATIVE_SOCKET_INVALID,
        UINT32_C(0),
        (uintptr_t)0
    };
    operation->buffer = NULL;
    operation->buffer_size = UINT64_C(0);
    operation->on_completion = NULL;
    operation->callback_context = NULL;
    operation->event_pending = false;
    operation->cancel_requested = false;
}

static void cr_memory_make_quiescent(
    cr_backend_memory_state *state,
    cr_net_receive_operation *operation
) {
    cr_memory_unlink_operation(state, operation);
    cr_memory_clear_borrowed_fields(operation);
    operation->state = CR_MEMORY_OPERATION_QUIESCENT;
    cr_memory_trace(
        state,
        CR_BACKEND_MEMORY_TRACE_QUIESCENT,
        operation
    );
}

static void cr_memory_queue_completion(
    cr_backend_memory_state *state,
    cr_net_receive_operation *operation,
    cr_net_receive_terminal_kind terminal_kind,
    uint64_t bytes_transferred,
    cr_net_error_category error_category,
    cr_native_error_domain native_domain,
    int64_t native_code
) {
    operation->pending_completion = (cr_net_receive_completion){
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_receive_completion),
        terminal_kind,
        error_category,
        bytes_transferred,
        native_domain,
        UINT32_C(0),
        native_code
    };
    operation->event_pending = true;
    cr_memory_trace(
        state,
        CR_BACKEND_MEMORY_TRACE_TERMINAL_QUEUED,
        operation
    );
}

static void cr_memory_queue_cancel(
    cr_backend_memory_state *state,
    cr_net_receive_operation *operation
) {
    operation->cancel_requested = true;
    cr_memory_trace(
        state,
        CR_BACKEND_MEMORY_TRACE_CANCEL_REQUESTED,
        operation
    );
    cr_memory_queue_completion(
        state,
        operation,
        CR_NET_RECEIVE_CANCELED,
        UINT64_C(0),
        CR_NET_ERROR_NONE,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    );
}

static bool cr_memory_dispatch_operation(
    cr_backend_memory_state *state,
    cr_net_receive_operation *operation
) {
    cr_net_receive_completion completion;
    cr_net_receive_completion_fn callback;
    void *callback_context;

    if (operation == NULL || !operation->event_pending ||
        operation->state != CR_MEMORY_OPERATION_SUBMITTED) {
        return false;
    }
    completion = operation->pending_completion;
    callback = operation->on_completion;
    callback_context = operation->callback_context;
    operation->event_pending = false;
    operation->state = CR_MEMORY_OPERATION_TERMINAL;
    operation->callback_delivered = true;
    cr_memory_trace(
        state,
        CR_BACKEND_MEMORY_TRACE_TERMINAL_CALLBACK,
        operation
    );
    callback(callback_context, &completion);
    return true;
}

static uint32_t cr_memory_dispatch_pending(
    cr_backend_memory_state *state,
    uint32_t max_events
) {
    cr_net_receive_operation *operation = state->active_head;
    uint32_t dispatched = UINT32_C(0);

    while (operation != NULL && dispatched < max_events) {
        cr_net_receive_operation *next = operation->next;
        if (cr_memory_dispatch_operation(state, operation)) {
            dispatched++;
        }
        operation = next;
    }
    return dispatched;
}

static bool cr_memory_quiesce_operation(
    cr_backend_memory_state *state,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    if (operation->state == CR_MEMORY_OPERATION_QUIESCENT) {
        cr_memory_clear_error(out_error);
        return true;
    }
    if (operation->state == CR_MEMORY_OPERATION_INITIALIZED) {
        cr_memory_make_quiescent(state, operation);
        cr_memory_clear_error(out_error);
        return true;
    }
    if (operation->state == CR_MEMORY_OPERATION_SUBMITTED &&
        !operation->event_pending) {
        cr_memory_queue_cancel(state, operation);
    }
    while (operation->state == CR_MEMORY_OPERATION_SUBMITTED) {
        if (cr_memory_dispatch_pending(state, UINT32_MAX) == UINT32_C(0)) {
            cr_memory_set_error(
                out_error,
                CR_NET_ERROR_INTERNAL_BACKEND,
                CR_NATIVE_ERROR_DOMAIN_MEMORY,
                INT64_C(1)
            );
            return false;
        }
    }
    if (operation->state != CR_MEMORY_OPERATION_TERMINAL ||
        !operation->callback_delivered) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INTERNAL_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_MEMORY,
            INT64_C(2)
        );
        return false;
    }
    cr_memory_make_quiescent(state, operation);
    cr_memory_clear_error(out_error);
    return true;
}

static bool cr_memory_receive_initialize(
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
    cr_backend_memory_state *state;
    uint64_t generation = UINT64_C(1);

    if (!cr_memory_require_owner(backend, out_error)) return false;
    state = cr_memory_state_from_backend(backend);
    if (cr_backend_internal_is_closed(backend) || state->shutdown) {
        cr_memory_set_error(
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
        socket.kind != CR_NATIVE_SOCKET_MEMORY ||
        socket.reserved != UINT32_C(0) ||
        buffer == NULL || buffer_size == UINT64_C(0) || buffer_size > SIZE_MAX ||
        on_completion == NULL) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (cr_memory_operation_has_magic(operation)) {
        if (operation->state != CR_MEMORY_OPERATION_QUIESCENT) {
            cr_memory_set_error(
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
    operation->magic = CR_MEMORY_OPERATION_MAGIC;
    operation->generation = generation;
    operation->state = CR_MEMORY_OPERATION_INITIALIZED;
    operation->backend = backend;
    operation->socket = socket;
    operation->buffer = buffer;
    operation->buffer_size = buffer_size;
    operation->on_completion = on_completion;
    operation->callback_context = callback_context;
    cr_memory_trace(
        state,
        CR_BACKEND_MEMORY_TRACE_INITIALIZED,
        operation
    );
    cr_memory_clear_error(out_error);
    return true;
}

static bool cr_memory_receive_submit(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    cr_backend_memory_state *state;

    if (!cr_memory_require_owner(backend, out_error)) return false;
    state = cr_memory_state_from_backend(backend);
    if (!cr_memory_operation_belongs_to(operation, backend) ||
        operation->state != CR_MEMORY_OPERATION_INITIALIZED) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (cr_backend_internal_is_closed(backend) || state->shutdown) {
        cr_memory_make_quiescent(state, operation);
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_CLOSED_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (cr_memory_resource_is_busy(state, operation)) {
        cr_memory_make_quiescent(state, operation);
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_BUSY,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    cr_memory_link_operation(state, operation);
    operation->state = CR_MEMORY_OPERATION_SUBMITTED;
    cr_memory_trace(
        state,
        CR_BACKEND_MEMORY_TRACE_SUBMITTED,
        operation
    );
    cr_memory_clear_error(out_error);
    return true;
}

static bool cr_memory_receive_cancel(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    cr_backend_memory_state *state;

    if (!cr_memory_require_owner(backend, out_error)) return false;
    state = cr_memory_state_from_backend(backend);
    if (!cr_memory_operation_belongs_to(operation, backend) ||
        operation->state != CR_MEMORY_OPERATION_SUBMITTED) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (operation->cancel_requested) {
        cr_memory_clear_error(out_error);
        return true;
    }
    operation->cancel_requested = true;
    cr_memory_trace(
        state,
        CR_BACKEND_MEMORY_TRACE_CANCEL_REQUESTED,
        operation
    );
    if (!operation->event_pending) {
        cr_memory_queue_completion(
            state,
            operation,
            CR_NET_RECEIVE_CANCELED,
            UINT64_C(0),
            CR_NET_ERROR_NONE,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
    }
    cr_memory_clear_error(out_error);
    return true;
}

static bool cr_memory_receive_quiesce(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    cr_backend_memory_state *state;

    if (!cr_memory_require_owner(backend, out_error)) return false;
    state = cr_memory_state_from_backend(backend);
    if (!cr_memory_operation_belongs_to(operation, backend)) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    return cr_memory_quiesce_operation(state, operation, out_error);
}

static bool cr_memory_receive_destroy(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    cr_backend_memory_state *state;

    if (!cr_memory_require_owner(backend, out_error)) return false;
    state = cr_memory_state_from_backend(backend);
    if (!cr_memory_operation_belongs_to(operation, backend) ||
        operation->state != CR_MEMORY_OPERATION_QUIESCENT) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    operation->state = CR_MEMORY_OPERATION_DESTROYED;
    cr_memory_trace(
        state,
        CR_BACKEND_MEMORY_TRACE_DESTROYED,
        operation
    );
    operation->backend = NULL;
    cr_memory_clear_error(out_error);
    return true;
}

bool cr_backend_memory_set_trace(
    cr_backend *backend,
    cr_backend_memory_trace_fn trace,
    void *trace_context,
    cr_backend_error *out_error
) {
    cr_backend_memory_state *state;

    cr_backend_internal_clear_error(out_error);
    if (!cr_backend_internal_require_owner(backend, out_error)) return false;
    state = cr_memory_state_from_backend(backend);
    if (state == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (cr_backend_internal_is_closed(backend) || state->shutdown) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_CLOSED,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    state->trace = trace;
    state->trace_context = trace_context;
    return true;
}

bool cr_backend_memory_complete_ready(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    const void *data,
    uint64_t data_size,
    cr_net_error *out_error
) {
    cr_backend_memory_state *state;

    if (!cr_memory_require_owner(backend, out_error)) return false;
    state = cr_memory_state_from_backend(backend);
    if (cr_backend_internal_is_closed(backend) || state->shutdown) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_CLOSED_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (!cr_memory_operation_belongs_to(operation, backend) ||
        operation->state != CR_MEMORY_OPERATION_SUBMITTED) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (operation->event_pending) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_BUSY,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (data_size > operation->buffer_size || data_size > SIZE_MAX ||
        (data_size != UINT64_C(0) && data == NULL)) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (data_size != UINT64_C(0)) {
        memcpy(operation->buffer, data, (size_t)data_size);
    }
    cr_memory_queue_completion(
        state,
        operation,
        CR_NET_RECEIVE_READY,
        data_size,
        CR_NET_ERROR_NONE,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    );
    cr_memory_clear_error(out_error);
    return true;
}

bool cr_backend_memory_complete_error(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error_category category,
    cr_native_error_domain native_domain,
    int64_t native_code,
    cr_net_error *out_error
) {
    cr_backend_memory_state *state;

    if (!cr_memory_require_owner(backend, out_error)) return false;
    state = cr_memory_state_from_backend(backend);
    if (cr_backend_internal_is_closed(backend) || state->shutdown) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_CLOSED_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (!cr_memory_operation_belongs_to(operation, backend) ||
        operation->state != CR_MEMORY_OPERATION_SUBMITTED ||
        category == CR_NET_ERROR_NONE) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (operation->event_pending) {
        cr_memory_set_error(
            out_error,
            CR_NET_ERROR_BUSY,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    cr_memory_queue_completion(
        state,
        operation,
        CR_NET_RECEIVE_ERROR,
        UINT64_C(0),
        category,
        native_domain,
        native_code
    );
    cr_memory_clear_error(out_error);
    return true;
}

static bool cr_memory_provider_create(
    const cr_backend_provider_desc *provider,
    void **out_provider_state,
    cr_backend_error *out_error
) {
    cr_backend_memory_state *state;

    cr_backend_internal_clear_error(out_error);
    if (provider != &cr_backend_memory_provider_desc ||
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
    state = (cr_backend_memory_state *)
        CR_BACKEND_MEMORY_CALLOC(1u, sizeof(*state));
    if (state == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_OUT_OF_MEMORY,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    atomic_init(&state->interrupted, false);
    *out_provider_state = state;
    return true;
}

static const cr_backend_extension_desc *cr_memory_provider_query_extension(
    void *provider_state,
    cr_extension_id extension_id,
    uint32_t requested_abi_version,
    cr_backend_error *out_error
) {
    const cr_extension_id expected = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    cr_backend_memory_state *state =
        (cr_backend_memory_state *)provider_state;

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
    return &cr_backend_memory_net_extension.base;
}

static bool cr_memory_provider_pump(
    void *provider_state,
    uint64_t timeout_ns,
    uint32_t max_events,
    cr_backend_pump_result *out_result
) {
    cr_backend_memory_state *state =
        (cr_backend_memory_state *)provider_state;
    uint32_t operation_events;
    bool interrupted = false;

    (void)timeout_ns;
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
    operation_events = cr_memory_dispatch_pending(state, max_events);
    if (operation_events < max_events) {
        interrupted = atomic_exchange_explicit(
            &state->interrupted,
            false,
            memory_order_acquire
        );
        if (interrupted) {
            cr_memory_trace(
                state,
                CR_BACKEND_MEMORY_TRACE_INTERRUPT_CONSUMED,
                NULL
            );
        }
    }
    *out_result = (cr_backend_pump_result){
        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_backend_pump_result),
        operation_events != UINT32_C(0)
            ? CR_BACKEND_PUMP_PROGRESS
            : interrupted
                ? CR_BACKEND_PUMP_INTERRUPTED
                : CR_BACKEND_PUMP_TIMEOUT,
        operation_events + (interrupted ? UINT32_C(1) : UINT32_C(0)),
        CR_BACKEND_ERROR_NONE,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    };
    return true;
}

static bool cr_memory_provider_interrupt(
    void *provider_state,
    cr_backend_error *out_error
) {
    cr_backend_memory_state *state =
        (cr_backend_memory_state *)provider_state;

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
    atomic_store_explicit(&state->interrupted, true, memory_order_release);
    return true;
}

static bool cr_memory_provider_shutdown(
    void *provider_state,
    cr_backend_error *out_error
) {
    cr_backend_memory_state *state =
        (cr_backend_memory_state *)provider_state;

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
        if (!cr_memory_quiesce_operation(
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
    atomic_store_explicit(&state->interrupted, false, memory_order_release);
    cr_memory_trace(state, CR_BACKEND_MEMORY_TRACE_SHUTDOWN, NULL);
    return true;
}

static void cr_memory_provider_destroy(void *provider_state) {
    cr_backend_memory_state *state =
        (cr_backend_memory_state *)provider_state;

    if (state == NULL) return;
    CR_BACKEND_MEMORY_FREE(state);
}

static const cr_net_extension_desc cr_backend_memory_net_extension = {
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
    cr_memory_receive_initialize,
    cr_memory_receive_submit,
    cr_memory_receive_cancel,
    cr_memory_receive_quiesce,
    cr_memory_receive_destroy
};

const cr_backend_provider_desc cr_backend_memory_provider_desc = {
    CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
    sizeof(cr_backend_provider_desc),
    UINT64_C(0),
    CR_BACKEND_CORE_ID_INIT,
    cr_memory_provider_create,
    cr_memory_provider_query_extension,
    cr_memory_provider_pump,
    cr_memory_provider_interrupt,
    cr_memory_provider_shutdown,
    cr_memory_provider_destroy
};
"#;
