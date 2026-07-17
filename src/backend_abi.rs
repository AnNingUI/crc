//! Stable Backend core and net-receive v1 C ABI declarations.

/// Stable Backend core ABI version.
pub const CR_BACKEND_ABI_VERSION: u32 = 1;

/// Stable net-receive extension ABI version.
pub const CR_NET_ABI_VERSION: u32 = 1;

/// Source-compatible Stage 6 alias for [`CR_BACKEND_ABI_VERSION`].
pub const CR_BACKEND_EXPERIMENTAL_ABI_VERSION: u32 = CR_BACKEND_ABI_VERSION;

/// Source-compatible Stage 6 alias for [`CR_NET_ABI_VERSION`].
pub const CR_NET_EXPERIMENTAL_ABI_VERSION: u32 = CR_NET_ABI_VERSION;

/// Stable 128-bit identity for the Backend core Provider contract.
pub const CR_BACKEND_CORE_ID_HIGH: u64 = 0x4352_5f42_4143_4b45;

/// Low word of the backend core provider contract identity.
pub const CR_BACKEND_CORE_ID_LOW: u64 = 0x4e44_5f43_4f52_4531;

/// Stable 128-bit identity for the one-shot net-receive extension.
pub const CR_NET_RECEIVE_EXTENSION_ID_HIGH: u64 = 0x4352_5f4e_4554_5f52;

/// Low word of the one-shot net-receive extension identity.
pub const CR_NET_RECEIVE_EXTENSION_ID_LOW: u64 = 0x4543_4549_5645_5f31;

/// Returns the stable portable C11 Backend core header.
#[must_use]
pub fn backend_header() -> &'static str {
    r#"#ifndef CR_BACKEND_H
#define CR_BACKEND_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* Stable Backend core v1. Append fields only after each v1 minimum prefix. */
/* CR_STABLE_BACKEND_V1_BEGIN */

typedef struct cr_backend cr_backend;
typedef struct cr_backend_provider_desc cr_backend_provider_desc;
typedef struct cr_backend_extension_desc cr_backend_extension_desc;

typedef struct cr_extension_id {
    uint64_t high;
    uint64_t low;
} cr_extension_id;

typedef uint32_t cr_backend_error_category;
typedef uint32_t cr_native_error_domain;
typedef uint32_t cr_backend_pump_reason;

typedef struct cr_storage_layout {
    uint32_t abi_version;
    uint32_t struct_size;
    uint64_t size;
    uint64_t alignment;
} cr_storage_layout;

typedef struct cr_backend_error {
    uint32_t abi_version;
    uint32_t struct_size;
    cr_backend_error_category category;
    cr_native_error_domain native_domain;
    int64_t native_code;
} cr_backend_error;

typedef struct cr_backend_pump_result {
    uint32_t abi_version;
    uint32_t struct_size;
    cr_backend_pump_reason reason;
    uint32_t events_dispatched;
    cr_backend_error_category error_category;
    cr_native_error_domain native_error_domain;
    int64_t native_error_code;
} cr_backend_pump_result;

typedef bool (*cr_backend_provider_create_fn)(
    const cr_backend_provider_desc *provider,
    void **out_provider_state,
    cr_backend_error *out_error
);

typedef const cr_backend_extension_desc *
(*cr_backend_provider_query_extension_fn)(
    void *provider_state,
    cr_extension_id extension_id,
    uint32_t requested_abi_version,
    cr_backend_error *out_error
);

typedef bool (*cr_backend_provider_pump_fn)(
    void *provider_state,
    uint64_t timeout_ns,
    uint32_t max_events,
    cr_backend_pump_result *out_result
);

typedef bool (*cr_backend_provider_interrupt_fn)(
    void *provider_state,
    cr_backend_error *out_error
);

typedef bool (*cr_backend_provider_shutdown_fn)(
    void *provider_state,
    cr_backend_error *out_error
);

typedef void (*cr_backend_provider_destroy_fn)(void *provider_state);

struct cr_backend_provider_desc {
    uint32_t abi_version;
    uint32_t struct_size;
    uint64_t capability_bits;
    cr_extension_id provider_id;
    cr_backend_provider_create_fn create;
    cr_backend_provider_query_extension_fn query_extension;
    cr_backend_provider_pump_fn pump;
    cr_backend_provider_interrupt_fn interrupt;
    cr_backend_provider_shutdown_fn shutdown;
    cr_backend_provider_destroy_fn destroy;
};

