#include "windows_helpers.h"

#include "cr_backend_internal.h"
#if defined(CR_BACKEND_DIFFERENTIAL)
#include "transcript.h"
#else
#define cr_test_diff_emit(...) ((void)0)
#endif

#include <stddef.h>
#include <stdlib.h>

static unsigned int iocp_allocations;
static unsigned int iocp_frees;
static unsigned int iocp_handles_opened;
static unsigned int iocp_handles_closed;
static int last_submit_completed_inline = -1;

void *cr_test_iocp_calloc(size_t count, size_t size) {
    void *pointer = calloc(count, size);
    if (pointer != NULL) iocp_allocations++;
    return pointer;
}

void cr_test_iocp_free(void *pointer) {
    if (pointer != NULL) iocp_frees++;
    free(pointer);
}

void cr_test_iocp_handle_opened(void *handle) {
    assert(handle != NULL);
    iocp_handles_opened++;
}

void cr_test_iocp_handle_closed(void *handle) {
    assert(handle != NULL);
    iocp_handles_closed++;
}

void cr_test_iocp_submit_observed(
    const void *operation,
    int completed_inline
) {
    assert(operation != NULL);
    last_submit_completed_inline = completed_inline;
}

typedef union operation_storage {
    max_align_t alignment;
    unsigned char bytes[1024];
} operation_storage;

typedef struct completion_state {
    unsigned int calls;
    cr_net_receive_completion completion;
} completion_state;

typedef struct backend_fixture {
    cr_backend *backend;
    const cr_net_extension_desc *net;
} backend_fixture;

static void on_completion(
    void *callback_context,
    const cr_net_receive_completion *completion
) {
    completion_state *state = (completion_state *)callback_context;

    assert(state != NULL);
    assert(cr_net_receive_completion_has_v1_prefix(completion));
    assert(state->calls == 0u);
    state->calls++;
    state->completion = *completion;
}

static backend_fixture make_backend(void) {
    const cr_extension_id net_id = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    const cr_backend_extension_desc *extension;
    cr_backend_error error;
    backend_fixture fixture;

    fixture.backend = NULL;
    fixture.net = NULL;
    assert(cr_backend_create(
        &cr_backend_iocp_provider_desc,
        &fixture.backend,
        &error
    ));
    assert(error.category == CR_BACKEND_ERROR_NONE);
    extension = cr_backend_query_extension(
        fixture.backend,
        net_id,
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        &error
    );
    assert(extension != NULL);
    fixture.net = (const cr_net_extension_desc *)extension;
    assert(cr_net_extension_desc_is_compatible(fixture.net));
    assert(fixture.net->receive_operation_layout.size <=
        sizeof(operation_storage));
    assert(fixture.net->receive_operation_layout.alignment <=
        _Alignof(operation_storage));
    return fixture;
}

static void destroy_backend(backend_fixture *fixture) {
    cr_backend_error error;

    assert(cr_backend_destroy(fixture->backend, &error));
    assert(error.category == CR_BACKEND_ERROR_NONE);
    fixture->backend = NULL;
    fixture->net = NULL;
}

static cr_native_socket_handle socket_handle(SOCKET socket) {
    return (cr_native_socket_handle){
        CR_NATIVE_SOCKET_WINSOCK,
        UINT32_C(0),
        (uintptr_t)socket
    };
}

static cr_net_receive_operation *initialize_receive(
    const backend_fixture *fixture,
    operation_storage *storage,
    SOCKET socket,
    void *buffer,
    size_t buffer_size,
    completion_state *completion
) {
    cr_net_receive_operation *operation =
        (cr_net_receive_operation *)(void *)storage->bytes;
    cr_net_error error;

    assert(fixture->net->receive_initialize(
        fixture->backend,
        operation,
        sizeof(*storage),
        socket_handle(socket),
        buffer,
        (uint64_t)buffer_size,
        on_completion,
        completion,
        &error
    ));
    assert(error.category == CR_NET_ERROR_NONE);
    return operation;
}

static void quiesce_and_destroy(
    const backend_fixture *fixture,
    cr_net_receive_operation *operation
) {
    cr_net_error error;

    assert(fixture->net->receive_quiesce(
        fixture->backend,
        operation,
        &error
    ));
    assert(error.category == CR_NET_ERROR_NONE);
    assert(fixture->net->receive_destroy(
        fixture->backend,
        operation,
        &error
    ));
    assert(error.category == CR_NET_ERROR_NONE);
}

