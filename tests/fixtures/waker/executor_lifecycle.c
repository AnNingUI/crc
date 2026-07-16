#include "cr_executor.h"

#include <assert.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

typedef struct allocation_record {
    void *allocation;
    size_t size;
    bool live;
} allocation_record;

static allocation_record allocation_records[128];
static size_t allocation_record_count;
static size_t live_allocations;
static int failure_countdown = -1;

static bool test_allocator_must_fail(void) {
    if (failure_countdown < 0) return false;
    if (failure_countdown == 0) {
        failure_countdown = -1;
        return true;
    }
    failure_countdown--;
    return false;
}

static void test_allocator_record(void *allocation, size_t size) {
    assert(allocation != NULL);
    assert(allocation_record_count < 128u);
    allocation_records[allocation_record_count++] =
        (allocation_record){allocation, size, true};
    live_allocations++;
}

void *test_executor_malloc(size_t size) {
    void *allocation;
    if (test_allocator_must_fail()) return NULL;
    allocation = malloc(size);
    if (allocation != NULL) test_allocator_record(allocation, size);
    return allocation;
}

void *test_executor_calloc(size_t count, size_t size) {
    void *allocation;
    if (test_allocator_must_fail()) return NULL;
    allocation = calloc(count, size);
    if (allocation != NULL) test_allocator_record(allocation, count * size);
    return allocation;
}

void test_executor_free(void *allocation) {
    if (allocation == NULL) return;
    for (size_t index = allocation_record_count; index > 0u; index--) {
        allocation_record *record = &allocation_records[index - 1u];
        if (record->allocation == allocation && record->live) {
            record->live = false;
            assert(live_allocations > 0u);
            live_allocations--;
            free(allocation);
            return;
        }
    }
    assert(!"executor freed an unknown or already freed allocation");
}

static void test_allocator_fail_after(int successful_allocations) {
    assert(successful_allocations >= 0);
    failure_countdown = successful_allocations;
}

static size_t test_allocator_live_count(void) {
    return live_allocations;
}

static bool test_allocator_pointer_was_freed(const void *pointer) {
    uintptr_t address = (uintptr_t)pointer;
    for (size_t index = 0u; index < allocation_record_count; index++) {
        const allocation_record *record = &allocation_records[index];
        uintptr_t start = (uintptr_t)record->allocation;
        if (!record->live && address >= start &&
            address - start < record->size) {
            return true;
        }
    }
    return false;
}

enum lifecycle_mode {
    LIFECYCLE_READY,
    LIFECYCLE_ERROR,
    LIFECYCLE_PENDING,
    LIFECYCLE_YIELD
};

typedef struct lifecycle_state {
    int id;
    enum lifecycle_mode mode;
    int polls;
    int drops;
    cr_error error;
    cr_waker *external_waker;
} lifecycle_state;

typedef struct lifecycle_root {
    lifecycle_state state;
    cr_awaitable_vtable vtable;
} lifecycle_root;

typedef struct lifecycle_observation {
    int id;
    cr_poll_status status;
    int copied_value;
    int copied_error;
    const void *borrowed_value;
    const cr_error *borrowed_error;
} lifecycle_observation;

typedef struct lifecycle_log {
    lifecycle_observation entries[32];
    size_t count;
} lifecycle_log;

typedef struct lifecycle_binding {
    lifecycle_log *log;
    int id;
    cr_executor *executor;
    bool request_shutdown_on_yield;
} lifecycle_binding;

static cr_poll_status lifecycle_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    lifecycle_state *state = (lifecycle_state *)raw;
    state->polls++;
    assert(poll_context != NULL);
    assert(cr_waker_is_valid(poll_context->waker));
    if (state->external_waker != NULL &&
        !cr_waker_is_valid(state->external_waker)) {
        assert(cr_waker_clone(
            poll_context->waker,
            state->external_waker
        ));
    }

    if (state->mode == LIFECYCLE_READY) {
        *(int *)out_value = state->id * 10;
        return CR_POLL_READY;
    }
    if (state->mode == LIFECYCLE_ERROR) {
        state->error = (cr_error){800 + state->id, "lifecycle error"};
        return CR_POLL_ERROR;
    }
    if (state->mode == LIFECYCLE_PENDING) return CR_POLL_PENDING;
    assert(state->mode == LIFECYCLE_YIELD);
    *(int *)out_value = state->id * 10 + state->polls;
    return CR_POLL_YIELDED;
}