struct cr_backend_extension_desc {
    uint32_t abi_version;
    uint32_t struct_size;
    uint64_t capability_bits;
    cr_extension_id extension_id;
};

#define CR_BACKEND_ABI_VERSION 1u
#define CR_BACKEND_EXPERIMENTAL_ABI_VERSION CR_BACKEND_ABI_VERSION

#define CR_BACKEND_CORE_ID_HIGH UINT64_C(0x43525f4241434b45)
#define CR_BACKEND_CORE_ID_LOW  UINT64_C(0x4e445f434f524531)
#define CR_BACKEND_CORE_ID_INIT \
    {CR_BACKEND_CORE_ID_HIGH, CR_BACKEND_CORE_ID_LOW}

#define CR_EXTENSION_ID_V1_SIZE sizeof(cr_extension_id)

#define CR_BACKEND_PROVIDER_DESC_PREFIX_SIZE \
    (offsetof(cr_backend_provider_desc, provider_id) + \
     sizeof(((cr_backend_provider_desc *)0)->provider_id))

#define CR_BACKEND_PROVIDER_DESC_V1_MIN_SIZE \
    (offsetof(cr_backend_provider_desc, destroy) + \
     sizeof(((cr_backend_provider_desc *)0)->destroy))

#define CR_BACKEND_EXTENSION_DESC_PREFIX_SIZE \
    (offsetof(cr_backend_extension_desc, extension_id) + \
     sizeof(((cr_backend_extension_desc *)0)->extension_id))

#define CR_STORAGE_LAYOUT_V1_MIN_SIZE \
    (offsetof(cr_storage_layout, alignment) + \
     sizeof(((cr_storage_layout *)0)->alignment))

#define CR_BACKEND_ERROR_V1_MIN_SIZE \
    (offsetof(cr_backend_error, native_code) + \
     sizeof(((cr_backend_error *)0)->native_code))

#define CR_BACKEND_PUMP_RESULT_V1_MIN_SIZE \
    (offsetof(cr_backend_pump_result, native_error_code) + \
     sizeof(((cr_backend_pump_result *)0)->native_error_code))

#define CR_BACKEND_ERROR_NONE                   UINT32_C(0)
#define CR_BACKEND_ERROR_INVALID_ARGUMENT       UINT32_C(1)
#define CR_BACKEND_ERROR_UNSUPPORTED_CAPABILITY UINT32_C(2)
#define CR_BACKEND_ERROR_WRONG_THREAD           UINT32_C(3)
#define CR_BACKEND_ERROR_OUT_OF_MEMORY          UINT32_C(4)
#define CR_BACKEND_ERROR_CLOSED                 UINT32_C(5)
#define CR_BACKEND_ERROR_INTERNAL               UINT32_C(6)

#define CR_NATIVE_ERROR_DOMAIN_NONE    UINT32_C(0)
#define CR_NATIVE_ERROR_DOMAIN_ERRNO   UINT32_C(1)
#define CR_NATIVE_ERROR_DOMAIN_WINSOCK UINT32_C(2)
#define CR_NATIVE_ERROR_DOMAIN_WIN32   UINT32_C(3)
#define CR_NATIVE_ERROR_DOMAIN_MEMORY  UINT32_C(4)

#define CR_BACKEND_PUMP_INVALID     UINT32_C(0)
#define CR_BACKEND_PUMP_PROGRESS    UINT32_C(1)
#define CR_BACKEND_PUMP_TIMEOUT     UINT32_C(2)
#define CR_BACKEND_PUMP_INTERRUPTED UINT32_C(3)
#define CR_BACKEND_PUMP_ERROR       UINT32_C(4)

static inline bool cr_extension_id_is_valid(cr_extension_id id) {
    return id.high != UINT64_C(0) || id.low != UINT64_C(0);
}

static inline bool cr_extension_id_equal(
    cr_extension_id left,
    cr_extension_id right
) {
    return left.high == right.high && left.low == right.low;
}

static inline bool cr_storage_layout_is_valid(
    const cr_storage_layout *layout
) {
    uint64_t alignment;

    if (layout == NULL ||
        layout->abi_version < CR_BACKEND_ABI_VERSION ||
        layout->struct_size < CR_STORAGE_LAYOUT_V1_MIN_SIZE ||
        layout->size == UINT64_C(0)) {
        return false;
    }
    alignment = layout->alignment;
    return alignment != UINT64_C(0) &&
        (alignment & (alignment - UINT64_C(1))) == UINT64_C(0);
}

