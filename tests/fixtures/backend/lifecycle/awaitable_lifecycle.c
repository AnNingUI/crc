#include "cr_backend_internal.h"
#include "cr_waker.h"

#include <assert.h>
#include <stddef.h>
#include <string.h>

typedef union opaque_storage {
    max_align_t alignment;
    unsigned char bytes[512];
} opaque_storage;

typedef struct guarded_buffer {
    unsigned char before[8];
    unsigned char payload[16];
    unsigned char after[8];
} guarded_buffer;

typedef struct drop_trace {
    bool in_drop;
    bool callback_during_drop;
    uint32_t submitted;
    uint32_t terminal_callbacks;
    uint32_t quiescent;
    uint32_t destroyed;
} drop_trace;

static void trace_drop(
    void *context,
    cr_backend_memory_trace_event event,
    const cr_net_receive_operation *operation,
    uint64_t generation
) {
    drop_trace *trace = (drop_trace *)context;
    (void)operation;
    (void)generation;
    if (event == CR_BACKEND_MEMORY_TRACE_SUBMITTED) trace->submitted++;
    if (event == CR_BACKEND_MEMORY_TRACE_TERMINAL_CALLBACK) {
        trace->terminal_callbacks++;
        if (trace->in_drop) trace->callback_during_drop = true;
    }
    if (event == CR_BACKEND_MEMORY_TRACE_QUIESCENT) trace->quiescent++;
    if (event == CR_BACKEND_MEMORY_TRACE_DESTROYED) trace->destroyed++;
}

static void fill_guards(guarded_buffer *buffer) {
    memset(buffer->before, 0xa5, sizeof(buffer->before));
    memset(buffer->payload, 0, sizeof(buffer->payload));
    memset(buffer->after, 0x5a, sizeof(buffer->after));
}

static void assert_guards(const guarded_buffer *buffer) {
    for (size_t index = 0; index < sizeof(buffer->before); index++) {
        assert(buffer->before[index] == 0xa5u);
    }
    for (size_t index = 0; index < sizeof(buffer->after); index++) {
        assert(buffer->after[index] == 0x5au);
    }
}

int main(void) {
    const cr_extension_id net_id = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    cr_backend *backend = NULL;
    cr_backend_error backend_error;
    cr_backend_pump_result pump;
    cr_net_error net_error;
    cr_error error;
    const cr_backend_extension_desc *base;
    const cr_net_extension_desc *net;
    opaque_storage awaitable_storage[3] = {0};
    opaque_storage operation_storage[3] = {0};
    guarded_buffer buffers[3];
    cr_awaitable awaitable;
    uint64_t bytes = UINT64_C(0);
    drop_trace trace = {0};
    const cr_net_receive_completion *completion;

    for (size_t index = 0; index < 3u; index++) fill_guards(&buffers[index]);
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
    assert(cr_backend_memory_set_trace(
        backend,
        trace_drop,
        &trace,
        &backend_error
    ));

    assert(cr_net_receive_awaitable_initialize(
        (cr_net_receive_awaitable_state *)(void *)awaitable_storage[0].bytes,
        sizeof(awaitable_storage[0]),
        backend,
        net,
        (cr_net_receive_operation *)(void *)operation_storage[0].bytes,
        sizeof(operation_storage[0]),
        (cr_native_socket_handle){
            CR_NATIVE_SOCKET_MEMORY,
            UINT32_C(0),
            (uintptr_t)300u
        },
        buffers[0].payload,
        sizeof(buffers[0].payload),
        &awaitable,
        &error
    ));
    awaitable.vtable->drop(awaitable.state);
    assert(trace.submitted == UINT32_C(0));
    assert(trace.terminal_callbacks == UINT32_C(0));
    assert_guards(&buffers[0]);

    assert(cr_net_receive_awaitable_initialize(
        (cr_net_receive_awaitable_state *)(void *)awaitable_storage[1].bytes,
        sizeof(awaitable_storage[1]),
        backend,
        net,
        (cr_net_receive_operation *)(void *)operation_storage[1].bytes,
        sizeof(operation_storage[1]),
        (cr_native_socket_handle){
            CR_NATIVE_SOCKET_MEMORY,
            UINT32_C(0),
            (uintptr_t)301u
        },
        buffers[1].payload,
        sizeof(buffers[1].payload),
        &awaitable,
        &error
    ));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &bytes) == CR_POLL_PENDING);
    trace.in_drop = true;
    awaitable.vtable->drop(awaitable.state);
    trace.in_drop = false;
    assert(trace.submitted == UINT32_C(1));
    assert(trace.terminal_callbacks == UINT32_C(1));
    assert(trace.callback_during_drop);
    assert(trace.quiescent == UINT32_C(1));
    assert(trace.destroyed == UINT32_C(1));
    completion = cr_net_receive_awaitable_completion(
        (cr_net_receive_awaitable_state *)(void *)awaitable_storage[1].bytes
    );
    assert(completion != NULL);
    assert(completion->terminal_kind == CR_NET_RECEIVE_CANCELED);
    assert_guards(&buffers[1]);
    assert(!cr_backend_memory_complete_ready(
        backend,
        (cr_net_receive_operation *)(void *)operation_storage[1].bytes,
        "late",
        UINT64_C(4),
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);
    assert(trace.terminal_callbacks == UINT32_C(1));
    assert_guards(&buffers[1]);

    assert(cr_net_receive_awaitable_initialize(
        (cr_net_receive_awaitable_state *)(void *)awaitable_storage[2].bytes,
        sizeof(awaitable_storage[2]),
        backend,
        net,
        (cr_net_receive_operation *)(void *)operation_storage[2].bytes,
        sizeof(operation_storage[2]),
        (cr_native_socket_handle){
            CR_NATIVE_SOCKET_MEMORY,
            UINT32_C(0),
            (uintptr_t)302u
        },
        buffers[2].payload,
        sizeof(buffers[2].payload),
        &awaitable,
        &error
    ));
    assert(cr_net_receive_awaitable_cancel(
        (cr_net_receive_awaitable_state *)(void *)awaitable_storage[2].bytes,
        &error
    ));
    assert(
        awaitable.vtable->poll(awaitable.state, NULL, &bytes) ==
        CR_POLL_CANCELED
    );
    awaitable.vtable->drop(awaitable.state);
    assert(trace.submitted == UINT32_C(1));
    assert_guards(&buffers[2]);

    assert(cr_backend_destroy(backend, &backend_error));
    return 0;
}
