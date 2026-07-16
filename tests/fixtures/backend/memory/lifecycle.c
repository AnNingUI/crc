#include "cr_backend_internal.h"
#if defined(CR_BACKEND_DIFFERENTIAL)
#include "transcript.h"
#else
#define cr_test_diff_emit(...) ((void)0)
#endif

#include <assert.h>
#include <stddef.h>
#include <string.h>

typedef union operation_storage {
    max_align_t alignment;
    unsigned char bytes[512];
} operation_storage;

typedef struct observation {
    uint32_t calls;
    cr_net_receive_completion last;
} observation;

typedef struct trace_log {
    uint32_t counts[10];
    uint64_t max_generation;
} trace_log;

static cr_net_receive_operation *operation_from(operation_storage *storage) {
    return (cr_net_receive_operation *)(void *)storage->bytes;
}

static void observe_completion(
    void *context,
    const cr_net_receive_completion *completion
) {
    observation *result = (observation *)context;
    assert(cr_net_receive_completion_has_v1_prefix(completion));
    result->calls++;
    result->last = *completion;
}

static void observe_trace(
    void *context,
    cr_backend_memory_trace_event event,
    const cr_net_receive_operation *operation,
    uint64_t generation
) {
    trace_log *log = (trace_log *)context;
    (void)operation;
    assert(event >= CR_BACKEND_MEMORY_TRACE_INITIALIZED);
    assert(event <= CR_BACKEND_MEMORY_TRACE_SHUTDOWN);
    log->counts[event]++;
    if (generation > log->max_generation) log->max_generation = generation;
}

static cr_native_socket_handle memory_socket(uintptr_t value) {
    return (cr_native_socket_handle){
        CR_NATIVE_SOCKET_MEMORY,
        UINT32_C(0),
        value
    };
}