static inline bool cr_backend_provider_desc_is_compatible(
    const cr_backend_provider_desc *provider
) {
    return provider != NULL &&
        provider->abi_version >= CR_BACKEND_ABI_VERSION &&
        provider->struct_size >= CR_BACKEND_PROVIDER_DESC_V1_MIN_SIZE &&
        cr_extension_id_is_valid(provider->provider_id) &&
        provider->create != NULL &&
        provider->query_extension != NULL &&
        provider->pump != NULL &&
        provider->interrupt != NULL &&
        provider->shutdown != NULL &&
        provider->destroy != NULL;
}

static inline bool cr_backend_extension_desc_is_compatible(
    const cr_backend_extension_desc *extension,
    cr_extension_id expected_id,
    uint32_t requested_abi_version
) {
    return extension != NULL &&
        requested_abi_version != UINT32_C(0) &&
        extension->abi_version >= requested_abi_version &&
        extension->struct_size >= CR_BACKEND_EXTENSION_DESC_PREFIX_SIZE &&
        cr_extension_id_is_valid(expected_id) &&
        cr_extension_id_is_valid(extension->extension_id) &&
        cr_extension_id_equal(extension->extension_id, expected_id);
}

/* Owner-thread operations. A capability miss returns NULL without failure. */
bool cr_backend_create(
    const cr_backend_provider_desc *provider,
    cr_backend **out_backend,
    cr_backend_error *out_error
);

const cr_backend_extension_desc *cr_backend_query_extension(
    cr_backend *backend,
    cr_extension_id extension_id,
    uint32_t requested_abi_version,
    cr_backend_error *out_error
);

bool cr_backend_pump(
    cr_backend *backend,
    uint64_t timeout_ns,
    uint32_t max_events,
    cr_backend_pump_result *out_result
);

bool cr_backend_shutdown(
    cr_backend *backend,
    cr_backend_error *out_error
);

bool cr_backend_destroy(
    cr_backend *backend,
    cr_backend_error *out_error
);

/* Thread-safe while the caller keeps backend alive. */
bool cr_backend_interrupt(
    cr_backend *backend,
    cr_backend_error *out_error
);

/* CR_STABLE_BACKEND_V1_END */

#endif
"#
}

/// Returns the stable portable C11 net-receive extension header.
#[must_use]
pub fn net_header() -> &'static str {
    r#"#ifndef CR_NET_H
#define CR_NET_H

#include "cr_backend.h"

/* Stable net-receive v1. Append fields only after each v1 minimum prefix. */
/* CR_STABLE_NET_V1_BEGIN */

typedef struct cr_net_receive_operation cr_net_receive_operation;
typedef struct cr_net_extension_desc cr_net_extension_desc;

typedef uint32_t cr_native_socket_kind;
typedef uint32_t cr_net_error_category;
typedef uint32_t cr_net_receive_terminal_kind;

typedef struct cr_native_socket_handle {
    cr_native_socket_kind kind;
    uint32_t reserved;
    uintptr_t value;
} cr_native_socket_handle;

typedef struct cr_net_error {
    uint32_t abi_version;
    uint32_t struct_size;
    cr_net_error_category category;
    cr_native_error_domain native_domain;
    int64_t native_code;
} cr_net_error;

typedef struct cr_net_receive_completion {
    uint32_t abi_version;
    uint32_t struct_size;
    cr_net_receive_terminal_kind terminal_kind;
    cr_net_error_category error_category;
    uint64_t bytes_transferred;
    cr_native_error_domain native_error_domain;
    uint32_t reserved;
    int64_t native_error_code;
} cr_net_receive_completion;

typedef void (*cr_net_receive_completion_fn)(
    void *callback_context,
    const cr_net_receive_completion *completion
);

typedef bool (*cr_net_receive_initialize_fn)(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    uint64_t operation_storage_size,
    cr_native_socket_handle socket,
    void *buffer,
    uint64_t buffer_size,
    cr_net_receive_completion_fn on_completion,
    void *callback_context,
    cr_net_error *out_error
);

typedef bool (*cr_net_receive_submit_fn)(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
);

typedef bool (*cr_net_receive_cancel_fn)(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
);

typedef bool (*cr_net_receive_quiesce_fn)(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
);

typedef bool (*cr_net_receive_destroy_fn)(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error *out_error
);

