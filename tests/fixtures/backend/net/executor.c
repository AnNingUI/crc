#include "cr_backend_internal.h"
#include "cr_executor.h"

#include <assert.h>
#include <stddef.h>

typedef union opaque_storage {
    max_align_t alignment;
    unsigned char bytes[512];
} opaque_storage;

typedef struct observation {
    uint32_t calls;
    cr_poll_status status;
    uint64_t bytes;
    unsigned char *buffer;
} observation;

static void observe_receive(
    void *context,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    observation *result = (observation *)context;
    result->calls++;
    result->status = status;
    assert(status == CR_POLL_READY);
    assert(value != NULL);
    assert(error == NULL);
    result->bytes = *(const uint64_t *)value;
    result->buffer[0] = (unsigned char)'Z';
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
    cr_executor *executor;
    cr_executor_task *ticket = NULL;
    opaque_storage awaitable_storage = {0};
    opaque_storage operation_storage = {0};
    unsigned char buffer[16] = {0};
    cr_net_receive_awaitable_state *state =
        (cr_net_receive_awaitable_state *)(void *)awaitable_storage.bytes;
    cr_net_receive_operation *operation =
        (cr_net_receive_operation *)(void *)operation_storage.bytes;
    cr_awaitable awaitable;
    observation result = {0, CR_POLL_PENDING, UINT64_C(0), buffer};

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
    assert(cr_net_receive_awaitable_initialize(
        state,
        sizeof(awaitable_storage),
        backend,
        net,
        operation,
        sizeof(operation_storage),
        (cr_native_socket_handle){
            CR_NATIVE_SOCKET_MEMORY,
            UINT32_C(0),
            (uintptr_t)20u
        },
        buffer,
        sizeof(buffer),
        &awaitable,
        &error
    ));

    executor = cr_executor_create_single(&error);
    assert(executor != NULL);
    assert(cr_executor_spawn(
        executor,
        &awaitable,
        observe_receive,
        &result,
        &error,
        &ticket
    ));
    assert(cr_executor_run_ready(executor) == 1u);
    assert(result.calls == UINT32_C(0));
    assert(cr_backend_memory_complete_ready(
        backend,
        operation,
        "wake",
        UINT64_C(4),
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_MAX, UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_PROGRESS);
    assert(cr_executor_run_ready(executor) == 1u);
    assert(result.calls == UINT32_C(1));
    assert(result.status == CR_POLL_READY);
    assert(result.bytes == UINT64_C(4));
    assert(buffer[0] == (unsigned char)'Z');
    assert(
        cr_net_receive_awaitable_completion(state)->terminal_kind ==
        CR_NET_RECEIVE_READY
    );

    cr_executor_task_release(ticket);
    cr_executor_destroy(executor);
    assert(cr_backend_destroy(backend, &backend_error));
    return 0;
}
