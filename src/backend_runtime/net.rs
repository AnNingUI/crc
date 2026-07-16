pub(crate) const NET_RECEIVE_SOURCE: &str = r#"#include "cr_backend_internal.h"
#include "cr_waker.h"

#include <string.h>

#define CR_NET_AWAITABLE_MAGIC UINT64_C(0x43524e4554415731)

struct cr_net_receive_awaitable_state {
    uint64_t magic;
    uint64_t generation;
    cr_backend *backend;
    const cr_net_extension_desc *net;
    cr_net_receive_operation *operation;
    uint64_t operation_storage_size;
    cr_native_socket_handle socket;
    void *buffer;
    uint64_t buffer_size;
    bool operation_initialized;
    bool submitted;
    bool terminal_ready;
    bool quiescent;
    bool operation_destroyed;
    bool cancel_requested;
    bool wake_enabled;
    bool dropped;
    bool forced_error;
    cr_waker retained_waker;
    cr_net_receive_completion completion;
    cr_error error;
};

static void cr_net_awaitable_clear_error(cr_error *out_error) {
    if (out_error == NULL) return;
    *out_error = (cr_error){0, NULL};
}

static void cr_net_awaitable_set_external_error(
    cr_error *out_error,
    int32_t code,
    const char *message
) {
    if (out_error == NULL) return;
    *out_error = (cr_error){code, message};
}

static int32_t cr_net_awaitable_error_code(
    cr_net_error_category category
) {
    switch (category) {
        case CR_NET_ERROR_INVALID_ARGUMENT:
            return CR_ERROR_NET_RECEIVE_INVALID_ARGUMENT;
        case CR_NET_ERROR_UNSUPPORTED_CAPABILITY:
            return CR_ERROR_NET_RECEIVE_UNSUPPORTED;
        case CR_NET_ERROR_BUSY:
            return CR_ERROR_NET_RECEIVE_BUSY;
        case CR_NET_ERROR_OUT_OF_MEMORY:
            return CR_ERROR_NET_RECEIVE_OUT_OF_MEMORY;
        case CR_NET_ERROR_CLOSED_BACKEND:
            return CR_ERROR_NET_RECEIVE_CLOSED;
        case CR_NET_ERROR_NETWORK_FAILURE:
            return CR_ERROR_NET_RECEIVE_NETWORK_FAILURE;
        case CR_NET_ERROR_WRONG_THREAD:
            return CR_ERROR_NET_RECEIVE_WRONG_THREAD;
        case CR_NET_ERROR_INTERNAL_BACKEND:
        default:
            return CR_ERROR_NET_RECEIVE_INTERNAL;
    }
}

static const char *cr_net_awaitable_error_message(
    cr_net_error_category category
) {
    switch (category) {
        case CR_NET_ERROR_INVALID_ARGUMENT:
            return "net receive invalid argument";
        case CR_NET_ERROR_UNSUPPORTED_CAPABILITY:
            return "net receive capability unsupported";
        case CR_NET_ERROR_BUSY:
            return "net receive operation busy";
        case CR_NET_ERROR_OUT_OF_MEMORY:
            return "net receive out of memory";
        case CR_NET_ERROR_CLOSED_BACKEND:
            return "net receive backend closed";
        case CR_NET_ERROR_NETWORK_FAILURE:
            return "net receive network failure";
        case CR_NET_ERROR_WRONG_THREAD:
            return "net receive called from the wrong owner thread";
        case CR_NET_ERROR_INTERNAL_BACKEND:
        default:
            return "net receive backend failure";
    }
}

static bool cr_net_awaitable_has_magic(
    const cr_net_receive_awaitable_state *state
) {
    uint64_t magic = UINT64_C(0);

    if (state == NULL) return false;
    memcpy(&magic, state, sizeof(magic));
    return magic == CR_NET_AWAITABLE_MAGIC;
}

static void cr_net_awaitable_force_error(
    cr_net_receive_awaitable_state *state,
    int32_t code,
    const char *message,
    cr_net_error_category category,
    cr_native_error_domain native_domain,
    int64_t native_code
) {
    state->forced_error = true;
    state->terminal_ready = true;
    state->completion = (cr_net_receive_completion){
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_net_receive_completion),
        CR_NET_RECEIVE_ERROR,
        category,
        UINT64_C(0),
        native_domain,
        UINT32_C(0),
        native_code
    };
    state->error = (cr_error){code, message};
}

