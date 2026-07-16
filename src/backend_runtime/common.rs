pub(crate) const INTERNAL_HEADER: &str = r#"#ifndef CR_BACKEND_INTERNAL_H
#define CR_BACKEND_INTERNAL_H

#include "cr_net.h"

#include <stdlib.h>

#ifndef CR_BACKEND_CALLOC
#define CR_BACKEND_CALLOC calloc
#endif

#ifndef CR_BACKEND_FREE
#define CR_BACKEND_FREE free
#endif

#ifndef CR_BACKEND_MEMORY_CALLOC
#define CR_BACKEND_MEMORY_CALLOC calloc
#endif

#ifndef CR_BACKEND_MEMORY_FREE
#define CR_BACKEND_MEMORY_FREE free
#endif

#ifndef CR_BACKEND_IOCP_CALLOC
#define CR_BACKEND_IOCP_CALLOC calloc
#endif

#ifndef CR_BACKEND_IOCP_FREE
#define CR_BACKEND_IOCP_FREE free
#endif

#ifndef CR_BACKEND_EPOLL_CALLOC
#define CR_BACKEND_EPOLL_CALLOC calloc
#endif

#ifndef CR_BACKEND_EPOLL_FREE
#define CR_BACKEND_EPOLL_FREE free
#endif

#ifndef CR_BACKEND_KQUEUE_CALLOC
#define CR_BACKEND_KQUEUE_CALLOC calloc
#endif

#ifndef CR_BACKEND_KQUEUE_FREE
#define CR_BACKEND_KQUEUE_FREE free
#endif

#ifndef CR_BACKEND_TRACKING_CALLOC
#define CR_BACKEND_TRACKING_CALLOC calloc
#endif

#ifndef CR_BACKEND_TRACKING_FREE
#define CR_BACKEND_TRACKING_FREE free
#endif

#ifndef CR_BACKEND_AWAITABLE_CALLOC
#define CR_BACKEND_AWAITABLE_CALLOC calloc
#endif

#ifndef CR_BACKEND_AWAITABLE_FREE
#define CR_BACKEND_AWAITABLE_FREE free
#endif

#ifndef CR_BACKEND_OPERATION_CALLOC
#define CR_BACKEND_OPERATION_CALLOC calloc
#endif

#ifndef CR_BACKEND_OPERATION_FREE
#define CR_BACKEND_OPERATION_FREE free
#endif

struct cr_backend {
    const void *owner_token;
    const cr_backend_provider_desc *provider;
    void *provider_state;
    bool closed;
};

const void *cr_backend_default_owner_token(void);

#ifndef CR_BACKEND_CURRENT_OWNER_TOKEN
#define CR_BACKEND_CURRENT_OWNER_TOKEN cr_backend_default_owner_token
#endif

const void *cr_backend_internal_current_owner_token(void);
bool cr_backend_internal_require_owner(
    const cr_backend *backend,
    cr_backend_error *out_error
);
bool cr_backend_internal_is_closed(const cr_backend *backend);
void *cr_backend_internal_provider_state(const cr_backend *backend);
const cr_backend_provider_desc *cr_backend_internal_provider(
    const cr_backend *backend
);
void cr_backend_internal_clear_error(cr_backend_error *out_error);
void cr_backend_internal_set_error(
    cr_backend_error *out_error,
    cr_backend_error_category category,
    cr_native_error_domain native_domain,
    int64_t native_code
);

typedef uint32_t cr_backend_memory_trace_event;

#define CR_BACKEND_MEMORY_TRACE_INITIALIZED        UINT32_C(1)
#define CR_BACKEND_MEMORY_TRACE_SUBMITTED          UINT32_C(2)
#define CR_BACKEND_MEMORY_TRACE_CANCEL_REQUESTED   UINT32_C(3)
#define CR_BACKEND_MEMORY_TRACE_TERMINAL_QUEUED    UINT32_C(4)
#define CR_BACKEND_MEMORY_TRACE_TERMINAL_CALLBACK  UINT32_C(5)
#define CR_BACKEND_MEMORY_TRACE_QUIESCENT          UINT32_C(6)
#define CR_BACKEND_MEMORY_TRACE_DESTROYED          UINT32_C(7)
#define CR_BACKEND_MEMORY_TRACE_INTERRUPT_CONSUMED UINT32_C(8)
#define CR_BACKEND_MEMORY_TRACE_SHUTDOWN           UINT32_C(9)

typedef void (*cr_backend_memory_trace_fn)(
    void *trace_context,
    cr_backend_memory_trace_event event,
    const cr_net_receive_operation *operation,
    uint64_t generation
);

extern const cr_backend_provider_desc cr_backend_memory_provider_desc;
extern const cr_backend_provider_desc cr_backend_iocp_provider_desc;
extern const cr_backend_provider_desc cr_backend_epoll_provider_desc;
extern const cr_backend_provider_desc cr_backend_kqueue_provider_desc;

