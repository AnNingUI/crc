#include "cr_executor.h"

#include <assert.h>
#include <stdint.h>
#include <string.h>

#if defined(_WIN32)
#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0600
#endif
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#else
#include <pthread.h>
#endif

typedef struct test_event {
#if defined(_WIN32)
    HANDLE handle;
#else
    pthread_mutex_t lock;
    pthread_cond_t condition;
    bool signaled;
#endif
} test_event;

static void test_event_init(test_event *event) {
#if defined(_WIN32)
    event->handle = CreateEventA(NULL, TRUE, FALSE, NULL);
    assert(event->handle != NULL);
#else
    assert(pthread_mutex_init(&event->lock, NULL) == 0);
    assert(pthread_cond_init(&event->condition, NULL) == 0);
    event->signaled = false;
#endif
}

static void test_event_signal(test_event *event) {
#if defined(_WIN32)
    assert(SetEvent(event->handle));
#else
    assert(pthread_mutex_lock(&event->lock) == 0);
    event->signaled = true;
    assert(pthread_cond_broadcast(&event->condition) == 0);
    assert(pthread_mutex_unlock(&event->lock) == 0);
#endif
}

static void test_event_wait(test_event *event) {
#if defined(_WIN32)
    assert(WaitForSingleObject(event->handle, INFINITE) == WAIT_OBJECT_0);
#else
    assert(pthread_mutex_lock(&event->lock) == 0);
    while (!event->signaled) {
        assert(pthread_cond_wait(&event->condition, &event->lock) == 0);
    }
    assert(pthread_mutex_unlock(&event->lock) == 0);
#endif
}

static void test_event_destroy(test_event *event) {
#if defined(_WIN32)
    assert(CloseHandle(event->handle));
#else
    assert(pthread_cond_destroy(&event->condition) == 0);
    assert(pthread_mutex_destroy(&event->lock) == 0);
#endif
}

typedef void (*test_thread_fn)(void *argument);

typedef struct test_thread_start {
    test_thread_fn run;
    void *argument;
} test_thread_start;

typedef struct test_thread {
    test_thread_start start;
#if defined(_WIN32)
    HANDLE handle;
#else
    pthread_t handle;
#endif
} test_thread;

#if defined(_WIN32)
static DWORD WINAPI test_thread_entry(LPVOID raw) {
    test_thread_start *start = (test_thread_start *)raw;
    start->run(start->argument);
    return 0u;
}
#else
static void *test_thread_entry(void *raw) {
    test_thread_start *start = (test_thread_start *)raw;
    start->run(start->argument);
    return NULL;
}
#endif

static void test_thread_create(
    test_thread *thread,
    test_thread_fn run,
    void *argument
) {
    thread->start = (test_thread_start){run, argument};
#if defined(_WIN32)
    thread->handle = CreateThread(
        NULL, 0u, test_thread_entry, &thread->start, 0u, NULL
    );
    assert(thread->handle != NULL);
#else
    assert(pthread_create(
        &thread->handle, NULL, test_thread_entry, &thread->start
    ) == 0);
#endif
}

static void test_thread_join(test_thread *thread) {
#if defined(_WIN32)
    assert(WaitForSingleObject(thread->handle, INFINITE) == WAIT_OBJECT_0);
    assert(CloseHandle(thread->handle));
#else
    assert(pthread_join(thread->handle, NULL) == 0);
#endif
}

#if defined(_WIN32)
typedef DWORD test_thread_id;
static test_thread_id test_current_thread(void) {
    return GetCurrentThreadId();
}
static bool test_same_thread(test_thread_id left, test_thread_id right) {
    return left == right;
}
#else
typedef pthread_t test_thread_id;
static test_thread_id test_current_thread(void) {
    return pthread_self();
}
static bool test_same_thread(test_thread_id left, test_thread_id right) {
    return pthread_equal(left, right) != 0;
}
#endif

static test_event *wait_hook_event;

void test_executor_wait_hook(void *shared) {
    (void)shared;
    if (wait_hook_event != NULL) test_event_signal(wait_hook_event);
}

enum threaded_mode {
    THREADED_WAKE_READY,
    THREADED_PENDING,
    THREADED_READY
};

typedef struct threaded_state {
    enum threaded_mode mode;
    int ready_value;
    int polls;
    int drops;
    bool wait_after_registration;
    bool pending_after_wait;
    cr_waker external_waker;
    test_event registered;
    test_event continue_poll;
    test_thread_id owner;
} threaded_state;

typedef struct threaded_root {
    threaded_state state;
    cr_awaitable_vtable vtable;
} threaded_root;