static void pump_one(
    const backend_fixture *fixture,
    cr_backend_pump_reason expected_reason
) {
    cr_backend_pump_result pump;

    assert(cr_backend_pump(
        fixture->backend,
        UINT64_MAX,
        UINT32_C(1),
        &pump
    ));
    assert(pump.reason == expected_reason);
    assert(pump.events_dispatched == UINT32_C(1));
}

static void test_immediate_completion_wins_late_cancel(void) {
    static const char payload[] = "ready";
    cr_test_socket_pair pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state completion = {0};
    unsigned char buffer[16] = {0};
    cr_net_receive_operation *operation;
    cr_net_error error;

    cr_test_send_exact(pair.sender, payload, (int)(sizeof(payload) - 1u));
    operation = initialize_receive(
        &fixture,
        &storage,
        pair.receiver,
        buffer,
        sizeof(buffer),
        &completion
    );
    last_submit_completed_inline = -1;
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation,
        &error
    ));
    assert(last_submit_completed_inline == 1);
    assert(fixture.net->receive_cancel(
        fixture.backend,
        operation,
        &error
    ));
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(completion.calls == 1u);
    assert(completion.completion.terminal_kind == CR_NET_RECEIVE_READY);
    assert(completion.completion.bytes_transferred == sizeof(payload) - 1u);
    assert(memcmp(buffer, payload, sizeof(payload) - 1u) == 0);
    quiesce_and_destroy(&fixture, operation);
    cr_test_diff_emit(
        "success",
        completion.completion.terminal_kind,
        completion.completion.bytes_transferred,
        completion.completion.error_category,
        completion.calls,
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&pair);
}

typedef struct send_thread_context {
    SOCKET socket;
    HANDLE start;
    const char *data;
    int data_size;
} send_thread_context;

static DWORD WINAPI send_thread(void *raw_context) {
    send_thread_context *context = (send_thread_context *)raw_context;

    assert(WaitForSingleObject(context->start, INFINITE) == WAIT_OBJECT_0);
    cr_test_send_exact(context->socket, context->data, context->data_size);
    return 0;
}

static void test_deferred_completion(void) {
    static const char payload[] = "deferred";
    cr_test_socket_pair pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state completion = {0};
    unsigned char buffer[32] = {0};
    cr_net_receive_operation *operation;
    cr_net_error error;
    send_thread_context context;
    HANDLE thread;

    operation = initialize_receive(
        &fixture,
        &storage,
        pair.receiver,
        buffer,
        sizeof(buffer),
        &completion
    );
    last_submit_completed_inline = -1;
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation,
        &error
    ));
    assert(last_submit_completed_inline == 0);
    context.socket = pair.sender;
    context.start = CreateEventW(NULL, TRUE, FALSE, NULL);
    context.data = payload;
    context.data_size = (int)(sizeof(payload) - 1u);
    assert(context.start != NULL);
    thread = CreateThread(NULL, 0, send_thread, &context, 0, NULL);
    assert(thread != NULL);
    assert(SetEvent(context.start));
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(WaitForSingleObject(thread, INFINITE) == WAIT_OBJECT_0);
    assert(CloseHandle(thread));
    assert(CloseHandle(context.start));
    assert(completion.calls == 1u);
    assert(completion.completion.terminal_kind == CR_NET_RECEIVE_READY);
    assert(completion.completion.bytes_transferred == sizeof(payload) - 1u);
    assert(memcmp(buffer, payload, sizeof(payload) - 1u) == 0);
    quiesce_and_destroy(&fixture, operation);
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&pair);
}

static void test_cancel_before_completion(void) {
    cr_test_socket_pair pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state completion = {0};
    unsigned char buffer[8] = {0};
    cr_net_receive_operation *operation;
    cr_net_error error;

    operation = initialize_receive(
        &fixture,
        &storage,
        pair.receiver,
        buffer,
        sizeof(buffer),
        &completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation,
        &error
    ));
    assert(fixture.net->receive_cancel(
        fixture.backend,
        operation,
        &error
    ));
    assert(fixture.net->receive_cancel(
        fixture.backend,
        operation,
        &error
    ));
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(completion.calls == 1u);
    assert(completion.completion.terminal_kind == CR_NET_RECEIVE_CANCELED);
    quiesce_and_destroy(&fixture, operation);
    cr_test_diff_emit(
        "cancel",
        completion.completion.terminal_kind,
        completion.completion.bytes_transferred,
        completion.completion.error_category,
        completion.calls,
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&pair);
}

