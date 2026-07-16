#include "macos_helpers.h"

#include "cr_backend_internal.h"
#if defined(CR_BACKEND_DIFFERENTIAL)
#include "transcript.h"
#else
#define cr_test_diff_emit(...) ((void)0)
#endif

#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/event.h>

static unsigned int kqueue_allocations;
static unsigned int kqueue_frees;
static unsigned int kqueue_fds_opened;
static unsigned int kqueue_fds_closed;
static unsigned int rearm_count;
static uint64_t last_generation;
static uint64_t last_token;
static uint64_t substitute_token;
static int drain_before_recv;
static unsigned int eof_events;
static uint32_t last_eof_fflags;
static int forced_recv_errno;

void *cr_test_kqueue_calloc(size_t count, size_t size) {
    void *pointer = calloc(count, size);
    if (pointer != NULL) kqueue_allocations++;
    return pointer;
}

void cr_test_kqueue_free(void *pointer) {
    if (pointer != NULL) kqueue_frees++;
    free(pointer);
}

void cr_test_kqueue_fd_opened(int fd) {
    assert(fd >= 0);
    kqueue_fds_opened++;
}

void cr_test_kqueue_fd_closed(int fd) {
    assert(fd >= 0);
    kqueue_fds_closed++;
}

void cr_test_kqueue_submit_observed(
    const void *operation,
    uint64_t generation,
    uint64_t token
) {
    assert(operation != NULL);
    assert(generation != UINT64_C(0));
    assert(token >= UINT64_C(3));
    last_generation = generation;
    last_token = token;
}

void cr_test_kqueue_before_recv(
    const void *operation,
    uint64_t generation,
    int fd
) {
    unsigned char discarded[64];
    ssize_t received;

    assert(operation != NULL);
    assert(generation != UINT64_C(0));
    if (!drain_before_recv) return;
    drain_before_recv = 0;
    do {
        received = recv(fd, discarded, sizeof(discarded), 0);
    } while (received < 0 && errno == EINTR);
    assert(received > 0);
}

void cr_test_kqueue_rearmed(
    const void *operation,
    uint64_t generation,
    uint64_t token
) {
    assert(operation != NULL);
    assert(generation != UINT64_C(0));
    assert(token >= UINT64_C(3));
    rearm_count++;
}

uint64_t cr_test_kqueue_filter_event_token(uint64_t token) {
    if (substitute_token != UINT64_C(0) && token >= UINT64_C(3)) {
        uint64_t replacement = substitute_token;
        substitute_token = UINT64_C(0);
        return replacement;
    }
    return token;
}

void cr_test_kqueue_event_observed(
    uint64_t token,
    uint16_t flags,
    uint32_t fflags,
    intptr_t data
) {
    (void)token;
    (void)data;
    if ((flags & EV_EOF) != 0u) {
        eof_events++;
        last_eof_fflags = fflags;
    }
}

ssize_t cr_test_kqueue_recv(int fd, void *buffer, size_t size) {
    if (forced_recv_errno != 0) {
        int native_error = forced_recv_errno;
        forced_recv_errno = 0;
        errno = native_error;
        return -1;
    }
    return recv(fd, buffer, size, 0);
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
        &cr_backend_kqueue_provider_desc,
        &fixture.backend,
        &error
    ));
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
    fixture->backend = NULL;
    fixture->net = NULL;
}

static cr_native_socket_handle socket_handle(int fd) {
    return (cr_native_socket_handle){
        CR_NATIVE_SOCKET_POSIX_FD,
        UINT32_C(0),
        (uintptr_t)fd
    };
}