typedef struct threaded_log {
    size_t count;
    cr_poll_status status[8];
    int value[8];
    test_thread_id observer_thread[8];
    test_thread_id owner;
} threaded_log;

static cr_poll_status threaded_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    threaded_state *state = (threaded_state *)raw;
    assert(test_same_thread(test_current_thread(), state->owner));
    assert(poll_context != NULL);
    assert(cr_waker_is_valid(poll_context->waker));
    assert(
        (poll_context->waker->vtable->provided_flags &
         CR_WAKER_FLAG_CROSS_THREAD) != 0u
    );
    state->polls++;
    if (!cr_waker_is_valid(&state->external_waker)) {
        assert(cr_waker_clone(
            poll_context->waker,
            &state->external_waker
        ));
        test_event_signal(&state->registered);
    }
    if (state->polls == 1 && state->wait_after_registration) {
        test_event_wait(&state->continue_poll);
        if (state->pending_after_wait) return CR_POLL_PENDING;
    }
    if (state->mode == THREADED_PENDING) return CR_POLL_PENDING;
    if (state->mode == THREADED_READY) {
        *(int *)out_value = 73;
        return CR_POLL_READY;
    }
    if (state->ready_value == 0) return CR_POLL_PENDING;
    *(int *)out_value = state->ready_value;
    return CR_POLL_READY;
}

static const cr_error *threaded_error(const void *raw) {
    (void)raw;
    return NULL;
}

static void threaded_drop(void *raw) {
    threaded_state *state = (threaded_state *)raw;
    assert(test_same_thread(test_current_thread(), state->owner));
    state->drops++;
}

static void threaded_root_init(
    threaded_root *root,
    enum threaded_mode mode,
    test_thread_id owner
) {
    memset(root, 0, sizeof(*root));
    root->state.mode = mode;
    root->state.owner = owner;
    test_event_init(&root->state.registered);
    test_event_init(&root->state.continue_poll);
    root->vtable = (cr_awaitable_vtable){
        CR_AWAITABLE_VTABLE_ABI_VERSION,
        sizeof(cr_awaitable_vtable),
        0u,
        CR_POLL_CAP_WAKER,
        threaded_poll,
        threaded_error,
        threaded_drop,
        sizeof(int),
        _Alignof(int)
    };
}

static void threaded_root_destroy(threaded_root *root) {
    cr_waker_drop(&root->state.external_waker);
    test_event_destroy(&root->state.continue_poll);
    test_event_destroy(&root->state.registered);
}

static cr_awaitable threaded_awaitable(threaded_root *root) {
    return (cr_awaitable){&root->state, &root->vtable};
}

static void threaded_observe(
    void *raw,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    threaded_log *log = (threaded_log *)raw;
    size_t index = log->count++;
    assert(index < 8u);
    assert(test_same_thread(test_current_thread(), log->owner));
    log->status[index] = status;
    log->observer_thread[index] = test_current_thread();
    log->value[index] = value != NULL ? *(const int *)value : 0;
    assert(error == NULL);
}

static cr_executor_task *threaded_spawn(
    cr_executor *executor,
    threaded_root *root,
    threaded_log *log
) {
    cr_error error = {0};
    cr_executor_task *ticket = NULL;
    cr_awaitable source = threaded_awaitable(root);
    assert(cr_executor_spawn(
        executor, &source, threaded_observe, log, &error, &ticket
    ));
    assert(source.state == NULL && source.vtable == NULL);
    return ticket;
}

typedef struct wake_context {
    threaded_state *state;
    test_event done;
} wake_context;

static void producer_wake_during_poll(void *raw) {
    wake_context *context = (wake_context *)raw;
    cr_waker local = {NULL, NULL};
    test_event_wait(&context->state->registered);
    assert(cr_waker_clone(&context->state->external_waker, &local));
    context->state->ready_value = 41;
    cr_waker_wake(&local);
    cr_waker_wake(&local);
    cr_waker_drop(&local);
    test_event_signal(&context->state->continue_poll);
    test_event_signal(&context->done);
}