static void cr_net_awaitable_force_net_error(
    cr_net_receive_awaitable_state *state,
    const cr_net_error *error
) {
    cr_net_error_category category = CR_NET_ERROR_INTERNAL_BACKEND;
    cr_native_error_domain native_domain = CR_NATIVE_ERROR_DOMAIN_NONE;
    int64_t native_code = INT64_C(0);

    if (error != NULL &&
        error->abi_version >= CR_NET_EXPERIMENTAL_ABI_VERSION &&
        error->struct_size >= CR_NET_ERROR_V1_MIN_SIZE) {
        category = error->category;
        native_domain = error->native_domain;
        native_code = error->native_code;
    }
    cr_net_awaitable_force_error(
        state,
        cr_net_awaitable_error_code(category),
        cr_net_awaitable_error_message(category),
        category,
        native_domain,
        native_code
    );
}

static void cr_net_awaitable_copy_completion(
    cr_net_receive_awaitable_state *state,
    const cr_net_receive_completion *completion
) {
    if (!cr_net_receive_completion_has_v1_prefix(completion) ||
        (completion->terminal_kind != CR_NET_RECEIVE_READY &&
         completion->terminal_kind != CR_NET_RECEIVE_ERROR &&
         completion->terminal_kind != CR_NET_RECEIVE_CANCELED)) {
        cr_net_awaitable_force_error(
            state,
            CR_ERROR_NET_RECEIVE_INVALID_COMPLETION,
            "net receive returned an invalid completion",
            CR_NET_ERROR_INTERNAL_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return;
    }
    state->completion = *completion;
    state->terminal_ready = true;
    if (completion->terminal_kind == CR_NET_RECEIVE_ERROR) {
        state->error = (cr_error){
            cr_net_awaitable_error_code(completion->error_category),
            cr_net_awaitable_error_message(completion->error_category)
        };
    }
}

static void cr_net_awaitable_on_completion(
    void *callback_context,
    const cr_net_receive_completion *completion
) {
    cr_net_receive_awaitable_state *state =
        (cr_net_receive_awaitable_state *)callback_context;

    if (!cr_net_awaitable_has_magic(state) || state->dropped) return;
    if (!state->forced_error) {
        cr_net_awaitable_copy_completion(state, completion);
    }
    if (state->wake_enabled && cr_waker_is_valid(&state->retained_waker)) {
        cr_waker_wake(&state->retained_waker);
    }
}

static bool cr_net_awaitable_register_waker(
    cr_net_receive_awaitable_state *state,
    const cr_poll_context *poll_context
) {
    cr_waker replacement = {NULL, NULL};
    bool has_waker = poll_context != NULL &&
        (poll_context->available_capabilities & CR_POLL_CAP_WAKER) != 0u &&
        cr_waker_is_valid(poll_context->waker);

    if (!has_waker) {
        cr_waker_drop(&state->retained_waker);
        return true;
    }
    if (!cr_waker_clone(poll_context->waker, &replacement)) {
        cr_net_awaitable_force_error(
            state,
            CR_ERROR_NET_RECEIVE_WAKER_CLONE_FAILED,
            "net receive Waker clone failed",
            CR_NET_ERROR_INTERNAL_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    cr_waker_drop(&state->retained_waker);
    state->retained_waker = replacement;
    return true;
}

static cr_poll_status cr_net_awaitable_finish_terminal(
    cr_net_receive_awaitable_state *state,
    void *out_value
) {
    cr_net_error quiesce_error;

    if (!state->quiescent && state->operation_initialized) {
        if (!state->net->receive_quiesce(
                state->backend,
                state->operation,
                &quiesce_error
            )) {
            cr_net_awaitable_force_net_error(state, &quiesce_error);
            if (state->wake_enabled &&
                cr_waker_is_valid(&state->retained_waker)) {
                cr_waker_wake(&state->retained_waker);
            }
            return CR_POLL_PENDING;
        }
        state->quiescent = true;
    }
    cr_waker_drop(&state->retained_waker);
    if (state->forced_error ||
        state->completion.terminal_kind == CR_NET_RECEIVE_ERROR) {
        return CR_POLL_ERROR;
    }
    if (state->completion.terminal_kind == CR_NET_RECEIVE_CANCELED) {
        return CR_POLL_CANCELED;
    }
    if (state->completion.terminal_kind != CR_NET_RECEIVE_READY ||
        out_value == NULL) {
        cr_net_awaitable_force_error(
            state,
            CR_ERROR_NET_RECEIVE_INVALID_COMPLETION,
            "net receive result storage is invalid",
            CR_NET_ERROR_INTERNAL_BACKEND,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return CR_POLL_ERROR;
    }
    *(uint64_t *)out_value = state->completion.bytes_transferred;
    return CR_POLL_READY;
}

static cr_poll_status cr_net_awaitable_poll(
    void *raw_state,
    const cr_poll_context *poll_context,
    void *out_value
) {
    cr_net_receive_awaitable_state *state =
        (cr_net_receive_awaitable_state *)raw_state;
    cr_net_error operation_error;

    if (!cr_net_awaitable_has_magic(state) || state->dropped) {
        return CR_POLL_ERROR;
    }
    if (state->terminal_ready) {
        return cr_net_awaitable_finish_terminal(state, out_value);
    }
    if (!cr_net_awaitable_register_waker(state, poll_context)) {
        if (state->submitted) {
            (void)state->net->receive_cancel(
                state->backend,
                state->operation,
                &operation_error
            );
        }
        return cr_net_awaitable_finish_terminal(state, out_value);
    }
    if (!state->operation_initialized) {
        if (!state->net->receive_initialize(
                state->backend,
                state->operation,
                state->operation_storage_size,
                state->socket,
                state->buffer,
                state->buffer_size,
                cr_net_awaitable_on_completion,
                state,
                &operation_error
            )) {
            state->quiescent = true;
            cr_net_awaitable_force_net_error(state, &operation_error);
            return cr_net_awaitable_finish_terminal(state, out_value);
        }
        state->operation_initialized = true;
        state->quiescent = false;
    }
    if (!state->submitted) {
        if (!state->net->receive_submit(
                state->backend,
                state->operation,
                &operation_error
            )) {
            state->quiescent = true;
            cr_net_awaitable_force_net_error(state, &operation_error);
            return cr_net_awaitable_finish_terminal(state, out_value);
        }
        state->submitted = true;
    }
    if (state->terminal_ready) {
        return cr_net_awaitable_finish_terminal(state, out_value);
    }
    return CR_POLL_PENDING;
}

static const cr_error *cr_net_awaitable_vtable_error(
    const void *raw_state
) {
    const cr_net_receive_awaitable_state *state =
        (const cr_net_receive_awaitable_state *)raw_state;

    if (!cr_net_awaitable_has_magic(state)) return NULL;
    return state->error.code != 0 ? &state->error : NULL;
}

static void cr_net_awaitable_drop(void *raw_state) {
    cr_net_receive_awaitable_state *state =
        (cr_net_receive_awaitable_state *)raw_state;
    cr_net_error operation_error;

    if (!cr_net_awaitable_has_magic(state) || state->dropped) return;
    state->wake_enabled = false;
    if (state->operation_initialized && !state->quiescent) {
        if (state->submitted && !state->terminal_ready) {
            (void)state->net->receive_cancel(
                state->backend,
                state->operation,
                &operation_error
            );
        }
        if (!state->net->receive_quiesce(
                state->backend,
                state->operation,
                &operation_error
            )) {
            abort();
        }
        state->quiescent = true;
    }
    if (state->operation_initialized && !state->operation_destroyed) {
        if (!state->net->receive_destroy(
                state->backend,
                state->operation,
                &operation_error
            )) {
            abort();
        }
        state->operation_destroyed = true;
    }
    cr_waker_drop(&state->retained_waker);
    state->dropped = true;
}

static const cr_awaitable_vtable cr_net_receive_awaitable_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    UINT64_C(0),
    UINT64_C(0),
    cr_net_awaitable_poll,
    cr_net_awaitable_vtable_error,
    cr_net_awaitable_drop,
    sizeof(uint64_t),
    _Alignof(uint64_t)
};

cr_storage_layout cr_net_receive_awaitable_state_layout(void) {
    return (cr_storage_layout){
        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_storage_layout),
        sizeof(cr_net_receive_awaitable_state),
        _Alignof(cr_net_receive_awaitable_state)
    };
}

bool cr_net_receive_awaitable_initialize(
    cr_net_receive_awaitable_state *state,
    uint64_t state_storage_size,
    cr_backend *backend,
    const cr_net_extension_desc *net,
    cr_net_receive_operation *operation,
    uint64_t operation_storage_size,
    cr_native_socket_handle socket,
    void *buffer,
    uint64_t buffer_size,
    cr_awaitable *out_awaitable,
    cr_error *out_error
) {
    uint64_t generation = UINT64_C(1);

    cr_net_awaitable_clear_error(out_error);
    if (out_awaitable != NULL) {
        *out_awaitable = (cr_awaitable){NULL, NULL};
    }
    if (state == NULL || out_awaitable == NULL || backend == NULL ||
        !cr_net_extension_desc_is_compatible(net) || operation == NULL ||
        state_storage_size < sizeof(cr_net_receive_awaitable_state) ||
        ((uintptr_t)state %
         _Alignof(cr_net_receive_awaitable_state)) != 0u ||
        operation_storage_size < net->receive_operation_layout.size ||
        net->receive_operation_layout.size > SIZE_MAX ||
        net->receive_operation_layout.alignment > UINTPTR_MAX ||
        ((uintptr_t)operation %
         (uintptr_t)net->receive_operation_layout.alignment) != 0u ||
        !cr_native_socket_handle_is_valid(socket) || buffer == NULL ||
        buffer_size == UINT64_C(0) || buffer_size > SIZE_MAX) {
        cr_net_awaitable_set_external_error(
            out_error,
            CR_ERROR_NET_RECEIVE_INVALID_ARGUMENT,
            "invalid net receive awaitable initialization"
        );
        return false;
    }
    if (cr_net_awaitable_has_magic(state)) {
        if (!state->dropped) {
            cr_net_awaitable_set_external_error(
                out_error,
                CR_ERROR_NET_RECEIVE_BUSY,
                "net receive awaitable storage is active"
            );
            return false;
        }
        generation = state->generation + UINT64_C(1);
        if (generation == UINT64_C(0)) generation = UINT64_C(1);
    }
    memset(state, 0, sizeof(*state));
    state->magic = CR_NET_AWAITABLE_MAGIC;
    state->generation = generation;
    state->backend = backend;
    state->net = net;
    state->operation = operation;
    state->operation_storage_size = operation_storage_size;
    state->socket = socket;
    state->buffer = buffer;
    state->buffer_size = buffer_size;
    state->quiescent = true;
    state->wake_enabled = true;
    *out_awaitable = (cr_awaitable){
        state,
        &cr_net_receive_awaitable_vtable
    };
    return true;
}

bool cr_net_receive_awaitable_cancel(
    cr_net_receive_awaitable_state *state,
    cr_error *out_error
) {
    cr_net_error operation_error;

    cr_net_awaitable_clear_error(out_error);
    if (!cr_net_awaitable_has_magic(state) || state->dropped) {
        cr_net_awaitable_set_external_error(
            out_error,
            CR_ERROR_NET_RECEIVE_INVALID_ARGUMENT,
            "invalid net receive awaitable cancellation"
        );
        return false;
    }
    if (state->cancel_requested || state->terminal_ready) return true;
    state->cancel_requested = true;
    if (!state->submitted) {
        state->terminal_ready = true;
        state->quiescent = true;
        state->completion = (cr_net_receive_completion){
            CR_NET_EXPERIMENTAL_ABI_VERSION,
            sizeof(cr_net_receive_completion),
            CR_NET_RECEIVE_CANCELED,
            CR_NET_ERROR_NONE,
            UINT64_C(0),
            CR_NATIVE_ERROR_DOMAIN_NONE,
            UINT32_C(0),
            INT64_C(0)
        };
        return true;
    }
    if (!state->net->receive_cancel(
            state->backend,
            state->operation,
            &operation_error
        )) {
        state->cancel_requested = false;
        cr_net_awaitable_set_external_error(
            out_error,
            cr_net_awaitable_error_code(operation_error.category),
            cr_net_awaitable_error_message(operation_error.category)
        );
        return false;
    }
    return true;
}

const cr_net_receive_completion *cr_net_receive_awaitable_completion(
    const cr_net_receive_awaitable_state *state
) {
    if (!cr_net_awaitable_has_magic(state) || !state->terminal_ready) {
        return NULL;
    }
    return &state->completion;
}

const cr_error *cr_net_receive_awaitable_error(
    const cr_net_receive_awaitable_state *state
) {
    if (!cr_net_awaitable_has_magic(state) || state->error.code == 0) {
        return NULL;
    }
    return &state->error;
}
"#;
