#include "cr_net.h"

typedef struct future_provider_desc {
    cr_backend_provider_desc v1;
    uint64_t appended_field;
} future_provider_desc;

typedef struct future_net_desc {
    cr_net_extension_desc v1;
    uint64_t appended_field;
} future_net_desc;

_Static_assert(
    CR_BACKEND_EXPERIMENTAL_ABI_VERSION == 1u,
    "backend experimental ABI version"
);
_Static_assert(
    CR_NET_EXPERIMENTAL_ABI_VERSION == 1u,
    "net experimental ABI version"
);
_Static_assert(sizeof(cr_extension_id) == 16u, "128-bit extension identity");
_Static_assert(
    offsetof(cr_backend_provider_desc, capability_bits) == 8u,
    "provider capability offset"
);
_Static_assert(
    offsetof(cr_backend_provider_desc, provider_id) == 16u,
    "provider identity offset"
);
_Static_assert(
    CR_BACKEND_PROVIDER_DESC_PREFIX_SIZE == 32u,
    "provider descriptor fixed prefix"
);
_Static_assert(
    CR_BACKEND_EXTENSION_DESC_PREFIX_SIZE == 32u,
    "extension descriptor fixed prefix"
);
_Static_assert(
    CR_STORAGE_LAYOUT_V1_MIN_SIZE == sizeof(cr_storage_layout),
    "storage layout v1 prefix"
);
_Static_assert(sizeof(cr_storage_layout) == 24u, "storage layout size");
_Static_assert(
    CR_BACKEND_ERROR_V1_MIN_SIZE == sizeof(cr_backend_error),
    "backend error v1 prefix"
);
_Static_assert(sizeof(cr_backend_error) == 24u, "backend error size");
_Static_assert(
    CR_BACKEND_PUMP_RESULT_V1_MIN_SIZE == sizeof(cr_backend_pump_result),
    "pump result v1 prefix"
);
_Static_assert(sizeof(cr_backend_pump_result) == 32u, "pump result size");
_Static_assert(
    offsetof(cr_native_socket_handle, value) == 8u,
    "native socket value offset"
);
_Static_assert(
    sizeof(((cr_native_socket_handle *)0)->value) == sizeof(uintptr_t),
    "native socket uses uintptr_t storage"
);
_Static_assert(
    CR_NET_ERROR_V1_MIN_SIZE == sizeof(cr_net_error),
    "net error v1 prefix"
);
_Static_assert(sizeof(cr_net_error) == 24u, "net error size");
_Static_assert(
    CR_NET_RECEIVE_COMPLETION_V1_MIN_SIZE ==
        sizeof(cr_net_receive_completion),
    "receive completion v1 prefix"
);
_Static_assert(
    sizeof(cr_net_receive_completion) == 40u,
    "receive completion size"
);
_Static_assert(
    offsetof(cr_net_extension_desc, base) == 0u,
    "net descriptor starts with common extension prefix"
);
_Static_assert(
    CR_NET_EXTENSION_DESC_V1_MIN_SIZE <= sizeof(cr_net_extension_desc),
    "net descriptor v1 prefix fits the complete structure"
);
_Static_assert(
    offsetof(future_provider_desc, appended_field) ==
        sizeof(cr_backend_provider_desc),
    "future provider descriptor appends after v1"
);
_Static_assert(
    offsetof(future_net_desc, appended_field) == sizeof(cr_net_extension_desc),
    "future net descriptor appends after v1"
);
_Static_assert(
    CR_BACKEND_CORE_ID_HIGH != 0u || CR_BACKEND_CORE_ID_LOW != 0u,
    "backend identity is nonzero"
);
_Static_assert(
    CR_NET_RECEIVE_EXTENSION_ID_HIGH != 0u ||
        CR_NET_RECEIVE_EXTENSION_ID_LOW != 0u,
    "net identity is nonzero"
);

int main(void) {
    cr_native_socket_handle socket = {
        CR_NATIVE_SOCKET_MEMORY,
        0u,
        (uintptr_t)7u
    };
    return cr_native_socket_handle_is_valid(socket) ? 0 : 1;
}