static void test_cross_thread_wake_visibility_and_coalescing(void) {
    cr_error error = {0};
    test_thread_id owner = test_current_thread();
    cr_executor *executor = cr_executor_create_threaded(&error);
    assert(executor != NULL && error.code == 0);
    threaded_root root;
    threaded_root_init(&root, THREADED_WAKE_READY, owner);
    root.state.wait_after_registration = true;
    root.state.pending_after_wait = true;
    threaded_log log = {0};
    log.owner = owner;
    cr_executor_task *ticket = threaded_spawn(executor, &root, &log);
    wake_context context = {0};
    context.state = &root.state;
    test_event_init(&context.done);
    test_thread producer;
    test_thread_create(&producer, producer_wake_during_poll, &context);

    assert(cr_executor_wait_one(executor));
    assert(root.state.polls == 1);
    assert(cr_executor_wait_one(executor));
    assert(root.state.polls == 2);
    assert(root.state.drops == 1);
    assert(log.count == 1u);
    assert(log.status[0] == CR_POLL_READY);
    assert(log.value[0] == 41);
    assert(cr_executor_run_ready(executor) == 0u);
    test_event_wait(&context.done);
    test_thread_join(&producer);

    cr_executor_task_release(ticket);
    threaded_root_destroy(&root);
    test_event_destroy(&context.done);
    cr_executor_destroy(executor);
}

typedef struct shutdown_context {
    cr_executor *executor;
    threaded_state *state;
    test_event *owner_waiting;
} shutdown_context;

static void producer_requests_shutdown(void *raw) {
    shutdown_context *context = (shutdown_context *)raw;
    test_event_wait(&context->state->registered);
    test_event_wait(context->owner_waiting);
    cr_executor_request_shutdown(context->executor);
}

static void test_blocked_owner_is_released_by_shutdown_request(void) {
    cr_error error = {0};
    test_thread_id owner = test_current_thread();
    cr_executor *executor = cr_executor_create_threaded(&error);
    threaded_root root;
    threaded_root_init(&root, THREADED_PENDING, owner);
    threaded_log log = {0};
    log.owner = owner;
    cr_executor_task *ticket = threaded_spawn(executor, &root, &log);
    test_event owner_waiting;
    test_event_init(&owner_waiting);
    wait_hook_event = &owner_waiting;
    shutdown_context context = {executor, &root.state, &owner_waiting};
    test_thread producer;
    test_thread_create(&producer, producer_requests_shutdown, &context);

    assert(cr_executor_wait_one(executor));
    assert(!cr_executor_wait_one(executor));
    test_thread_join(&producer);
    wait_hook_event = NULL;
    assert(root.state.drops == 1);
    assert(log.count == 1u);
    assert(log.status[0] == CR_POLL_CANCELED);

    cr_executor_task_release(ticket);
    threaded_root_destroy(&root);
    test_event_destroy(&owner_waiting);
    cr_executor_destroy(executor);
}

typedef struct wrong_owner_context {
    cr_executor *executor;
    cr_executor_task *ticket;
    threaded_root *spawn_root;
    size_t run_result;
    bool wait_result;
    bool spawn_result;
    cr_error spawn_error;
    cr_executor_task *spawn_ticket;
    cr_awaitable spawn_source;
} wrong_owner_context;

static void producer_attempts_owner_operations(void *raw) {
    wrong_owner_context *context = (wrong_owner_context *)raw;
    context->run_result = cr_executor_run_ready(context->executor);
    context->wait_result = cr_executor_wait_one(context->executor);
    cr_executor_cancel(context->ticket);
    cr_executor_shutdown(context->executor);
    cr_executor_task_release(context->ticket);
    context->spawn_source = threaded_awaitable(context->spawn_root);
    context->spawn_result = cr_executor_spawn(
        context->executor,
        &context->spawn_source,
        NULL,
        NULL,
        &context->spawn_error,
        &context->spawn_ticket
    );
    cr_executor_destroy(context->executor);
}

static void test_owner_operations_are_rejected_off_thread(void) {
    cr_error error = {0};
    test_thread_id owner = test_current_thread();
    cr_executor *executor = cr_executor_create_threaded(&error);
    threaded_root root;
    threaded_root spawn_root;
    threaded_root_init(&root, THREADED_PENDING, owner);
    threaded_root_init(&spawn_root, THREADED_READY, owner);
    threaded_log log = {0};
    log.owner = owner;
    cr_executor_task *ticket = threaded_spawn(executor, &root, &log);
    assert(cr_executor_wait_one(executor));

    wrong_owner_context context;
    memset(&context, 0, sizeof(context));
    context.executor = executor;
    context.ticket = ticket;
    context.spawn_root = &spawn_root;
    test_thread producer;
    test_thread_create(&producer, producer_attempts_owner_operations, &context);
    test_thread_join(&producer);

    assert(context.run_result == 0u);
    assert(!context.wait_result);
    assert(!context.spawn_result);
    assert(context.spawn_error.code == CR_EXECUTOR_ERROR_INVALID_ARGUMENT);
    assert(context.spawn_ticket == NULL);
    assert(context.spawn_source.state == &spawn_root.state);
    assert(root.state.drops == 0);
    assert(log.count == 0u);

    cr_executor_cancel(ticket);
    assert(root.state.drops == 1);
    assert(log.count == 1u && log.status[0] == CR_POLL_CANCELED);
    cr_executor_task_release(ticket);
    threaded_root_destroy(&spawn_root);
    threaded_root_destroy(&root);
    cr_executor_destroy(executor);
}