bool cr_backend_memory_set_trace(
    cr_backend *backend,
    cr_backend_memory_trace_fn trace,
    void *trace_context,
    cr_backend_error *out_error
);

bool cr_backend_memory_complete_ready(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    const void *data,
    uint64_t data_size,
    cr_net_error *out_error
);

bool cr_backend_memory_complete_error(
    cr_backend *backend,
    cr_net_receive_operation *operation,
    cr_net_error_category category,
    cr_native_error_domain native_domain,
    int64_t native_code,
    cr_net_error *out_error
);

#endif
"#;

pub(crate) const COMMON_SOURCE: &str = r#"#include "cr_backend_internal.h"

#include <string.h>

static _Thread_local unsigned char cr_backend_owner_tls;

const void *cr_backend_default_owner_token(void) {
    return (const void *)&cr_backend_owner_tls;
}

const void *cr_backend_internal_current_owner_token(void) {
    return CR_BACKEND_CURRENT_OWNER_TOKEN();
}

void cr_backend_internal_clear_error(cr_backend_error *out_error) {
    if (out_error == NULL) return;
    *out_error = (cr_backend_error){
        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_backend_error),
        CR_BACKEND_ERROR_NONE,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    };
}

void cr_backend_internal_set_error(
    cr_backend_error *out_error,
    cr_backend_error_category category,
    cr_native_error_domain native_domain,
    int64_t native_code
) {
    if (out_error == NULL) return;
    *out_error = (cr_backend_error){
        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_backend_error),
        category,
        native_domain,
        native_code
    };
}

bool cr_backend_internal_require_owner(
    const cr_backend *backend,
    cr_backend_error *out_error
) {
    if (backend == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (backend->owner_token != cr_backend_internal_current_owner_token()) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_WRONG_THREAD,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    cr_backend_internal_clear_error(out_error);
    return true;
}

bool cr_backend_internal_is_closed(const cr_backend *backend) {
    return backend == NULL || backend->closed;
}

void *cr_backend_internal_provider_state(const cr_backend *backend) {
    return backend != NULL ? backend->provider_state : NULL;
}

const cr_backend_provider_desc *cr_backend_internal_provider(
    const cr_backend *backend
) {
    return backend != NULL ? backend->provider : NULL;
}

static void cr_backend_set_pump_error(
    cr_backend_pump_result *out_result,
    cr_backend_error_category category
) {
    if (out_result == NULL) return;
    *out_result = (cr_backend_pump_result){
        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_backend_pump_result),
        CR_BACKEND_PUMP_ERROR,
        UINT32_C(0),
        category,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    };
}

bool cr_backend_create(
    const cr_backend_provider_desc *provider,
    cr_backend **out_backend,
    cr_backend_error *out_error
) {
    const cr_extension_id expected = CR_BACKEND_CORE_ID_INIT;
    cr_backend *backend;
    void *provider_state = NULL;

    cr_backend_internal_clear_error(out_error);
    if (out_backend == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    *out_backend = NULL;
    if (!cr_backend_provider_desc_is_compatible(provider) ||
        !cr_extension_id_equal(provider->provider_id, expected)) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_UNSUPPORTED_CAPABILITY,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    backend = (cr_backend *)CR_BACKEND_CALLOC(1u, sizeof(*backend));
    if (backend == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_OUT_OF_MEMORY,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (!provider->create(provider, &provider_state, out_error) ||
        provider_state == NULL) {
        if (provider_state != NULL) provider->destroy(provider_state);
        CR_BACKEND_FREE(backend);
        if (out_error == NULL || out_error->category == CR_BACKEND_ERROR_NONE) {
            cr_backend_internal_set_error(
                out_error,
                CR_BACKEND_ERROR_INTERNAL,
                CR_NATIVE_ERROR_DOMAIN_NONE,
                INT64_C(0)
            );
        }
        return false;
    }
    backend->owner_token = cr_backend_internal_current_owner_token();
    backend->provider = provider;
    backend->provider_state = provider_state;
    backend->closed = false;
    *out_backend = backend;
    return true;
}

const cr_backend_extension_desc *cr_backend_query_extension(
    cr_backend *backend,
    cr_extension_id extension_id,
    uint32_t requested_abi_version,
    cr_backend_error *out_error
) {
    const cr_backend_extension_desc *extension;

    cr_backend_internal_clear_error(out_error);
    if (!cr_backend_internal_require_owner(backend, out_error)) return NULL;
    if (backend->closed) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_CLOSED,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return NULL;
    }
    if (!cr_extension_id_is_valid(extension_id) ||
        requested_abi_version == UINT32_C(0)) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return NULL;
    }
    extension = backend->provider->query_extension(
        backend->provider_state,
        extension_id,
        requested_abi_version,
        out_error
    );
    if (extension == NULL) return NULL;
    if (!cr_backend_extension_desc_is_compatible(
            extension,
            extension_id,
            requested_abi_version
        )) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INTERNAL,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return NULL;
    }
    cr_backend_internal_clear_error(out_error);
    return extension;
}