static cr_net_receive_operation *initialize_receive(
    const backend_fixture *fixture,
    operation_storage *storage,
    int fd,
    void *buffer,
    size_t buffer_size,
    completion_state *completion
) {
    cr_net_receive_operation *operation =
        (cr_net_receive_operation *)(void *)storage->bytes;
    cr_net_error error;

    if (!fixture->net->receive_initialize(
            fixture->backend,
            operation,
            sizeof(*storage),
            socket_handle(fd),
            buffer,
            (uint64_t)buffer_size,
            on_completion,
            completion,
            &error
        )) {
        fprintf(
            stderr,
            "receive_initialize fd=%d category=%u domain=%u code=%lld\n",
            fd,
            (unsigned int)error.category,
            (unsigned int)error.native_domain,
            (long long)error.native_code
        );
        abort();
    }
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
    assert(fixture->net->receive_destroy(
        fixture->backend,
        operation,
        &error
    ));
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

static void test_data_and_rearm(void) {
    static const char ready[] = "ready";
    static const char drained[] = "drain";
    static const char after_rearm[] = "after-rearm";
    cr_test_socket_pair pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state completion = {0};
    unsigned char buffer[32] = {0};
    cr_net_receive_operation *operation;
    cr_net_error error;
    unsigned int previous_rearms;

    cr_test_send_exact(pair.sender, ready, sizeof(ready) - 1u);
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
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(completion.completion.terminal_kind == CR_NET_RECEIVE_READY);
    assert(memcmp(buffer, ready, sizeof(ready) - 1u) == 0);
    assert(fixture.net->receive_quiesce(
        fixture.backend,
        operation,
        &error
    ));
    cr_test_diff_emit(
        "success",
        completion.completion.terminal_kind,
        completion.completion.bytes_transferred,
        completion.completion.error_category,
        completion.calls,
        UINT32_C(0), UINT32_C(1), UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS, UINT32_C(1)
    );

    memset(&completion, 0, sizeof(completion));
    memset(buffer, 0, sizeof(buffer));
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
    previous_rearms = rearm_count;
    drain_before_recv = 1;
    cr_test_send_exact(pair.sender, drained, sizeof(drained) - 1u);
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(completion.calls == 0u);
    assert(rearm_count == previous_rearms + 1u);
    cr_test_send_exact(
        pair.sender,
        after_rearm,
        sizeof(after_rearm) - 1u
    );
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(completion.calls == 1u);
    assert(memcmp(buffer, after_rearm, sizeof(after_rearm) - 1u) == 0);
    quiesce_and_destroy(&fixture, operation);
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&pair);
}

static void test_dispatch_suppresses_duplicate_readiness(void) {
    static const char payload[] = "ab";
    cr_test_socket_pair pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state first_completion = {0};
    completion_state second_completion = {0};
    unsigned char first_byte = 0;
    unsigned char second_byte = 0;
    cr_net_receive_operation *operation;
    cr_net_error error;
    cr_backend_pump_result pump;

    cr_test_send_exact(pair.sender, payload, sizeof(payload) - 1u);
    operation = initialize_receive(
        &fixture,
        &storage,
        pair.receiver,
        &first_byte,
        1u,
        &first_completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation,
        &error
    ));
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(first_byte == (unsigned char)'a');
    assert(cr_backend_pump(
        fixture.backend,
        UINT64_C(0),
        UINT32_C(1),
        &pump
    ));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);
    assert(fixture.net->receive_quiesce(
        fixture.backend,
        operation,
        &error
    ));
    operation = initialize_receive(
        &fixture,
        &storage,
        pair.receiver,
        &second_byte,
        1u,
        &second_completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation,
        &error
    ));
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(second_byte == (unsigned char)'b');
    quiesce_and_destroy(&fixture, operation);
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&pair);
}