typedef struct late_wake_context {
    threaded_state *state;
    test_event cloned;
    test_event destroyed;
    test_event done;
} late_wake_context;

static void producer_wakes_after_destroy(void *raw) {
    late_wake_context *context = (late_wake_context *)raw;
    cr_waker local = {NULL, NULL};
    test_event_wait(&context->state->registered);
    assert(cr_waker_clone(&context->state->external_waker, &local));
    test_event_signal(&context->cloned);
    test_event_signal(&context->state->continue_poll);
    test_event_wait(&context->destroyed);
    cr_waker_wake(&local);
    cr_waker_drop(&local);
    test_event_signal(&context->done);
}

static void test_control_block_survives_terminal_and_public_destroy(void) {
    cr_error error = {0};
    test_thread_id owner = test_current_thread();
    cr_executor *executor = cr_executor_create_threaded(&error);
    threaded_root root;
    threaded_root_init(&root, THREADED_READY, owner);
    root.state.wait_after_registration = true;
    threaded_log log = {0};
    log.owner = owner;
    cr_executor_task *ticket = threaded_spawn(executor, &root, &log);
    late_wake_context context = {0};
    context.state = &root.state;
    test_event_init(&context.cloned);
    test_event_init(&context.destroyed);
    test_event_init(&context.done);
    test_thread producer;
    test_thread_create(&producer, producer_wakes_after_destroy, &context);

    assert(cr_executor_wait_one(executor));
    test_event_wait(&context.cloned);
    assert(root.state.drops == 1);
    assert(log.count == 1u && log.status[0] == CR_POLL_READY);
    cr_waker_drop(&root.state.external_waker);
    cr_executor_task_release(ticket);
    cr_executor_destroy(executor);
    test_event_signal(&context.destroyed);
    test_event_wait(&context.done);
    test_thread_join(&producer);

    test_event_destroy(&context.done);
    test_event_destroy(&context.destroyed);
    test_event_destroy(&context.cloned);
    test_event_destroy(&root.state.continue_poll);
    test_event_destroy(&root.state.registered);
}

typedef struct wake_before_cancel_context {
    threaded_state *state;
    test_event done;
} wake_before_cancel_context;

static void producer_wakes_before_cancel(void *raw) {
    wake_before_cancel_context *context =
        (wake_before_cancel_context *)raw;
    cr_waker local = {NULL, NULL};
    test_event_wait(&context->state->registered);
    assert(cr_waker_clone(&context->state->external_waker, &local));
    cr_waker_wake(&local);
    cr_waker_wake(&local);
    cr_waker_drop(&local);
    test_event_signal(&context->done);
}

static void test_wake_before_cancel_leaves_safe_queue_record(void) {
    cr_error error = {0};
    test_thread_id owner = test_current_thread();
    cr_executor *executor = cr_executor_create_threaded(&error);
    threaded_root root;
    threaded_root_init(&root, THREADED_PENDING, owner);
    threaded_log log = {0};
    log.owner = owner;
    cr_executor_task *ticket = threaded_spawn(executor, &root, &log);
    wake_before_cancel_context context = {0};
    context.state = &root.state;
    test_event_init(&context.done);
    test_thread producer;
    test_thread_create(&producer, producer_wakes_before_cancel, &context);

    assert(cr_executor_wait_one(executor));
    test_event_wait(&context.done);
    test_thread_join(&producer);
    cr_executor_cancel(ticket);
    assert(root.state.drops == 1);
    assert(log.count == 1u && log.status[0] == CR_POLL_CANCELED);
    assert(cr_executor_run_ready(executor) == 0u);

    cr_executor_task_release(ticket);
    threaded_root_destroy(&root);
    test_event_destroy(&context.done);
    cr_executor_destroy(executor);
}

int main(void) {
    test_cross_thread_wake_visibility_and_coalescing();
    test_blocked_owner_is_released_by_shutdown_request();
    test_owner_operations_are_rejected_off_thread();
    test_control_block_survives_terminal_and_public_destroy();
    test_wake_before_cancel_leaves_safe_queue_record();
    return 0;
}
