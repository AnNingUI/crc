#include "cr_backend_internal.h"
#include "cr_waker.h"

#include <assert.h>
#include <stddef.h>
#include <string.h>

typedef union opaque_storage {
    max_align_t alignment;
    unsigned char bytes[512];
} opaque_storage;

typedef struct immediate_trace {
    cr_backend *backend;
    bool fired;
} immediate_trace;

static cr_net_receive_awaitable_state *awaitable_state(
    opaque_storage *storage
) {
    return (cr_net_receive_awaitable_state *)(void *)storage->bytes;
}

static cr_net_receive_operation *operation_state(opaque_storage *storage) {
    return (cr_net_receive_operation *)(void *)storage->bytes;
}

static cr_native_socket_handle memory_socket(uintptr_t value) {
    return (cr_native_socket_handle){
        CR_NATIVE_SOCKET_MEMORY,
        UINT32_C(0),
        value
    };
}

static void complete_during_submit(
    void *context,
    cr_backend_memory_trace_event event,
    const cr_net_receive_operation *operation,
    uint64_t generation
) {
    immediate_trace *trace = (immediate_trace *)context;
    cr_net_error net_error;
    cr_backend_pump_result pump;
    (void)generation;

    if (event != CR_BACKEND_MEMORY_TRACE_SUBMITTED || trace->fired) return;
    trace->fired = true;
    assert(cr_backend_memory_complete_ready(
        trace->backend,
        (cr_net_receive_operation *)(void *)operation,
        "now",
        UINT64_C(3),
        &net_error
    ));
    assert(cr_backend_pump(
        trace->backend,
        UINT64_C(0),
        UINT32_C(1),
        &pump
    ));
    assert(pump.reason == CR_BACKEND_PUMP_PROGRESS);
}

