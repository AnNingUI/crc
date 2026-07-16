#include "cr_backend_internal.h"

#include <assert.h>
#include <stddef.h>

typedef union operation_storage {
    max_align_t alignment;
    unsigned char bytes[512];
} operation_storage;

static int owner_a;
static int owner_b;
static const void *current_owner = &owner_a;

const void *test_owner_token(void) {
    return current_owner;
}

static void observe_completion(
    void *context,
    const cr_net_receive_completion *completion
) {
    (void)context;
    (void)completion;
    assert(0 && "wrong-owner operation must not complete");
}

int main(void) {
    const cr_extension_id net_id = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    cr_backend *backend = NULL;
    cr_backend_error backend_error;
    cr_backend_pump_result pump;
    cr_net_error net_error;
    const cr_backend_extension_desc *base;
    const cr_net_extension_desc *net;
    operation_storage storage = {0};
    unsigned char buffer[8] = {0};

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

    current_owner = &owner_b;
    assert(cr_backend_query_extension(
        backend,
        net_id,
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        &backend_error
    ) == NULL);
    assert(backend_error.category == CR_BACKEND_ERROR_WRONG_THREAD);
    assert(!cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.error_category == CR_BACKEND_ERROR_WRONG_THREAD);
    assert(!net->receive_initialize(
        backend,
        (cr_net_receive_operation *)(void *)storage.bytes,
        sizeof(storage),
        (cr_native_socket_handle){
            CR_NATIVE_SOCKET_MEMORY,
            UINT32_C(0),
            (uintptr_t)1u
        },
        buffer,
        sizeof(buffer),
        observe_completion,
        NULL,
        &net_error
    ));
    assert(net_error.category == CR_NET_ERROR_WRONG_THREAD);
    assert(!cr_backend_shutdown(backend, &backend_error));
    assert(backend_error.category == CR_BACKEND_ERROR_WRONG_THREAD);

    assert(cr_backend_interrupt(backend, &backend_error));
    current_owner = &owner_a;
    assert(cr_backend_pump(backend, UINT64_MAX, UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_INTERRUPTED);
    assert(cr_backend_destroy(backend, &backend_error));
    return 0;
}