int main(void) {
    const cr_extension_id net_id = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    const cr_extension_id capability_miss = {
        UINT64_C(0x1111),
        UINT64_C(0x2222)
    };
    cr_backend_error backend_error;
    cr_net_error net_error;
    cr_backend_pump_result pump;
    cr_backend *backend = NULL;
    const cr_backend_extension_desc *base;
    const cr_net_extension_desc *net;
    operation_storage first_storage = {0};
    operation_storage second_storage = {0};
    operation_storage third_storage = {0};
    operation_storage shutdown_storage = {0};
    cr_net_receive_operation *first = operation_from(&first_storage);
    cr_net_receive_operation *second = operation_from(&second_storage);
    cr_net_receive_operation *third = operation_from(&third_storage);
    cr_net_receive_operation *shutdown_operation =
        operation_from(&shutdown_storage);
    observation first_result = {0};
    observation second_result = {0};
    observation third_result = {0};
    observation shutdown_result = {0};
    trace_log trace = {0};
    unsigned char first_buffer[16] = {0};
    unsigned char second_buffer[16] = {0};
    unsigned char third_buffer[16] = {0};
    unsigned char shutdown_buffer[16] = {0};
    const char ready_data[] = "hello";

    assert(cr_backend_create(
        &cr_backend_memory_provider_desc,
        &backend,
        &backend_error
    ));
    assert(backend != NULL);
    assert(backend_error.category == CR_BACKEND_ERROR_NONE);

    assert(cr_backend_query_extension(
        backend,
        capability_miss,
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        &backend_error
    ) == NULL);
    assert(backend_error.category == CR_BACKEND_ERROR_NONE);
    assert(cr_backend_query_extension(
        backend,
        net_id,
        CR_NET_EXPERIMENTAL_ABI_VERSION + 1u,
        &backend_error
    ) == NULL);
    assert(backend_error.category == CR_BACKEND_ERROR_NONE);

    base = cr_backend_query_extension(
        backend,
        net_id,
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        &backend_error
    );
    assert(base != NULL);
    net = (const cr_net_extension_desc *)(const void *)base;
    assert(cr_net_extension_desc_is_compatible(net));
    assert(net->receive_operation_layout.size <= sizeof(operation_storage));
    assert(
        net->receive_operation_layout.alignment <=
        _Alignof(operation_storage)
    );
    assert(cr_backend_memory_set_trace(
        backend,
        observe_trace,
        &trace,
        &backend_error
    ));

    assert(!net->receive_initialize(
        backend,
        first,
        sizeof(first_storage),
        memory_socket((uintptr_t)1u),
        first_buffer,
        UINT64_C(0),
        observe_completion,
        &first_result,
        &net_error
    ));
    assert(net_error.category == CR_NET_ERROR_INVALID_ARGUMENT);
    assert(first_result.calls == UINT32_C(0));

    assert(net->receive_initialize(
        backend,
        first,
        sizeof(first_storage),
        memory_socket((uintptr_t)1u),
        first_buffer,
        sizeof(first_buffer),
        observe_completion,
        &first_result,
        &net_error
    ));
    assert(net->receive_submit(backend, first, &net_error));

    assert(net->receive_initialize(
        backend,
        second,
        sizeof(second_storage),
        memory_socket((uintptr_t)1u),
        second_buffer,
        sizeof(second_buffer),
        observe_completion,
        &second_result,
        &net_error
    ));
    assert(!net->receive_submit(backend, second, &net_error));
    assert(net_error.category == CR_NET_ERROR_BUSY);
    assert(second_result.calls == UINT32_C(0));

    assert(cr_backend_memory_complete_ready(
        backend,
        first,
        ready_data,
        sizeof(ready_data) - 1u,
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_PROGRESS);
    assert(pump.events_dispatched == UINT32_C(1));
    assert(first_result.calls == UINT32_C(1));
    assert(first_result.last.terminal_kind == CR_NET_RECEIVE_READY);
    assert(first_result.last.bytes_transferred == sizeof(ready_data) - 1u);
    assert(memcmp(first_buffer, ready_data, sizeof(ready_data) - 1u) == 0);
    assert(net->receive_quiesce(backend, first, &net_error));
    cr_test_diff_emit(
        "success",
        first_result.last.terminal_kind,
        first_result.last.bytes_transferred,
        first_result.last.error_category,
        UINT32_C(1),
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );

    assert(net->receive_initialize(
        backend,
        first,
        sizeof(first_storage),
        memory_socket((uintptr_t)1u),
        first_buffer,
        sizeof(first_buffer),
        observe_completion,
        &first_result,
        &net_error
    ));
    assert(net->receive_submit(backend, first, &net_error));
    assert(net->receive_cancel(backend, first, &net_error));
    assert(net->receive_cancel(backend, first, &net_error));
    assert(cr_backend_pump(backend, UINT64_C(10), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_PROGRESS);
    assert(first_result.calls == UINT32_C(2));
    assert(first_result.last.terminal_kind == CR_NET_RECEIVE_CANCELED);
    assert(net->receive_quiesce(backend, first, &net_error));
    assert(net->receive_destroy(backend, first, &net_error));
    cr_test_diff_emit(
        "cancel",
        first_result.last.terminal_kind,
        first_result.last.bytes_transferred,
        first_result.last.error_category,
        UINT32_C(1),
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );

    assert(net->receive_initialize(
        backend,
        second,
        sizeof(second_storage),
        memory_socket((uintptr_t)2u),
        second_buffer,
        sizeof(second_buffer),
        observe_completion,
        &second_result,
        &net_error
    ));
    assert(net->receive_submit(backend, second, &net_error));
    assert(cr_backend_memory_complete_ready(
        backend,
        second,
        "x",
        UINT64_C(1),
        &net_error
    ));
    assert(cr_backend_interrupt(backend, &backend_error));
    assert(cr_backend_interrupt(backend, &backend_error));
    assert(cr_backend_pump(backend, UINT64_MAX, UINT32_C(2), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_PROGRESS);
    assert(pump.events_dispatched == UINT32_C(2));
    assert(second_result.calls == UINT32_C(1));
    assert(net->receive_quiesce(backend, second, &net_error));
    assert(net->receive_destroy(backend, second, &net_error));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);
    cr_test_diff_emit(
        "timeout",
        CR_NET_RECEIVE_INVALID,
        UINT64_C(0),
        CR_NET_ERROR_NONE,
        UINT32_C(0),
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        pump.reason,
        pump.events_dispatched
    );

    assert(cr_backend_interrupt(backend, &backend_error));
    assert(cr_backend_interrupt(backend, &backend_error));
    assert(cr_backend_pump(backend, UINT64_MAX, UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_INTERRUPTED);
    assert(pump.events_dispatched == UINT32_C(1));
    cr_test_diff_emit(
        "interrupt",
        CR_NET_RECEIVE_INVALID,
        UINT64_C(0),
        CR_NET_ERROR_NONE,
        UINT32_C(0),
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        pump.reason,
        pump.events_dispatched
    );
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);

    assert(net->receive_initialize(
        backend,
        third,
        sizeof(third_storage),
        memory_socket((uintptr_t)3u),
        third_buffer,
        sizeof(third_buffer),
        observe_completion,
        &third_result,
        &net_error
    ));
    assert(net->receive_submit(backend, third, &net_error));
    assert(cr_backend_memory_complete_error(
        backend,
        third,
        CR_NET_ERROR_NETWORK_FAILURE,
        CR_NATIVE_ERROR_DOMAIN_ERRNO,
        INT64_C(55),
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_PROGRESS);
    assert(third_result.calls == UINT32_C(1));
    assert(third_result.last.terminal_kind == CR_NET_RECEIVE_ERROR);
    assert(third_result.last.error_category == CR_NET_ERROR_NETWORK_FAILURE);
    assert(third_result.last.native_error_domain == CR_NATIVE_ERROR_DOMAIN_ERRNO);
    assert(third_result.last.native_error_code == INT64_C(55));
    assert(net->receive_quiesce(backend, third, &net_error));
    cr_test_diff_emit(
        "error",
        third_result.last.terminal_kind,
        third_result.last.bytes_transferred,
        third_result.last.error_category,
        third_result.calls,
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );

    memset(&third_result, 0, sizeof(third_result));
    assert(net->receive_initialize(
        backend,
        third,
        sizeof(third_storage),
        memory_socket((uintptr_t)3u),
        third_buffer,
        sizeof(third_buffer),
        observe_completion,
        &third_result,
        &net_error
    ));
    assert(net->receive_submit(backend, third, &net_error));
    assert(cr_backend_memory_complete_ready(
        backend,
        third,
        NULL,
        UINT64_C(0),
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(third_result.calls == UINT32_C(1));
    assert(third_result.last.terminal_kind == CR_NET_RECEIVE_READY);
    assert(third_result.last.bytes_transferred == UINT64_C(0));
    assert(net->receive_quiesce(backend, third, &net_error));
    assert(net->receive_destroy(backend, third, &net_error));
    cr_test_diff_emit(
        "eof",
        third_result.last.terminal_kind,
        third_result.last.bytes_transferred,
        third_result.last.error_category,
        third_result.calls,
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );

    assert(!cr_backend_pump(backend, UINT64_C(0), UINT32_C(0), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_ERROR);
    assert(pump.error_category == CR_BACKEND_ERROR_INVALID_ARGUMENT);

    assert(net->receive_initialize(
        backend,
        shutdown_operation,
        sizeof(shutdown_storage),
        memory_socket((uintptr_t)4u),
        shutdown_buffer,
        sizeof(shutdown_buffer),
        observe_completion,
        &shutdown_result,
        &net_error
    ));
    assert(net->receive_submit(backend, shutdown_operation, &net_error));
    assert(cr_backend_shutdown(backend, &backend_error));
    assert(cr_backend_shutdown(backend, &backend_error));
    assert(shutdown_result.calls == UINT32_C(1));
    assert(shutdown_result.last.terminal_kind == CR_NET_RECEIVE_CANCELED);
    assert(net->receive_destroy(backend, shutdown_operation, &net_error));
    cr_test_diff_emit(
        "shutdown",
        shutdown_result.last.terminal_kind,
        shutdown_result.last.bytes_transferred,
        shutdown_result.last.error_category,
        shutdown_result.calls,
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(0),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );

    assert(trace.counts[CR_BACKEND_MEMORY_TRACE_INITIALIZED] == UINT32_C(7));
    assert(trace.counts[CR_BACKEND_MEMORY_TRACE_SUBMITTED] == UINT32_C(6));
    assert(
        trace.counts[CR_BACKEND_MEMORY_TRACE_CANCEL_REQUESTED] ==
        UINT32_C(2)
    );
    assert(
        trace.counts[CR_BACKEND_MEMORY_TRACE_TERMINAL_QUEUED] == UINT32_C(6)
    );
    assert(
        trace.counts[CR_BACKEND_MEMORY_TRACE_TERMINAL_CALLBACK] ==
        UINT32_C(6)
    );
    assert(trace.counts[CR_BACKEND_MEMORY_TRACE_QUIESCENT] == UINT32_C(7));
    assert(trace.counts[CR_BACKEND_MEMORY_TRACE_DESTROYED] == UINT32_C(4));
    assert(
        trace.counts[CR_BACKEND_MEMORY_TRACE_INTERRUPT_CONSUMED] ==
        UINT32_C(2)
    );
    assert(trace.counts[CR_BACKEND_MEMORY_TRACE_SHUTDOWN] == UINT32_C(1));
    assert(trace.max_generation == UINT64_C(2));

    assert(cr_backend_destroy(backend, &backend_error));
    return 0;
}
