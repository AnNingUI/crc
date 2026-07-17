#include "cr_net.h"

#include <assert.h>

static bool create_provider(
    const cr_backend_provider_desc *provider,
    void **out_provider_state,
    cr_backend_error *out_error
) {
    (void)provider;
    (void)out_error;
    *out_provider_state = out_provider_state;
    return true;
}

static const cr_backend_extension_desc *query_extension(
    void *provider_state,
    cr_extension_id extension_id,
    uint32_t requested_abi_version,
    cr_backend_error *out_error
) {
    (void)provider_state;
    (void)extension_id;
    (void)requested_abi_version;
    (void)out_error;
    return NULL;
}

static bool pump_provider(
    void *provider_state,
    uint64_t timeout_ns,
    uint32_t max_events,
    cr_backend_pump_result *out_result
) {
    (void)provider_state;
    (void)timeout_ns;
    (void)max_events;
    (void)out_result;
    return true;
}

static bool control_provider(void *provider_state, cr_backend_error *out_error) {
    (void)provider_state;
    (void)out_error;
    return true;
}

static void destroy_provider(void *provider_state) {
    (void)provider_state;
}

static bool receive_initialize(
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
    (void)backend;
    (void)operation;
    (void)operation_storage_size;
    (void)socket;
    (void)buffer;
    (void)buffer_size;
    (void)on_completion;
    (void)callback_context;
    (void)out_error;
    return true;
}

static bool receive_control(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
) {
    (void)backend;
    (void)operation;
    (void)out_error;
    return true;
}

typedef struct future_provider_desc {
    cr_backend_provider_desc v1;
    uint64_t unknown_tail;
} future_provider_desc;

typedef struct future_net_desc {
    cr_net_extension_desc v1;
    uint64_t unknown_tail;
} future_net_desc;

int main(void) {
    const cr_extension_id backend_id = CR_BACKEND_CORE_ID_INIT;
    const cr_extension_id net_id = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    const cr_extension_id invalid_id = {0u, 0u};
    future_provider_desc provider = {
        {
            CR_BACKEND_ABI_VERSION,
            sizeof(future_provider_desc),
            UINT64_C(1) << 63,
            CR_BACKEND_CORE_ID_INIT,
            create_provider,
            query_extension,
            pump_provider,
            control_provider,
            control_provider,
            destroy_provider
        },
        UINT64_C(0xfeedface)
    };
    cr_backend_provider_desc truncated_provider = provider.v1;
    cr_backend_provider_desc invalid_provider = provider.v1;
    future_net_desc net = {
        {
            {
                CR_NET_ABI_VERSION,
                sizeof(future_net_desc),
                UINT64_C(1) << 62,
                CR_NET_RECEIVE_EXTENSION_ID_INIT
            },
            {
                CR_BACKEND_ABI_VERSION,
                sizeof(cr_storage_layout),
                UINT64_C(128),
                UINT64_C(16)
            },
            receive_initialize,
            receive_control,
            receive_control,
            receive_control,
            receive_control
        },
        UINT64_C(0xcafebabe)
    };
    future_net_desc truncated_net = net;
    future_net_desc invalid_net = net;
    cr_storage_layout invalid_alignment = net.v1.receive_operation_layout;
    cr_net_receive_completion completion = {
        CR_NET_ABI_VERSION,
        sizeof(cr_net_receive_completion),
        CR_NET_RECEIVE_READY,
        CR_NET_ERROR_NONE,
        UINT64_C(4),
        CR_NATIVE_ERROR_DOMAIN_NONE,
        0u,
        INT64_C(0)
    };

    assert(cr_extension_id_is_valid(backend_id));
    assert(cr_extension_id_is_valid(net_id));
    assert(!cr_extension_id_is_valid(invalid_id));
    assert(!cr_extension_id_equal(backend_id, net_id));

    assert(cr_backend_provider_desc_is_compatible(&provider.v1));
    truncated_provider.struct_size = CR_BACKEND_PROVIDER_DESC_PREFIX_SIZE;
    assert(!cr_backend_provider_desc_is_compatible(&truncated_provider));
    invalid_provider.provider_id = invalid_id;
    assert(!cr_backend_provider_desc_is_compatible(&invalid_provider));

    assert(cr_backend_extension_desc_is_compatible(
        &net.v1.base,
        net_id,
        CR_NET_ABI_VERSION
    ));
    assert(!cr_backend_extension_desc_is_compatible(
        &net.v1.base,
        backend_id,
        CR_NET_ABI_VERSION
    ));
    assert(!cr_backend_extension_desc_is_compatible(
        &net.v1.base,
        net_id,
        CR_NET_ABI_VERSION + 1u
    ));

    assert(cr_net_extension_desc_is_compatible(&net.v1));
    truncated_net.v1.base.struct_size = CR_BACKEND_EXTENSION_DESC_PREFIX_SIZE;
    assert(!cr_net_extension_desc_is_compatible(&truncated_net.v1));
    invalid_net.v1.base.extension_id = invalid_id;
    assert(!cr_net_extension_desc_is_compatible(&invalid_net.v1));

    assert(cr_storage_layout_is_valid(&net.v1.receive_operation_layout));
    invalid_alignment.alignment = UINT64_C(3);
    assert(!cr_storage_layout_is_valid(&invalid_alignment));
    assert(cr_net_receive_completion_has_v1_prefix(&completion));
    completion.struct_size = CR_NET_RECEIVE_COMPLETION_V1_MIN_SIZE - 1u;
    assert(!cr_net_receive_completion_has_v1_prefix(&completion));
    return 0;
}