static void test_cancel_quiescence_and_stale_token(void) {
    static const char payload[] = "stale";
    cr_test_socket_pair first = cr_test_make_socket_pair();
    cr_test_socket_pair second = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state first_completion = {0};
    completion_state second_completion = {0};
    unsigned char first_buffer[8] = {0};
    unsigned char second_buffer[8] = {0};
    cr_net_receive_operation *operation;
    cr_net_error error;
    uint64_t retired_token;
    uint64_t generation;

    operation = initialize_receive(
        &fixture,
        &storage,
        first.receiver,
        first_buffer,
        sizeof(first_buffer),
        &first_completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation,
        &error
    ));
    retired_token = last_token;
    generation = last_generation;
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
    assert(first_completion.completion.terminal_kind ==
        CR_NET_RECEIVE_CANCELED);
    assert(fixture.net->receive_quiesce(
        fixture.backend,
        operation,
        &error
    ));
    cr_test_diff_emit(
        "cancel",
        first_completion.completion.terminal_kind,
        first_completion.completion.bytes_transferred,
        first_completion.completion.error_category,
        first_completion.calls,
        UINT32_C(0), UINT32_C(1), UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS, UINT32_C(1)
    );

    operation = initialize_receive(
        &fixture,
        &storage,
        second.receiver,
        second_buffer,
        sizeof(second_buffer),
        &second_completion
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation,
        &error
    ));
    assert(last_generation == generation + UINT64_C(1));
    assert(last_token != retired_token);
    substitute_token = retired_token;
    cr_test_send_exact(second.sender, payload, sizeof(payload) - 1u);
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(second_completion.calls == 0u);
    assert(fixture.net->receive_cancel(
        fixture.backend,
        operation,
        &error
    ));
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(second_completion.completion.terminal_kind ==
        CR_NET_RECEIVE_CANCELED);
    quiesce_and_destroy(&fixture, operation);
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&first);
    cr_test_close_socket_pair(&second);
}

static void test_unrelated_cancel_and_event_budget(void) {
    static const char first_payload[] = "a";
    static const char second_payload[] = "b";
    cr_test_socket_pair first = cr_test_make_socket_pair();
    cr_test_socket_pair second = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage[5];
    completion_state completion[5];
    unsigned char buffer[5][8] = {{0}};
    cr_net_receive_operation *operation[5];
    cr_net_error error;
    cr_backend_pump_result pump;

    memset(storage, 0, sizeof(storage));
    memset(completion, 0, sizeof(completion));
    operation[0] = initialize_receive(
        &fixture,
        &storage[0],
        first.receiver,
        buffer[0],
        sizeof(buffer[0]),
        &completion[0]
    );
    operation[1] = initialize_receive(
        &fixture,
        &storage[1],
        second.receiver,
        buffer[1],
        sizeof(buffer[1]),
        &completion[1]
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation[0],
        &error
    ));
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation[1],
        &error
    ));
    operation[4] = initialize_receive(
        &fixture,
        &storage[4],
        first.receiver,
        buffer[4],
        sizeof(buffer[4]),
        &completion[4]
    );
    assert(!fixture.net->receive_submit(
        fixture.backend,
        operation[4],
        &error
    ));
    assert(error.category == CR_NET_ERROR_BUSY);
    assert(fixture.net->receive_destroy(
        fixture.backend,
        operation[4],
        &error
    ));
    assert(fixture.net->receive_cancel(
        fixture.backend,
        operation[1],
        &error
    ));
    assert(fixture.net->receive_quiesce(
        fixture.backend,
        operation[0],
        &error
    ));
    assert(completion[0].calls == 1u);
    assert(completion[1].calls == 1u);
    assert(fixture.net->receive_destroy(
        fixture.backend,
        operation[0],
        &error
    ));
    quiesce_and_destroy(&fixture, operation[1]);

    do {
        assert(cr_backend_pump(
            fixture.backend,
            UINT64_C(0),
            UINT32_C(4),
            &pump
        ));
    } while (pump.reason == CR_BACKEND_PUMP_PROGRESS);
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);
    cr_test_diff_emit(
        "timeout",
        CR_NET_RECEIVE_INVALID, UINT64_C(0), CR_NET_ERROR_NONE,
        UINT32_C(0), UINT32_C(0), UINT32_C(1), UINT32_C(1),
        pump.reason, pump.events_dispatched
    );

    operation[2] = initialize_receive(
        &fixture,
        &storage[2],
        first.receiver,
        buffer[2],
        sizeof(buffer[2]),
        &completion[2]
    );
    operation[3] = initialize_receive(
        &fixture,
        &storage[3],
        second.receiver,
        buffer[3],
        sizeof(buffer[3]),
        &completion[3]
    );
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation[2],
        &error
    ));
    assert(fixture.net->receive_submit(
        fixture.backend,
        operation[3],
        &error
    ));
    cr_test_send_exact(first.sender, first_payload, 1u);
    cr_test_send_exact(second.sender, second_payload, 1u);
    assert(cr_backend_pump(
        fixture.backend,
        UINT64_MAX,
        UINT32_C(1),
        &pump
    ));
    assert(pump.events_dispatched == UINT32_C(1));
    assert(completion[2].calls + completion[3].calls == 1u);
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(completion[2].calls == 1u);
    assert(completion[3].calls == 1u);
    quiesce_and_destroy(&fixture, operation[2]);
    quiesce_and_destroy(&fixture, operation[3]);
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&first);
    cr_test_close_socket_pair(&second);
}