static bool cr_backend_pump_result_is_valid(
    const cr_backend_pump_result *result,
    uint32_t max_events,
    bool provider_success
) {
    if (result == NULL ||
        result->abi_version < CR_BACKEND_EXPERIMENTAL_ABI_VERSION ||
        result->struct_size < CR_BACKEND_PUMP_RESULT_V1_MIN_SIZE ||
        result->events_dispatched > max_events) {
        return false;
    }
    switch (result->reason) {
        case CR_BACKEND_PUMP_PROGRESS:
            return provider_success &&
                result->events_dispatched != UINT32_C(0) &&
                result->error_category == CR_BACKEND_ERROR_NONE;
        case CR_BACKEND_PUMP_TIMEOUT:
            return provider_success &&
                result->events_dispatched == UINT32_C(0) &&
                result->error_category == CR_BACKEND_ERROR_NONE;
        case CR_BACKEND_PUMP_INTERRUPTED:
            return provider_success &&
                result->events_dispatched != UINT32_C(0) &&
                result->error_category == CR_BACKEND_ERROR_NONE;
        case CR_BACKEND_PUMP_ERROR:
            return !provider_success &&
                result->error_category != CR_BACKEND_ERROR_NONE;
        default:
            return false;
    }
}

bool cr_backend_pump(
    cr_backend *backend,
    uint64_t timeout_ns,
    uint32_t max_events,
    cr_backend_pump_result *out_result
) {
    cr_backend_error owner_error;
    bool provider_success;

    if (out_result == NULL) return false;
    *out_result = (cr_backend_pump_result){
        CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
        sizeof(cr_backend_pump_result),
        CR_BACKEND_PUMP_TIMEOUT,
        UINT32_C(0),
        CR_BACKEND_ERROR_NONE,
        CR_NATIVE_ERROR_DOMAIN_NONE,
        INT64_C(0)
    };
    if (!cr_backend_internal_require_owner(backend, &owner_error)) {
        cr_backend_set_pump_error(out_result, owner_error.category);
        return false;
    }
    if (backend->closed) {
        cr_backend_set_pump_error(out_result, CR_BACKEND_ERROR_CLOSED);
        return false;
    }
    if (max_events == UINT32_C(0)) {
        cr_backend_set_pump_error(
            out_result,
            CR_BACKEND_ERROR_INVALID_ARGUMENT
        );
        return false;
    }
    provider_success = backend->provider->pump(
        backend->provider_state,
        timeout_ns,
        max_events,
        out_result
    );
    if (!cr_backend_pump_result_is_valid(
            out_result,
            max_events,
            provider_success
        )) {
        cr_backend_set_pump_error(out_result, CR_BACKEND_ERROR_INTERNAL);
        return false;
    }
    return provider_success;
}

bool cr_backend_interrupt(
    cr_backend *backend,
    cr_backend_error *out_error
) {
    cr_backend_internal_clear_error(out_error);
    if (backend == NULL) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_INVALID_ARGUMENT,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    if (backend->closed) {
        cr_backend_internal_set_error(
            out_error,
            CR_BACKEND_ERROR_CLOSED,
            CR_NATIVE_ERROR_DOMAIN_NONE,
            INT64_C(0)
        );
        return false;
    }
    return backend->provider->interrupt(backend->provider_state, out_error);
}

bool cr_backend_shutdown(
    cr_backend *backend,
    cr_backend_error *out_error
) {
    cr_backend_internal_clear_error(out_error);
    if (!cr_backend_internal_require_owner(backend, out_error)) return false;
    if (backend->closed) return true;
    if (!backend->provider->shutdown(backend->provider_state, out_error)) {
        return false;
    }
    backend->closed = true;
    return true;
}

bool cr_backend_destroy(
    cr_backend *backend,
    cr_backend_error *out_error
) {
    const cr_backend_provider_desc *provider;
    void *provider_state;

    cr_backend_internal_clear_error(out_error);
    if (!cr_backend_internal_require_owner(backend, out_error)) return false;
    if (!cr_backend_shutdown(backend, out_error)) return false;
    provider = backend->provider;
    provider_state = backend->provider_state;
    backend->provider = NULL;
    backend->provider_state = NULL;
    provider->destroy(provider_state);
    CR_BACKEND_FREE(backend);
    cr_backend_internal_clear_error(out_error);
    return true;
}
"#;