static void test_quiesce_dispatches_unrelated_completion(void) {
    static const char payload[] = "other";
    cr_test_socket_pair first = cr_test_make_socket_pair();
    cr_test_socket_pair second = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage first_storage = {0};
    operation_storage second_storage = {0};
    completion_state first_completion = {0};
    completion_state second_completion = {0};
    unsigned char first_buffer[8] = {0};
    unsigned char second_buffer[8] = {0};
    cr_net_receive_operation *first_operation;
    cr_net_receive_operation *second_operation;
    cr_net_error error;

    cr_test_send_exact(
        second.sender,
        payload,
        (int)(sizeof(payload) - 1u)
    );
    second_operation = initialize_receive(
        &fixture,
        &second_storage,
        second.receiver,
        second_buffer,
        sizeof(second_buffer),
        &second_completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        second_operation,
        &error
    ));
    assert(last_submit_completed_inline == 1);
    first_operation = initialize_receive(
        &fixture,
        &first_storage,
        first.receiver,
        first_buffer,
        sizeof(first_buffer),
        &first_completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        first_operation,
        &error
    ));
    assert(fixture.net->receive_cancel(
        fixture.backend,
        first_operation,
        &error
    ));
    assert(fixture.net->receive_quiesce(
        fixture.backend,
        first_operation,
        &error
    ));
    assert(first_completion.calls == 1u);
    assert(first_completion.completion.terminal_kind ==
        CR_NET_RECEIVE_CANCELED);
    assert(second_completion.calls == 1u);
    assert(second_completion.completion.terminal_kind ==
        CR_NET_RECEIVE_READY);
    assert(fixture.net->receive_destroy(
        fixture.backend,
        first_operation,
        &error
    ));
    quiesce_and_destroy(&fixture, second_operation);
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&first);
    cr_test_close_socket_pair(&second);
}

static void test_busy_socket_and_event_budget(void) {
    static const char first_payload[] = "a";
    static const char second_payload[] = "b";
    cr_test_socket_pair first = cr_test_make_socket_pair();
    cr_test_socket_pair second = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage[3];
    completion_state completion[3] = {{0}};
    unsigned char buffer[3][8] = {{0}};
    cr_net_receive_operation *operation[3];
    cr_net_error error;
    cr_backend_pump_result pump;

    memset(storage, 0, sizeof(storage));
    operation[0] = initialize_receive(
        &fixture,
        &storage[0],
        first.receiver,
        buffer[0],
        sizeof(buffer[0]),
        &completion[0]
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation[0],
        &error
    ));
    operation[1] = initialize_receive(
        &fixture,
        &storage[1],
        first.receiver,
        buffer[1],
        sizeof(buffer[1]),
        &completion[1]
    );
    assert(!fixture.net->receive_submit(
        fixture.backend,
        operation[1],
        &error
    ));
    assert(error.category == CR_NET_ERROR_BUSY);
    assert(fixture.net->receive_destroy(
        fixture.backend,
        operation[1],
        &error
    ));
    cr_test_send_exact(
        first.sender,
        first_payload,
        (int)(sizeof(first_payload) - 1u)
    );

    cr_test_send_exact(
        second.sender,
        second_payload,
        (int)(sizeof(second_payload) - 1u)
    );
    operation[2] = initialize_receive(
        &fixture,
        &storage[2],
        second.receiver,
        buffer[2],
        sizeof(buffer[2]),
        &completion[2]
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation[2],
        &error
    ));
    assert(cr_backend_pump(
        fixture.backend,
        UINT64_MAX,
        UINT32_C(1),
        &pump
    ));
    assert(pump.reason == CR_BACKEND_PUMP_PROGRESS);
    assert(pump.events_dispatched == UINT32_C(1));
    assert(completion[0].calls + completion[2].calls == 1u);
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(completion[0].calls == 1u);
    assert(completion[2].calls == 1u);
    quiesce_and_destroy(&fixture, operation[0]);
    quiesce_and_destroy(&fixture, operation[2]);
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&first);
    cr_test_close_socket_pair(&second);
}

typedef struct interrupt_thread_context {
    cr_backend *backend;
} interrupt_thread_context;