typedef struct interrupt_context {
    cr_backend *backend;
} interrupt_context;

static void *interrupt_thread(void *raw_context) {
    interrupt_context *context = (interrupt_context *)raw_context;
    cr_backend_error error;

    assert(cr_backend_interrupt(context->backend, &error));
    return NULL;
}

static void test_timeout_interrupt_and_validation(void) {
    cr_test_socket_pair pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state completion = {0};
    unsigned char buffer[8] = {0};
    cr_net_receive_operation *operation =
        (cr_net_receive_operation *)(void *)storage.bytes;
    cr_backend_pump_result pump;
    cr_backend_error backend_error;
    cr_net_error net_error;
    interrupt_context context;
    pthread_t thread;
    int flags;

    assert(cr_backend_pump(
        fixture.backend,
        UINT64_C(0),
        UINT32_C(1),
        &pump
    ));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);
    assert(!cr_backend_pump(
        fixture.backend,
        UINT64_C(0),
        UINT32_C(0),
        &pump
    ));
    assert(cr_backend_interrupt(fixture.backend, &backend_error));
    assert(cr_backend_interrupt(fixture.backend, &backend_error));
    pump_one(&fixture, CR_BACKEND_PUMP_INTERRUPTED);
    cr_test_diff_emit(
        "interrupt",
        CR_NET_RECEIVE_INVALID, UINT64_C(0), CR_NET_ERROR_NONE,
        UINT32_C(0), UINT32_C(0), UINT32_C(1), UINT32_C(1),
        CR_BACKEND_PUMP_INTERRUPTED, UINT32_C(1)
    );
    context.backend = fixture.backend;
    assert(pthread_create(&thread, NULL, interrupt_thread, &context) == 0);
    pump_one(&fixture, CR_BACKEND_PUMP_INTERRUPTED);
    assert(pthread_join(thread, NULL) == 0);

    flags = fcntl(pair.receiver, F_GETFL, 0);
    assert(flags >= 0);
    assert(fcntl(pair.receiver, F_SETFL, flags & ~O_NONBLOCK) == 0);
    assert(!fixture.net->receive_initialize(
        fixture.backend,
        operation,
        sizeof(storage),
        socket_handle(pair.receiver),
        buffer,
        sizeof(buffer),
        on_completion,
        &completion,
        &net_error
    ));
    assert(net_error.category == CR_NET_ERROR_INVALID_ARGUMENT);
    assert(net_error.native_code == EINVAL);
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&pair);
}