struct cr_net_extension_desc {
    cr_backend_extension_desc base;
    cr_storage_layout receive_operation_layout;
    cr_net_receive_initialize_fn receive_initialize;
    cr_net_receive_submit_fn receive_submit;
    cr_net_receive_cancel_fn receive_cancel;
    cr_net_receive_quiesce_fn receive_quiesce;
    cr_net_receive_destroy_fn receive_destroy;
};

#define CR_NET_ABI_VERSION 1u
#define CR_NET_EXPERIMENTAL_ABI_VERSION CR_NET_ABI_VERSION

#define CR_NET_RECEIVE_EXTENSION_ID_HIGH UINT64_C(0x43525f4e45545f52)
#define CR_NET_RECEIVE_EXTENSION_ID_LOW  UINT64_C(0x4543454956455f31)
#define CR_NET_RECEIVE_EXTENSION_ID_INIT \
    {CR_NET_RECEIVE_EXTENSION_ID_HIGH, CR_NET_RECEIVE_EXTENSION_ID_LOW}

#define CR_NATIVE_SOCKET_HANDLE_V1_SIZE \
    sizeof(cr_native_socket_handle)

#define CR_NET_EXTENSION_DESC_V1_MIN_SIZE \
    (offsetof(cr_net_extension_desc, receive_destroy) + \
     sizeof(((cr_net_extension_desc *)0)->receive_destroy))

#define CR_NET_ERROR_V1_MIN_SIZE \
    (offsetof(cr_net_error, native_code) + \
     sizeof(((cr_net_error *)0)->native_code))

#define CR_NET_RECEIVE_COMPLETION_V1_MIN_SIZE \
    (offsetof(cr_net_receive_completion, native_error_code) + \
     sizeof(((cr_net_receive_completion *)0)->native_error_code))

#define CR_NATIVE_SOCKET_INVALID  UINT32_C(0)
#define CR_NATIVE_SOCKET_WINSOCK  UINT32_C(1)
#define CR_NATIVE_SOCKET_POSIX_FD UINT32_C(2)
#define CR_NATIVE_SOCKET_MEMORY   UINT32_C(3)

#define CR_NET_ERROR_NONE                   UINT32_C(0)
#define CR_NET_ERROR_INVALID_ARGUMENT       UINT32_C(1)
#define CR_NET_ERROR_UNSUPPORTED_CAPABILITY UINT32_C(2)
#define CR_NET_ERROR_BUSY                   UINT32_C(3)
#define CR_NET_ERROR_OUT_OF_MEMORY          UINT32_C(4)
#define CR_NET_ERROR_CLOSED_BACKEND         UINT32_C(5)
#define CR_NET_ERROR_NETWORK_FAILURE        UINT32_C(6)
#define CR_NET_ERROR_INTERNAL_BACKEND       UINT32_C(7)
#define CR_NET_ERROR_WRONG_THREAD           UINT32_C(8)

#define CR_NET_RECEIVE_INVALID  UINT32_C(0)
#define CR_NET_RECEIVE_READY    UINT32_C(1)
#define CR_NET_RECEIVE_ERROR    UINT32_C(2)
#define CR_NET_RECEIVE_CANCELED UINT32_C(3)

#define CR_ERROR_NET_RECEIVE_INVALID_ARGUMENT  UINT32_C(1301)
#define CR_ERROR_NET_RECEIVE_UNSUPPORTED       UINT32_C(1302)
#define CR_ERROR_NET_RECEIVE_BUSY              UINT32_C(1303)
#define CR_ERROR_NET_RECEIVE_OUT_OF_MEMORY     UINT32_C(1304)
#define CR_ERROR_NET_RECEIVE_CLOSED            UINT32_C(1305)
#define CR_ERROR_NET_RECEIVE_NETWORK_FAILURE   UINT32_C(1306)
#define CR_ERROR_NET_RECEIVE_INTERNAL          UINT32_C(1307)
#define CR_ERROR_NET_RECEIVE_WRONG_THREAD      UINT32_C(1308)
#define CR_ERROR_NET_RECEIVE_INVALID_COMPLETION UINT32_C(1309)
#define CR_ERROR_NET_RECEIVE_WAKER_CLONE_FAILED UINT32_C(1310)

static inline bool cr_native_socket_handle_is_valid(
    cr_native_socket_handle socket
) {
    return socket.kind >= CR_NATIVE_SOCKET_WINSOCK &&
        socket.kind <= CR_NATIVE_SOCKET_MEMORY;
}