static const cr_error *lifecycle_error(const void *raw) {
    const lifecycle_state *state = (const lifecycle_state *)raw;
    return state->error.code != 0 ? &state->error : NULL;
}

static void lifecycle_drop(void *raw) {
    lifecycle_state *state = (lifecycle_state *)raw;
    state->drops++;
    state->error.code = -999;
    state->error.message = "dropped payload";
}

static void lifecycle_root_init(
    lifecycle_root *root,
    int id,
    enum lifecycle_mode mode,
    cr_waker *external_waker
) {
    memset(root, 0, sizeof(*root));
    root->state.id = id;
    root->state.mode = mode;
    root->state.external_waker = external_waker;
    root->vtable = (cr_awaitable_vtable){
        CR_AWAITABLE_VTABLE_ABI_VERSION,
        sizeof(cr_awaitable_vtable),
        mode == LIFECYCLE_YIELD ? CR_AWAITABLE_CAN_YIELD : 0u,
        CR_POLL_CAP_WAKER,
        lifecycle_poll,
        lifecycle_error,
        lifecycle_drop,
        sizeof(int),
        _Alignof(int)
    };
}

static cr_awaitable lifecycle_awaitable(lifecycle_root *root) {
    return (cr_awaitable){&root->state, &root->vtable};
}

static void lifecycle_observe(
    void *raw,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    lifecycle_binding *binding = (lifecycle_binding *)raw;
    lifecycle_observation *entry;
    assert(binding->log->count < 32u);
    entry = &binding->log->entries[binding->log->count++];
    *entry = (lifecycle_observation){
        binding->id,
        status,
        0,
        0,
        value,
        error
    };
    if (status == CR_POLL_READY || status == CR_POLL_YIELDED) {
        assert(value != NULL);
        assert(error == NULL);
        entry->copied_value = *(const int *)value;
    } else if (status == CR_POLL_ERROR) {
        assert(value == NULL);
        assert(error != NULL);
        entry->copied_error = error->code;
    } else {
        assert(status == CR_POLL_CANCELED);
        assert(value == NULL);
        assert(error == NULL);
    }
    if (status == CR_POLL_YIELDED &&
        binding->request_shutdown_on_yield) {
        cr_executor_request_shutdown(binding->executor);
    }
}

static lifecycle_binding lifecycle_bind(
    lifecycle_log *log,
    int id,
    cr_executor *executor
) {
    return (lifecycle_binding){log, id, executor, false};
}

static cr_executor_task *lifecycle_spawn(
    cr_executor *executor,
    lifecycle_root *root,
    lifecycle_binding *binding
) {
    cr_error error = {0};
    cr_executor_task *ticket = NULL;
    cr_awaitable source = lifecycle_awaitable(root);
    assert(cr_executor_spawn(
        executor,
        &source,
        lifecycle_observe,
        binding,
        &error,
        &ticket
    ));
    assert(error.code == 0);
    assert(source.state == NULL && source.vtable == NULL);
    assert(ticket != NULL);
    return ticket;
}