static void test_eof_and_errno_are_independent(void) {
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
    unsigned int eof_before = eof_events;

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
    assert(shutdown(eof_pair.sender, SHUT_WR) == 0);
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(eof_events == eof_before + 1u);
    assert(last_eof_fflags == 0u);
    assert(eof_completion.completion.terminal_kind == CR_NET_RECEIVE_READY);
    assert(eof_completion.completion.bytes_transferred == UINT64_C(0));
    quiesce_and_destroy(&fixture, eof_operation);
    cr_test_diff_emit(
        "eof",
        eof_completion.completion.terminal_kind,
        eof_completion.completion.bytes_transferred,
        eof_completion.completion.error_category,
        eof_completion.calls,
        UINT32_C(0), UINT32_C(1), UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS, UINT32_C(1)
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
    forced_recv_errno = ECONNRESET;
    cr_test_send_exact(error_pair.sender, "x", 1u);
    pump_one(&fixture, CR_BACKEND_PUMP_PROGRESS);
    assert(error_completion.calls == 1u);
    assert(error_completion.completion.terminal_kind == CR_NET_RECEIVE_ERROR);
    assert(error_completion.completion.native_error_domain ==
        CR_NATIVE_ERROR_DOMAIN_ERRNO);
    assert(error_completion.completion.native_error_code != INT64_C(0));
    quiesce_and_destroy(&fixture, error_operation);
    cr_test_diff_emit(
        "error",
        error_completion.completion.terminal_kind,
        error_completion.completion.bytes_transferred,
        error_completion.completion.error_category,
        error_completion.calls,
        UINT32_C(0), UINT32_C(1), UINT32_C(1),
        CR_BACKEND_PUMP_PROGRESS, UINT32_C(1)
    );
    destroy_backend(&fixture);
    cr_test_close_socket_pair(&eof_pair);
    cr_test_close_socket_pair(&error_pair);
}

static void test_shutdown_preserves_borrowed_descriptor(void) {
    static const char payload[] = "z";
    cr_test_socket_pair pair = cr_test_make_socket_pair();
    backend_fixture fixture = make_backend();
    operation_storage storage = {0};
    completion_state completion = {0};
    unsigned char buffer[8] = {0};
    cr_net_receive_operation *operation;
    cr_net_error net_error;
    cr_backend_error backend_error;
    struct sockaddr_storage peer;
    socklen_t peer_size = (socklen_t)sizeof(peer);
    unsigned char received = 0;
    unsigned int allocations_after_create = kqueue_allocations;
    unsigned int frees_after_create = kqueue_frees;

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
    assert(cr_backend_shutdown(fixture.backend, &backend_error));
    assert(completion.completion.terminal_kind == CR_NET_RECEIVE_CANCELED);
    assert(kqueue_allocations == allocations_after_create);
    assert(kqueue_frees == frees_after_create);
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
        UINT32_C(0), UINT32_C(1), UINT32_C(0),
        CR_BACKEND_PUMP_PROGRESS, UINT32_C(1)
    );
    destroy_backend(&fixture);
    assert(getpeername(
        pair.receiver,
        (struct sockaddr *)&peer,
        &peer_size
    ) == 0);
    cr_test_send_exact(pair.sender, payload, 1u);
    cr_test_wait_readable(pair.receiver);
    assert(recv(pair.receiver, &received, 1u, 0) == 1);
    assert(received == (unsigned char)payload[0]);
    cr_test_close_socket_pair(&pair);
}

int main(void) {
    test_data_and_rearm();
    test_dispatch_suppresses_duplicate_readiness();
    test_cancel_quiescence_and_stale_token();
    test_unrelated_cancel_and_event_budget();
    test_timeout_interrupt_and_validation();
    test_eof_and_errno_are_independent();
    test_shutdown_preserves_borrowed_descriptor();
    assert(kqueue_allocations == kqueue_frees);
    assert(kqueue_fds_opened > 0u);
    assert(kqueue_fds_opened == kqueue_fds_closed);
    return 0;
}