static inline bool cr_net_receive_completion_has_v1_prefix(
    const cr_net_receive_completion *completion
) {
    return completion != NULL &&
        completion->abi_version >= CR_NET_ABI_VERSION &&
        completion->struct_size >= CR_NET_RECEIVE_COMPLETION_V1_MIN_SIZE;
}

static inline bool cr_net_extension_desc_is_compatible(
    const cr_net_extension_desc *extension
) {
    const cr_extension_id expected = CR_NET_RECEIVE_EXTENSION_ID_INIT;

    return extension != NULL &&
        cr_backend_extension_desc_is_compatible(
            &extension->base,
            expected,
            CR_NET_ABI_VERSION
        ) &&
        extension->base.struct_size >= CR_NET_EXTENSION_DESC_V1_MIN_SIZE &&
        cr_storage_layout_is_valid(&extension->receive_operation_layout) &&
        extension->receive_initialize != NULL &&
        extension->receive_submit != NULL &&
        extension->receive_cancel != NULL &&
        extension->receive_quiesce != NULL &&
        extension->receive_destroy != NULL;
}

/* CR_STABLE_NET_V1_END */

/* Experimental reference awaitable adapter. Layout and API can change. */
typedef struct cr_net_receive_awaitable_state
    cr_net_receive_awaitable_state;
typedef struct cr_awaitable cr_awaitable;
typedef struct cr_error cr_error;

cr_storage_layout cr_net_receive_awaitable_state_layout(void);

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
);

bool cr_net_receive_awaitable_cancel(
    cr_net_receive_awaitable_state *state,
    cr_error *out_error
);

const cr_net_receive_completion *cr_net_receive_awaitable_completion(
    const cr_net_receive_awaitable_state *state
);

const cr_error *cr_net_receive_awaitable_error(
    const cr_net_receive_awaitable_state *state
);

#endif
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headers_publish_stable_v1_and_keep_runtime_objects_opaque() {
        let backend = backend_header();
        let net = net_header();
        assert!(backend.contains("CR_BACKEND_ABI_VERSION 1u"));
        assert!(backend.contains("CR_STABLE_BACKEND_V1_BEGIN"));
        assert!(backend.contains("CR_STABLE_BACKEND_V1_END"));
        assert!(backend.contains("typedef struct cr_extension_id"));
        assert!(backend.contains("CR_BACKEND_PROVIDER_DESC_PREFIX_SIZE"));
        assert!(backend.contains("CR_BACKEND_PUMP_RESULT_V1_MIN_SIZE"));
        assert!(backend.contains("bool cr_backend_interrupt("));
        assert!(net.contains("CR_NET_ABI_VERSION 1u"));
        assert!(net.contains("CR_STABLE_NET_V1_BEGIN"));
        assert!(net.contains("CR_STABLE_NET_V1_END"));
        assert!(net.contains("uintptr_t value"));
        assert!(net.contains("CR_NET_RECEIVE_COMPLETION_V1_MIN_SIZE"));
        assert!(net.contains("cr_net_receive_operation *operation"));
        assert!(net.contains("cr_net_receive_awaitable_initialize("));
        for forbidden in ["cr_task", "cr_executor", "reactor", "eventsource"] {
            assert!(!backend.to_ascii_lowercase().contains(forbidden));
            assert!(!net.to_ascii_lowercase().contains(forbidden));
        }
        assert_eq!(CR_BACKEND_ABI_VERSION, 1);
        assert_eq!(CR_NET_ABI_VERSION, 1);
        assert_eq!(CR_BACKEND_EXPERIMENTAL_ABI_VERSION, CR_BACKEND_ABI_VERSION);
        assert_eq!(CR_NET_EXPERIMENTAL_ABI_VERSION, CR_NET_ABI_VERSION);
    }

    #[test]
    fn candidate_identity_words_encode_distinct_nonzero_namespaces() {
        let backend = (CR_BACKEND_CORE_ID_HIGH, CR_BACKEND_CORE_ID_LOW);
        let net = (
            CR_NET_RECEIVE_EXTENSION_ID_HIGH,
            CR_NET_RECEIVE_EXTENSION_ID_LOW,
        );
        assert_ne!(backend, (0, 0));
        assert_ne!(net, (0, 0));
        assert_ne!(backend, net);
    }
}
