#include "cr_backend_internal.h"
#include "cr_waker.h"

#include <assert.h>
#include <stddef.h>
#include <stdlib.h>

typedef union opaque_storage {
    max_align_t alignment;
    unsigned char bytes[512];
} opaque_storage;

static size_t backend_allocations;
static size_t backend_frees;
static size_t provider_allocations;
static size_t provider_frees;
static size_t tracking_allocations;
static size_t tracking_frees;
static size_t awaitable_allocations;
static size_t awaitable_frees;
static size_t operation_allocations;
static size_t operation_frees;

void *test_backend_calloc(size_t count, size_t size) {
    backend_allocations++;
    return calloc(count, size);
}

void test_backend_free(void *allocation) {
    backend_frees++;
    free(allocation);
}

void *test_provider_calloc(size_t count, size_t size) {
    provider_allocations++;
    return calloc(count, size);
}

void test_provider_free(void *allocation) {
    provider_frees++;
    free(allocation);
}

void *test_tracking_calloc(size_t count, size_t size) {
    tracking_allocations++;
    return calloc(count, size);
}

void test_tracking_free(void *allocation) {
    tracking_frees++;
    free(allocation);
}

void *test_awaitable_calloc(size_t count, size_t size) {
    awaitable_allocations++;
    return calloc(count, size);
}

void test_awaitable_free(void *allocation) {
    awaitable_frees++;
    free(allocation);
}

void *test_operation_calloc(size_t count, size_t size) {
    operation_allocations++;
    return calloc(count, size);
}

void test_operation_free(void *allocation) {
    operation_frees++;
    free(allocation);
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
    opaque_storage awaitable_storage = {0};
    opaque_storage operation_storage = {0};
    opaque_storage cancel_awaitable_storage = {0};
    opaque_storage cancel_operation_storage = {0};
    cr_net_receive_awaitable_state *state =
        (cr_net_receive_awaitable_state *)(void *)awaitable_storage.bytes;
    cr_net_receive_operation *operation =
        (cr_net_receive_operation *)(void *)operation_storage.bytes;
    cr_net_receive_awaitable_state *cancel_state =
        (cr_net_receive_awaitable_state *)(void *)
            cancel_awaitable_storage.bytes;
    cr_net_receive_operation *cancel_operation =
        (cr_net_receive_operation *)(void *)cancel_operation_storage.bytes;
    unsigned char buffer[16] = {0};
    unsigned char cancel_buffer[16] = {0};
    cr_awaitable awaitable;
    uint64_t bytes = UINT64_C(0);
    size_t allocations_after_create;

    assert(cr_backend_create(
        &cr_backend_memory_provider_desc,
        &backend,
        &backend_error
    ));
    assert(backend_allocations == 1u);
    assert(provider_allocations == 1u);
    allocations_after_create = backend_allocations + provider_allocations;

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
            (uintptr_t)200u
        },
        buffer,
        sizeof(buffer),
        &awaitable,
        &error
    ));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &bytes) == CR_POLL_PENDING);
    assert(
        backend_allocations + provider_allocations == allocations_after_create
    );
    assert(tracking_allocations == 0u);
    assert(awaitable_allocations == 0u);
    assert(operation_allocations == 0u);

    assert(cr_backend_memory_complete_ready(
        backend,
        operation,
        "data",
        UINT64_C(4),
        &net_error
    ));
    assert(
        backend_allocations + provider_allocations == allocations_after_create
    );
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &bytes) == CR_POLL_READY);
    assert(bytes == UINT64_C(4));
    assert(
        backend_allocations + provider_allocations == allocations_after_create
    );
    awaitable.vtable->drop(awaitable.state);
    assert(
        backend_allocations + provider_allocations == allocations_after_create
    );
    assert(tracking_allocations == 0u && tracking_frees == 0u);
    assert(awaitable_allocations == 0u && awaitable_frees == 0u);
    assert(operation_allocations == 0u && operation_frees == 0u);

    assert(cr_net_receive_awaitable_initialize(
        cancel_state,
        sizeof(cancel_awaitable_storage),
        backend,
        net,
        cancel_operation,
        sizeof(cancel_operation_storage),
        (cr_native_socket_handle){
            CR_NATIVE_SOCKET_MEMORY,
            UINT32_C(0),
            (uintptr_t)201u
        },
        cancel_buffer,
        sizeof(cancel_buffer),
        &awaitable,
        &error
    ));
    assert(awaitable.vtable->poll(awaitable.state, NULL, &bytes) == CR_POLL_PENDING);
    assert(cr_net_receive_awaitable_cancel(cancel_state, &error));
    assert(
        backend_allocations + provider_allocations == allocations_after_create
    );
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(
        awaitable.vtable->poll(awaitable.state, NULL, &bytes) ==
        CR_POLL_CANCELED
    );
    awaitable.vtable->drop(awaitable.state);
    assert(
        backend_allocations + provider_allocations == allocations_after_create
    );
    assert(tracking_allocations == 0u && tracking_frees == 0u);
    assert(awaitable_allocations == 0u && awaitable_frees == 0u);
    assert(operation_allocations == 0u && operation_frees == 0u);

    assert(cr_backend_shutdown(backend, &backend_error));
    assert(cr_backend_destroy(backend, &backend_error));
    assert(backend_frees == backend_allocations);
    assert(provider_frees == provider_allocations);
    return 0;
}