static DWORD WINAPI interrupt_thread(void *raw_context) {
    interrupt_thread_context *context =
        (interrupt_thread_context *)raw_context;
    cr_backend_error error;

    assert(cr_backend_interrupt(context->backend, &error));
    assert(error.category == CR_BACKEND_ERROR_NONE);
    return 0;
}

static void test_timeout_and_interrupt(void) {
    backend_fixture fixture = make_backend();
    cr_backend_pump_result pump;
    cr_backend_error error;
    interrupt_thread_context context;
    HANDLE thread;

    assert(cr_backend_pump(
        fixture.backend,
        UINT64_C(0),
        UINT32_C(1),
        &pump
    ));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);
    assert(pump.events_dispatched == UINT32_C(0));
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
    assert(!cr_backend_pump(
        fixture.backend,
        UINT64_C(0),
        UINT32_C(0),
        &pump
    ));
    assert(pump.reason == CR_BACKEND_PUMP_ERROR);
    assert(pump.error_category == CR_BACKEND_ERROR_INVALID_ARGUMENT);

    assert(cr_backend_interrupt(fixture.backend, &error));
    assert(cr_backend_interrupt(fixture.backend, &error));
    pump_one(&fixture, CR_BACKEND_PUMP_INTERRUPTED);
    cr_test_diff_emit(
        "interrupt",
        CR_NET_RECEIVE_INVALID,
        UINT64_C(0),
        CR_NET_ERROR_NONE,
        UINT32_C(0),
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        CR_BACKEND_PUMP_INTERRUPTED,
        UINT32_C(1)
    );
    assert(cr_backend_pump(
        fixture.backend,
        UINT64_C(0),
        UINT32_C(1),
        &pump
    ));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);

    context.backend = fixture.backend;
    thread = CreateThread(NULL, 0, interrupt_thread, &context, 0, NULL);
    assert(thread != NULL);
    pump_one(&fixture, CR_BACKEND_PUMP_INTERRUPTED);
    assert(WaitForSingleObject(thread, INFINITE) == WAIT_OBJECT_0);
    assert(CloseHandle(thread));
    destroy_backend(&fixture);
}

static void test_eof_and_winsock_error(void) {
    cr_test_socket_pair eof_pair = cr_test_make_socket_pair();
    cr_test_socket_pair error_pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage eof_storage = {0};
    operation_storage error_storage = {0};
    completion_state eof_completion = {0};
    completion_state error_completion = {0};
    unsigned char eof_buffer[8] = {0};
    unsigned char error_buffer[8] = {0};
    cr_net_receive_operation *eof_operation;
    cr_net_receive_operation *error_operation;
    cr_net_error error;
    struct linger reset = {1, 0};

    eof_operation = initialize_receive(
        &fixture,
        &eof_storage,
        eof_pair.receiver,
        eof_buffer,
        sizeof(eof_buffer),
        &eof_completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        eof_operation,
        &error
    ));
    assert(shutdown(eof_pair.sender, SD_SEND) == 0);
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(eof_completion.completion.terminal_kind == CR_NET_RECEIVE_READY);
    assert(eof_completion.completion.bytes_transferred == UINT64_C(0));
    quiesce_and_destroy(&fixture, eof_operation);
    cr_test_diff_emit(
        "eof",
        eof_completion.completion.terminal_kind,
        eof_completion.completion.bytes_transferred,
        eof_completion.completion.error_category,
        eof_completion.calls,
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );

    error_operation = initialize_receive(
        &fixture,
        &error_storage,
        error_pair.receiver,
        error_buffer,
        sizeof(error_buffer),
        &error_completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        error_operation,
        &error
    ));
    assert(setsockopt(
        error_pair.sender,
        SOL_SOCKET,
        SO_LINGER,
        (const char *)&reset,
        (int)sizeof(reset)
    ) == 0);
    assert(closesocket(error_pair.sender) == 0);
    error_pair.sender = INVALID_SOCKET;
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(error_completion.calls == 1u);
    assert(error_completion.completion.terminal_kind == CR_NET_RECEIVE_ERROR);
    assert(error_completion.completion.error_category ==
        CR_NET_ERROR_NETWORK_FAILURE);
    assert(error_completion.completion.native_error_domain ==
        CR_NATIVE_ERROR_DOMAIN_WINSOCK);
    assert(error_completion.completion.native_error_code != INT64_C(0));
    quiesce_and_destroy(&fixture, error_operation);
    cr_test_diff_emit(
        "error",
        error_completion.completion.terminal_kind,
        error_completion.completion.bytes_transferred,
        error_completion.completion.error_category,
        error_completion.calls,
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );

    destroy_backend(&fixture);
    cr_test_close_socket_pair(&eof_pair);
    cr_test_close_socket_pair(&error_pair);
}

