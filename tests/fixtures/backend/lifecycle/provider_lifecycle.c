#include "cr_backend_internal.h"

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

typedef struct observation {
    uint32_t calls;
    cr_net_receive_completion completion;
    const cr_net_extension_desc *net;
    cr_backend *backend;
    cr_net_receive_operation *operation;
    bool cancel_during_callback;
    bool cancel_result;
    cr_net_error cancel_error;
    uint32_t id;
    uint32_t *order;
    uint32_t *order_length;
} observation;

static cr_net_receive_operation *operation_at(opaque_storage *storage) {
    return (cr_net_receive_operation *)(void *)storage->bytes;
}

static cr_native_socket_handle memory_socket(uintptr_t value) {
    return (cr_native_socket_handle){
        CR_NATIVE_SOCKET_MEMORY,
        UINT32_C(0),
        value
    };
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

static void observe_completion(
    void *context,
    const cr_net_receive_completion *completion
) {
    observation *result = (observation *)context;
    result->calls++;
    result->completion = *completion;
    if (result->order != NULL) {
        result->order[*result->order_length] = result->id;
        (*result->order_length)++;
    }
    if (result->cancel_during_callback) {
        result->cancel_result = result->net->receive_cancel(
            result->backend,
            result->operation,
            &result->cancel_error
        );
    }
}

static void initialize_operation(
    const cr_net_extension_desc *net,
    cr_backend *backend,
    opaque_storage *storage,
    uintptr_t socket_value,
    void *buffer,
    uint64_t buffer_size,
    observation *result
) {
    cr_net_error error;
    cr_net_receive_operation *operation = operation_at(storage);
    result->net = net;
    result->backend = backend;
    result->operation = operation;
    assert(net->receive_initialize(
        backend,
        operation,
        sizeof(*storage),
        memory_socket(socket_value),
        buffer,
        buffer_size,
        observe_completion,
        result,
        &error
    ));
}

static void submit_operation(
    const cr_net_extension_desc *net,
    cr_backend *backend,
    opaque_storage *storage
) {
    cr_net_error error;
    assert(net->receive_submit(backend, operation_at(storage), &error));
}

static void quiesce_and_destroy(
    const cr_net_extension_desc *net,
    cr_backend *backend,
    opaque_storage *storage
) {
    cr_net_error error;
    assert(net->receive_quiesce(backend, operation_at(storage), &error));
    assert(net->receive_destroy(backend, operation_at(storage), &error));
}

int main(void) {
    const cr_extension_id net_id = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    cr_backend *backend = NULL;
    cr_backend_error backend_error;
    cr_backend_pump_result pump;
    cr_net_error net_error;
    const cr_backend_extension_desc *base;
    const cr_net_extension_desc *net;
    opaque_storage storage[12] = {0};
    guarded_buffer buffers[12];
    observation result[12] = {0};
    uint32_t order[8] = {0};
    uint32_t order_length = 0;
    const char payload[] = "safe";

    for (size_t index = 0; index < 12u; index++) fill_guards(&buffers[index]);
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

    initialize_operation(
        net,
        backend,
        &storage[0],
        (uintptr_t)100u,
        buffers[0].payload,
        sizeof(buffers[0].payload),
        &result[0]
    );
    submit_operation(net, backend, &storage[0]);

    initialize_operation(
        net,
        backend,
        &storage[1],
        (uintptr_t)100u,
        buffers[1].payload,
        sizeof(buffers[1].payload),
        &result[1]
    );
    assert(!net->receive_submit(
        backend,
        operation_at(&storage[1]),
        &net_error
    ));
    assert(net_error.category == CR_NET_ERROR_BUSY);
    assert(result[1].calls == UINT32_C(0));
    assert(net->receive_destroy(backend, operation_at(&storage[1]), &net_error));

    initialize_operation(
        net,
        backend,
        &storage[2],
        (uintptr_t)102u,
        buffers[0].payload,
        sizeof(buffers[0].payload),
        &result[2]
    );
    assert(!net->receive_submit(
        backend,
        operation_at(&storage[2]),
        &net_error
    ));
    assert(net_error.category == CR_NET_ERROR_BUSY);
    assert(result[2].calls == UINT32_C(0));
    assert(net->receive_destroy(backend, operation_at(&storage[2]), &net_error));

    assert(!net->receive_initialize(
        backend,
        operation_at(&storage[0]),
        sizeof(storage[0]),
        memory_socket((uintptr_t)103u),
        buffers[3].payload,
        sizeof(buffers[3].payload),
        observe_completion,
        &result[3],
        &net_error
    ));
    assert(net_error.category == CR_NET_ERROR_BUSY);

    assert(net->receive_cancel(backend, operation_at(&storage[0]), &net_error));
    assert(net->receive_cancel(backend, operation_at(&storage[0]), &net_error));
    assert(!cr_backend_memory_complete_ready(
        backend,
        operation_at(&storage[0]),
        payload,
        sizeof(payload) - 1u,
        &net_error
    ));
    assert(net_error.category == CR_NET_ERROR_BUSY);
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(result[0].calls == UINT32_C(1));
    assert(result[0].completion.terminal_kind == CR_NET_RECEIVE_CANCELED);
    assert_guards(&buffers[0]);
    assert(net->receive_quiesce(backend, operation_at(&storage[0]), &net_error));
    assert(!cr_backend_memory_complete_ready(
        backend,
        operation_at(&storage[0]),
        payload,
        sizeof(payload) - 1u,
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);
    assert(result[0].calls == UINT32_C(1));
    assert(net->receive_destroy(backend, operation_at(&storage[0]), &net_error));

    initialize_operation(
        net,
        backend,
        &storage[3],
        (uintptr_t)103u,
        buffers[3].payload,
        sizeof(buffers[3].payload),
        &result[3]
    );
    submit_operation(net, backend, &storage[3]);
    assert(cr_backend_memory_complete_ready(
        backend,
        operation_at(&storage[3]),
        payload,
        sizeof(payload) - 1u,
        &net_error
    ));
    assert(net->receive_cancel(backend, operation_at(&storage[3]), &net_error));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(result[3].calls == UINT32_C(1));
    assert(result[3].completion.terminal_kind == CR_NET_RECEIVE_READY);
    assert(memcmp(buffers[3].payload, payload, sizeof(payload) - 1u) == 0);
    assert_guards(&buffers[3]);
    quiesce_and_destroy(net, backend, &storage[3]);

    initialize_operation(
        net,
        backend,
        &storage[4],
        (uintptr_t)104u,
        buffers[4].payload,
        sizeof(buffers[4].payload),
        &result[4]
    );
    submit_operation(net, backend, &storage[4]);
    assert(cr_backend_memory_complete_error(
        backend,
        operation_at(&storage[4]),
        CR_NET_ERROR_NETWORK_FAILURE,
        CR_NATIVE_ERROR_DOMAIN_ERRNO,
        INT64_C(91),
        &net_error
    ));
    assert(net->receive_cancel(backend, operation_at(&storage[4]), &net_error));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(result[4].calls == UINT32_C(1));
    assert(result[4].completion.terminal_kind == CR_NET_RECEIVE_ERROR);
    assert(result[4].completion.native_error_code == INT64_C(91));
    quiesce_and_destroy(net, backend, &storage[4]);

    result[5].cancel_during_callback = true;
    initialize_operation(
        net,
        backend,
        &storage[5],
        (uintptr_t)105u,
        buffers[5].payload,
        sizeof(buffers[5].payload),
        &result[5]
    );
    submit_operation(net, backend, &storage[5]);
    assert(cr_backend_memory_complete_ready(
        backend,
        operation_at(&storage[5]),
        payload,
        sizeof(payload) - 1u,
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(result[5].calls == UINT32_C(1));
    assert(!result[5].cancel_result);
    assert(result[5].cancel_error.category == CR_NET_ERROR_INVALID_ARGUMENT);
    quiesce_and_destroy(net, backend, &storage[5]);

    result[6].id = UINT32_C(6);
    result[6].order = order;
    result[6].order_length = &order_length;
    result[7].id = UINT32_C(7);
    result[7].order = order;
    result[7].order_length = &order_length;
    initialize_operation(
        net,
        backend,
        &storage[6],
        (uintptr_t)106u,
        buffers[6].payload,
        sizeof(buffers[6].payload),
        &result[6]
    );
    submit_operation(net, backend, &storage[6]);
    initialize_operation(
        net,
        backend,
        &storage[7],
        (uintptr_t)107u,
        buffers[7].payload,
        sizeof(buffers[7].payload),
        &result[7]
    );
    submit_operation(net, backend, &storage[7]);
    assert(cr_backend_memory_complete_ready(
        backend,
        operation_at(&storage[7]),
        payload,
        sizeof(payload) - 1u,
        &net_error
    ));
    assert(net->receive_quiesce(backend, operation_at(&storage[6]), &net_error));
    assert(order_length == UINT32_C(2));
    assert(order[0] == UINT32_C(7));
    assert(order[1] == UINT32_C(6));
    assert(result[7].completion.terminal_kind == CR_NET_RECEIVE_READY);
    assert(result[6].completion.terminal_kind == CR_NET_RECEIVE_CANCELED);
    assert(net->receive_destroy(backend, operation_at(&storage[6]), &net_error));
    quiesce_and_destroy(net, backend, &storage[7]);

    initialize_operation(
        net,
        backend,
        &storage[8],
        (uintptr_t)108u,
        buffers[8].payload,
        sizeof(buffers[8].payload),
        &result[8]
    );
    submit_operation(net, backend, &storage[8]);
    initialize_operation(
        net,
        backend,
        &storage[9],
        (uintptr_t)109u,
        buffers[9].payload,
        sizeof(buffers[9].payload),
        &result[9]
    );
    submit_operation(net, backend, &storage[9]);
    assert(cr_backend_memory_complete_ready(
        backend,
        operation_at(&storage[8]),
        payload,
        sizeof(payload) - 1u,
        &net_error
    ));
    assert(cr_backend_shutdown(backend, &backend_error));
    assert(result[8].calls == UINT32_C(1));
    assert(result[8].completion.terminal_kind == CR_NET_RECEIVE_READY);
    assert(result[9].calls == UINT32_C(1));
    assert(result[9].completion.terminal_kind == CR_NET_RECEIVE_CANCELED);
    assert(net->receive_destroy(backend, operation_at(&storage[8]), &net_error));
    assert(net->receive_destroy(backend, operation_at(&storage[9]), &net_error));

    assert(cr_backend_destroy(backend, &backend_error));
    return 0;
}