static void test_allocation_failures_preserve_source(void) {
    cr_error error = {0};
    cr_executor *executor;
    cr_executor_task *ticket;
    lifecycle_root root;
    cr_awaitable source;

    assert(test_allocator_live_count() == 0u);
    test_allocator_fail_after(0);
    executor = cr_executor_create_single(&error);
    assert(executor == NULL);
    assert(error.code == CR_EXECUTOR_ERROR_OUT_OF_MEMORY);
    assert(test_allocator_live_count() == 0u);

    test_allocator_fail_after(1);
    executor = cr_executor_create_single(&error);
    assert(executor == NULL);
    assert(error.code == CR_EXECUTOR_ERROR_OUT_OF_MEMORY);
    assert(test_allocator_live_count() == 0u);

    executor = cr_executor_create_single(&error);
    assert(executor != NULL);
    assert(test_allocator_live_count() == 2u);
    lifecycle_root_init(&root, 1, LIFECYCLE_READY, NULL);
    source = lifecycle_awaitable(&root);

    ticket = (cr_executor_task *)(uintptr_t)1u;
    test_allocator_fail_after(0);
    assert(!cr_executor_spawn(
        executor, &source, NULL, NULL, &error, &ticket
    ));
    assert(error.code == CR_EXECUTOR_ERROR_OUT_OF_MEMORY);
    assert(ticket == NULL);
    assert(source.state == &root.state && source.vtable == &root.vtable);
    assert(root.state.drops == 0);
    assert(test_allocator_live_count() == 2u);

    ticket = (cr_executor_task *)(uintptr_t)1u;
    test_allocator_fail_after(1);
    assert(!cr_executor_spawn(
        executor, &source, NULL, NULL, &error, &ticket
    ));
    assert(error.code == CR_EXECUTOR_ERROR_OUT_OF_MEMORY);
    assert(ticket == NULL);
    assert(source.state == &root.state && source.vtable == &root.vtable);
    assert(root.state.drops == 0);
    assert(test_allocator_live_count() == 2u);

    cr_executor_destroy(executor);
    assert(test_allocator_live_count() == 0u);
}

static void test_ticket_release_before_terminal(void) {
    lifecycle_log log = {{{0}}, 0u};
    cr_error error = {0};
    cr_executor *executor = cr_executor_create_single(&error);
    lifecycle_root root;
    lifecycle_root_init(&root, 2, LIFECYCLE_READY, NULL);
    lifecycle_binding binding = lifecycle_bind(&log, 2, executor);
    cr_executor_task *ticket = lifecycle_spawn(executor, &root, &binding);
    assert(test_allocator_live_count() == 4u);

    cr_executor_task_release(ticket);
    assert(cr_executor_run_ready(executor) == 1u);
    assert(log.count == 1u);
    assert(log.entries[0].status == CR_POLL_READY);
    assert(log.entries[0].copied_value == 20);
    assert(test_allocator_pointer_was_freed(log.entries[0].borrowed_value));
    assert(root.state.drops == 1);
    assert(test_allocator_live_count() == 2u);

    cr_executor_destroy(executor);
    assert(test_allocator_live_count() == 0u);
}

static void test_terminal_payload_dies_before_ticket_and_waker(void) {
    lifecycle_log log = {{{0}}, 0u};
    cr_error error = {0};
    cr_waker external_waker = {NULL, NULL};
    cr_executor *executor = cr_executor_create_single(&error);
    lifecycle_root root;
    lifecycle_root_init(
        &root, 3, LIFECYCLE_READY, &external_waker
    );
    lifecycle_binding binding = lifecycle_bind(&log, 3, executor);
    cr_executor_task *ticket = lifecycle_spawn(executor, &root, &binding);

    assert(cr_executor_run_ready(executor) == 1u);
    assert(root.state.drops == 1);
    assert(log.count == 1u);
    assert(log.entries[0].copied_value == 30);
    assert(test_allocator_pointer_was_freed(log.entries[0].borrowed_value));
    assert(cr_waker_is_valid(&external_waker));
    assert(test_allocator_live_count() == 3u);

    cr_executor_cancel(ticket);
    cr_executor_cancel(ticket);
    assert(log.count == 1u);
    assert(root.state.drops == 1);

    cr_executor_destroy(executor);
    assert(test_allocator_live_count() == 2u);
    cr_waker_wake(&external_waker);
    cr_waker_wake(&external_waker);
    assert(test_allocator_live_count() == 2u);
    cr_executor_task_release(ticket);
    assert(test_allocator_live_count() == 2u);
    cr_waker_drop(&external_waker);
    assert(test_allocator_live_count() == 0u);
}