static void test_foreign_port_association_maps_win32_error(void) {
    cr_test_socket_pair pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state completion = {0};
    unsigned char buffer[8] = {0};
    cr_net_receive_operation *operation;
    cr_net_error error;
    HANDLE foreign_port;

    foreign_port = CreateIoCompletionPort(
        INVALID_HANDLE_VALUE,
        NULL,
        (ULONG_PTR)0,
        1
    );
    assert(foreign_port != NULL);
    assert(CreateIoCompletionPort(
        (HANDLE)(uintptr_t)pair.receiver,
        foreign_port,
        (ULONG_PTR)0,
        0
    ) == foreign_port);
    operation = initialize_receive(
        &fixture,
        &storage,
        pair.receiver,
        buffer,
        sizeof(buffer),
        &completion
    );
    assert(!fixture.net->receive_submit(
        fixture.backend,
        operation,
        &error
    ));
    assert(error.category == CR_NET_ERROR_NETWORK_FAILURE);
    assert(error.native_domain == CR_NATIVE_ERROR_DOMAIN_WIN32);
    assert(error.native_code != INT64_C(0));
    assert(completion.calls == 0u);
    assert(fixture.net->receive_destroy(
        fixture.backend,
        operation,
        &error
    ));
    destroy_backend(&fixture);
    assert(CloseHandle(foreign_port));
    cr_test_close_socket_pair(&pair);
}

static void test_shutdown_quiesces_and_preserves_socket(void) {
    static const char payload[] = "z";
    cr_test_socket_pair pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state completion = {0};
    unsigned char buffer[8] = {0};
    cr_net_receive_operation *operation;
    cr_net_error net_error;
    cr_backend_error backend_error;
    struct sockaddr_storage address;
    int address_size = (int)sizeof(address);
    char received = 0;
    unsigned int allocations_after_create = iocp_allocations;
    unsigned int frees_after_create = iocp_frees;

    operation = initialize_receive(
        &fixture,
        &storage,
        pair.receiver,
        buffer,
        sizeof(buffer),
        &completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation,
        &net_error
    ));
    assert(iocp_allocations == allocations_after_create);
    assert(iocp_frees == frees_after_create);
    assert(cr_backend_shutdown(fixture.backend, &backend_error));
    assert(completion.calls == 1u);
    assert(completion.completion.terminal_kind == CR_NET_RECEIVE_CANCELED);
    assert(iocp_allocations == allocations_after_create);
    assert(iocp_frees == frees_after_create);
    assert(fixture.net->receive_destroy(
        fixture.backend,
        operation,
        &net_error
    ));
    cr_test_diff_emit(
        "shutdown",
        completion.completion.terminal_kind,
        completion.completion.bytes_transferred,
        completion.completion.error_category,
        completion.calls,
        UINT32_C(0),
        UINT32_C(1),
        UINT32_C(0),
        CR_BACKEND_PUMP_PROGRESS,
        UINT32_C(1)
    );
    destroy_backend(&fixture);

    assert(getsockname(
        pair.receiver,
        (struct sockaddr *)&address,
        &address_size
    ) == 0);
    cr_test_send_exact(pair.sender, payload, 1);
    assert(recv(pair.receiver, &received, 1, 0) == 1);
    assert(received == payload[0]);
    cr_test_close_socket_pair(&pair);
}

int main(void) {
    WSADATA winsock;

    assert(WSAStartup(MAKEWORD(2, 2), &winsock) == 0);
    test_immediate_completion_wins_late_cancel();
    test_deferred_completion();
    test_cancel_before_completion();
    test_quiesce_dispatches_unrelated_completion();
    test_busy_socket_and_event_budget();
    test_timeout_and_interrupt();
    test_eof_and_winsock_error();
    test_foreign_port_association_maps_win32_error();
    test_shutdown_quiesces_and_preserves_socket();
    assert(iocp_allocations == iocp_frees);
    assert(iocp_handles_opened > 0u);
    assert(iocp_handles_opened == iocp_handles_closed);
    assert(WSACleanup() == 0);
    return 0;
}