int main(void) {
    const cr_extension_id net_id = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    cr_backend *backend = NULL;
    cr_backend_error backend_error;
    cr_net_error net_error;
    cr_error awaitable_error;
    cr_backend_pump_result pump;
    const cr_backend_extension_desc *base;
    const cr_net_extension_desc *net;
    cr_storage_layout state_layout;
    opaque_storage states[5] = {0};
    opaque_storage operations[5] = {0};
    unsigned char buffers[5][16] = {{0}};
    cr_awaitable awaitable;
    uint64_t result = UINT64_MAX;
    immediate_trace immediate;
    const cr_net_receive_completion *completion;
    const cr_error *error;

    assert(cr_backend_create(
        &cr_backend_memory_provider_desc,
        &backend,
        &backend_error
    ));
    base = cr_backend_query_extension(
        backend,
        net_id,
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        &backend_error
    );
    assert(base != NULL);
    net = (const cr_net_extension_desc *)(const void *)base;
    state_layout = cr_net_receive_awaitable_state_layout();
    assert(cr_storage_layout_is_valid(&state_layout));
    assert(state_layout.size <= sizeof(opaque_storage));
    assert(state_layout.alignment <= _Alignof(opaque_storage));
    assert(net->receive_operation_layout.size <= sizeof(opaque_storage));
    assert(net->receive_operation_layout.alignment <= _Alignof(opaque_storage));

    immediate = (immediate_trace){backend, false};
    assert(cr_backend_memory_set_trace(
        backend,
        complete_during_submit,
        &immediate,
        &backend_error
    ));
    assert(cr_net_receive_awaitable_initialize(
        awaitable_state(&states[0]),
        sizeof(states[0]),
        backend,
        net,
        operation_state(&operations[0]),
        sizeof(operations[0]),
        memory_socket((uintptr_t)10u),
        buffers[0],
        sizeof(buffers[0]),
        &awaitable,
        &awaitable_error
    ));
    assert(awaitable.vtable->required_context_capabilities == UINT64_C(0));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &result) == CR_POLL_READY);
    assert(immediate.fired);
    assert(result == UINT64_C(3));
    assert(memcmp(buffers[0], "now", 3u) == 0);
    awaitable.vtable->drop(awaitable.state);
    assert(cr_backend_memory_set_trace(
        backend,
        NULL,
        NULL,
        &backend_error
    ));

    result = UINT64_MAX;
    assert(cr_net_receive_awaitable_initialize(
        awaitable_state(&states[1]),
        sizeof(states[1]),
        backend,
        net,
        operation_state(&operations[1]),
        sizeof(operations[1]),
        memory_socket((uintptr_t)11u),
        buffers[1],
        sizeof(buffers[1]),
        &awaitable,
        &awaitable_error
    ));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &result) == CR_POLL_PENDING);
    assert(cr_backend_memory_complete_ready(
        backend,
        operation_state(&operations[1]),
        NULL,
        UINT64_C(0),
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &result) == CR_POLL_READY);
    assert(result == UINT64_C(0));
    completion = cr_net_receive_awaitable_completion(
        awaitable_state(&states[1])
    );
    assert(completion != NULL);
    assert(completion->terminal_kind == CR_NET_RECEIVE_READY);
    buffers[1][0] = 0x5au;
    awaitable.vtable->drop(awaitable.state);

    assert(cr_net_receive_awaitable_initialize(
        awaitable_state(&states[2]),
        sizeof(states[2]),
        backend,
        net,
        operation_state(&operations[2]),
        sizeof(operations[2]),
        memory_socket((uintptr_t)12u),
        buffers[2],
        sizeof(buffers[2]),
        &awaitable,
        &awaitable_error
    ));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &result) == CR_POLL_PENDING);
    assert(cr_backend_memory_complete_error(
        backend,
        operation_state(&operations[2]),
        CR_NET_ERROR_NETWORK_FAILURE,
        CR_NATIVE_ERROR_DOMAIN_ERRNO,
        INT64_C(77),
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &result) == CR_POLL_ERROR);
    error = awaitable.vtable->error(awaitable.state);
    assert(error != NULL);
    assert(error->code == CR_ERROR_NET_RECEIVE_NETWORK_FAILURE);
    completion = cr_net_receive_awaitable_completion(
        awaitable_state(&states[2])
    );
    assert(completion->native_error_domain == CR_NATIVE_ERROR_DOMAIN_ERRNO);
    assert(completion->native_error_code == INT64_C(77));
    assert(cr_net_receive_awaitable_error(awaitable_state(&states[2])) == error);
    awaitable.vtable->drop(awaitable.state);

    assert(cr_net_receive_awaitable_initialize(
        awaitable_state(&states[3]),
        sizeof(states[3]),
        backend,
        net,
        operation_state(&operations[3]),
        sizeof(operations[3]),
        memory_socket((uintptr_t)13u),
        buffers[3],
        sizeof(buffers[3]),
        &awaitable,
        &awaitable_error
    ));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &result) == CR_POLL_PENDING);
    assert(cr_net_receive_awaitable_cancel(
        awaitable_state(&states[3]),
        &awaitable_error
    ));
    assert(cr_net_receive_awaitable_cancel(
        awaitable_state(&states[3]),
        &awaitable_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(
        awaitable.vtable->poll(awaitable.state, NULL, &result) ==
        CR_POLL_CANCELED
    );
    awaitable.vtable->drop(awaitable.state);

    assert(cr_net_receive_awaitable_initialize(
        awaitable_state(&states[4]),
        sizeof(states[4]),
        backend,
        net,
        operation_state(&operations[4]),
        sizeof(operations[4]),
        memory_socket((uintptr_t)14u),
        buffers[4],
        sizeof(buffers[4]),
        &awaitable,
        &awaitable_error
    ));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &result) == CR_POLL_PENDING);
    awaitable.vtable->drop(awaitable.state);
    completion = cr_net_receive_awaitable_completion(
        awaitable_state(&states[4])
    );
    assert(completion != NULL);
    assert(completion->terminal_kind == CR_NET_RECEIVE_CANCELED);

    assert(cr_backend_destroy(backend, &backend_error));
    return 0;
}