static void test_error_pointer_is_ephemeral(void) {
    lifecycle_log log = {{{0}}, 0u};
    cr_error error = {0};
    cr_executor *executor = cr_executor_create_single(&error);
    lifecycle_root root;
    lifecycle_root_init(&root, 4, LIFECYCLE_ERROR, NULL);
    lifecycle_binding binding = lifecycle_bind(&log, 4, executor);
    cr_executor_task *ticket = lifecycle_spawn(executor, &root, &binding);

    assert(cr_executor_run_ready(executor) == 1u);
    assert(log.count == 1u);
    assert(log.entries[0].status == CR_POLL_ERROR);
    assert(log.entries[0].copied_error == 804);
    assert(log.entries[0].borrowed_error == &root.state.error);
    assert(root.state.error.code == -999);
    assert(root.state.drops == 1);
    cr_executor_task_release(ticket);
    cr_executor_destroy(executor);
    assert(test_allocator_live_count() == 0u);
}

static void test_cancel_retains_control_block_for_late_waker(void) {
    lifecycle_log log = {{{0}}, 0u};
    cr_error error = {0};
    cr_waker external_waker = {NULL, NULL};
    cr_executor *executor = cr_executor_create_single(&error);
    lifecycle_root root;
    lifecycle_root_init(
        &root, 5, LIFECYCLE_PENDING, &external_waker
    );
    lifecycle_binding binding = lifecycle_bind(&log, 5, executor);
    cr_executor_task *ticket = lifecycle_spawn(executor, &root, &binding);

    assert(cr_executor_run_ready(executor) == 1u);
    assert(cr_waker_is_valid(&external_waker));
    cr_executor_cancel(ticket);
    cr_executor_cancel(ticket);
    assert(log.count == 1u);
    assert(log.entries[0].status == CR_POLL_CANCELED);
    assert(root.state.drops == 1);
    assert(test_allocator_live_count() == 3u);

    cr_executor_destroy(executor);
    assert(test_allocator_live_count() == 2u);
    cr_waker_wake(&external_waker);
    assert(test_allocator_live_count() == 2u);
    cr_executor_task_release(ticket);
    assert(test_allocator_live_count() == 2u);
    cr_waker_drop(&external_waker);
    assert(test_allocator_live_count() == 0u);
}

static void test_canceled_queue_record_is_safe_to_skip(void) {
    lifecycle_log log = {{{0}}, 0u};
    cr_error error = {0};
    cr_executor *executor = cr_executor_create_single(&error);
    lifecycle_root root;
    lifecycle_root_init(&root, 6, LIFECYCLE_PENDING, NULL);
    lifecycle_binding binding = lifecycle_bind(&log, 6, executor);
    cr_executor_task *ticket = lifecycle_spawn(executor, &root, &binding);

    cr_executor_cancel(ticket);
    assert(log.count == 1u);
    assert(log.entries[0].status == CR_POLL_CANCELED);
    assert(root.state.polls == 0);
    assert(root.state.drops == 1);
    assert(test_allocator_live_count() == 3u);
    cr_executor_task_release(ticket);
    assert(test_allocator_live_count() == 3u);
    assert(cr_executor_run_ready(executor) == 0u);
    assert(root.state.polls == 0);
    assert(test_allocator_live_count() == 2u);
    cr_executor_destroy(executor);
    assert(test_allocator_live_count() == 0u);
}

static void test_shutdown_covers_terminal_pending_yielded_and_queued(void) {
    lifecycle_log log = {{{0}}, 0u};
    cr_error error = {0};
    cr_executor *executor = cr_executor_create_single(&error);
    lifecycle_root terminal;
    lifecycle_root pending;
    lifecycle_root yielded;
    lifecycle_root queued;
    lifecycle_root_init(&terminal, 7, LIFECYCLE_READY, NULL);
    lifecycle_root_init(&pending, 8, LIFECYCLE_PENDING, NULL);
    lifecycle_root_init(&yielded, 9, LIFECYCLE_YIELD, NULL);
    lifecycle_root_init(&queued, 10, LIFECYCLE_PENDING, NULL);
    lifecycle_binding terminal_binding = lifecycle_bind(&log, 7, executor);
    lifecycle_binding pending_binding = lifecycle_bind(&log, 8, executor);
    lifecycle_binding yielded_binding = lifecycle_bind(&log, 9, executor);
    lifecycle_binding queued_binding = lifecycle_bind(&log, 10, executor);
    yielded_binding.request_shutdown_on_yield = true;

    cr_executor_task *terminal_ticket = lifecycle_spawn(
        executor, &terminal, &terminal_binding
    );
    assert(cr_executor_run_ready(executor) == 1u);
    assert(terminal.state.drops == 1);

    cr_executor_task *pending_ticket = lifecycle_spawn(
        executor, &pending, &pending_binding
    );
    assert(cr_executor_run_ready(executor) == 1u);
    assert(pending.state.polls == 1);
    assert(pending.state.drops == 0);

    cr_executor_task *yielded_ticket = lifecycle_spawn(
        executor, &yielded, &yielded_binding
    );
    cr_executor_task *queued_ticket = lifecycle_spawn(
        executor, &queued, &queued_binding
    );
    assert(cr_executor_run_ready(executor) == 1u);
    assert(yielded.state.polls == 1);
    assert(queued.state.polls == 0);
    assert(terminal.state.drops == 1);
    assert(pending.state.drops == 1);
    assert(yielded.state.drops == 1);
    assert(queued.state.drops == 1);

    size_t terminal_ready = 0u;
    size_t pending_canceled = 0u;
    size_t yielded_notifications = 0u;
    size_t yielded_cancellations = 0u;
    size_t queued_canceled = 0u;
    for (size_t index = 0u; index < log.count; index++) {
        lifecycle_observation *entry = &log.entries[index];
        if (entry->id == 7 && entry->status == CR_POLL_READY) {
            terminal_ready++;
        }
        if (entry->id == 8 && entry->status == CR_POLL_CANCELED) {
            pending_canceled++;
        }
        if (entry->id == 9 && entry->status == CR_POLL_YIELDED) {
            yielded_notifications++;
        }
        if (entry->id == 9 && entry->status == CR_POLL_CANCELED) {
            yielded_cancellations++;
        }
        if (entry->id == 10 && entry->status == CR_POLL_CANCELED) {
            queued_canceled++;
        }
    }
    assert(terminal_ready == 1u);
    assert(pending_canceled == 1u);
    assert(yielded_notifications == 1u);
    assert(yielded_cancellations == 1u);
    assert(queued_canceled == 1u);

    cr_executor_task_release(terminal_ticket);
    cr_executor_task_release(pending_ticket);
    cr_executor_task_release(yielded_ticket);
    cr_executor_task_release(queued_ticket);
    cr_executor_destroy(executor);
    assert(test_allocator_live_count() == 0u);
}

static void test_destroy_performs_shutdown_before_ticket_release(void) {
    lifecycle_log log = {{{0}}, 0u};
    cr_error error = {0};
    cr_executor *executor = cr_executor_create_single(&error);
    lifecycle_root root;
    lifecycle_root_init(&root, 11, LIFECYCLE_PENDING, NULL);
    lifecycle_binding binding = lifecycle_bind(&log, 11, executor);
    cr_executor_task *ticket = lifecycle_spawn(executor, &root, &binding);

    cr_executor_destroy(executor);
    assert(log.count == 1u);
    assert(log.entries[0].status == CR_POLL_CANCELED);
    assert(root.state.polls == 0);
    assert(root.state.drops == 1);
    assert(test_allocator_live_count() == 2u);
    cr_executor_task_release(ticket);
    assert(test_allocator_live_count() == 0u);
}

int main(void) {
    test_allocation_failures_preserve_source();
    test_ticket_release_before_terminal();
    test_terminal_payload_dies_before_ticket_and_waker();
    test_error_pointer_is_ephemeral();
    test_cancel_retains_control_block_for_late_waker();
    test_canceled_queue_record_is_safe_to_skip();
    test_shutdown_covers_terminal_pending_yielded_and_queued();
    test_destroy_performs_shutdown_before_ticket_release();
    assert(test_allocator_live_count() == 0u);
    return 0;
}
