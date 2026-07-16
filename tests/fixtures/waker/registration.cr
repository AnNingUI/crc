#include "cr_waker.h"

#include <assert.h>
#include <string.h>

typedef struct test_event test_event;

typedef struct test_waker_state {
    int originals;
    int references;
    int clone_calls;
    int wake_calls;
    int drop_calls;
    int replacement_checks;
    bool clone_returns_null;
    bool verify_replacement;
    test_event *event;
    void *expected_registration_state;
} test_waker_state;

typedef void (*test_publish_hook)(test_event *event);

struct test_event {
    bool ready;
    cr_waker registered_waker;
    cr_error error;
    int poll_calls;
    int poll_depth;
    int max_poll_depth;
    int publish_calls;
    int hook_calls;
    int signal_calls;
    int operation_drops;
    test_publish_hook before_publish_hook;
    test_publish_hook after_publish_hook;
};

static void *test_waker_clone_state(void *raw) {
    test_waker_state *state = (test_waker_state *)raw;
    state->clone_calls++;
    if (state->clone_returns_null) return NULL;
    state->references++;
    return state;
}

static void test_waker_wake_by_ref(void *raw) {
    test_waker_state *state = (test_waker_state *)raw;
    state->wake_calls++;
}

static void test_waker_drop_state(void *raw) {
    test_waker_state *state = (test_waker_state *)raw;
    assert(state->references > 0);
    if (state->verify_replacement) {
        assert(state->event != NULL);
        assert(
            state->event->registered_waker.state ==
            state->expected_registration_state
        );
        state->replacement_checks++;
        state->verify_replacement = false;
    }
    state->drop_calls++;
    state->references--;
}

static const cr_waker_vtable test_waker_vtable = {
    CR_WAKER_VTABLE_ABI_VERSION,
    sizeof(cr_waker_vtable),
    0u,
    test_waker_clone_state,
    test_waker_wake_by_ref,
    test_waker_drop_state
};

static void test_waker_state_init(test_waker_state *state) {
    memset(state, 0, sizeof(*state));
    state->originals = 1;
    state->references = 1;
}

static cr_waker test_waker_make(test_waker_state *state) {
    return (cr_waker){state, &test_waker_vtable};
}

static void test_event_init(test_event *event) {
    memset(event, 0, sizeof(*event));
}

static void test_event_signal(test_event *event) {
    event->signal_calls++;
    event->ready = true;
    if (cr_waker_is_valid(&event->registered_waker)) {
        cr_waker_wake(&event->registered_waker);
    }
}

static void test_ready_before_publication(test_event *event) {
    int polls_before = event->poll_calls;
    int depth_before = event->poll_depth;

    event->hook_calls++;
    assert(!cr_waker_is_valid(&event->registered_waker));
    test_event_signal(event);
    assert(event->poll_calls == polls_before);
    assert(event->poll_depth == depth_before);
}

static void test_ready_after_publication(test_event *event) {
    int polls_before = event->poll_calls;
    int depth_before = event->poll_depth;

    event->hook_calls++;
    assert(cr_waker_is_valid(&event->registered_waker));
    test_event_signal(event);
    assert(event->poll_calls == polls_before);
    assert(event->poll_depth == depth_before);
}

static cr_poll_status test_event_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    test_event *event = (test_event *)raw;
    cr_poll_status status;

    event->poll_calls++;
    event->poll_depth++;
    if (event->poll_depth > event->max_poll_depth) {
        event->max_poll_depth = event->poll_depth;
    }
    assert(event->poll_depth == 1);

    if (event->ready) {
        cr_waker_drop(&event->registered_waker);
        *(int *)out_value = 42;
        status = CR_POLL_READY;
        goto done;
    }

    if (poll_context == NULL ||
        !cr_waker_is_valid(poll_context->waker)) {
        event->error = (cr_error){
            CR_ERROR_INVALID_WAKER_ABI,
            "invalid Waker ABI"
        };
        status = CR_POLL_ERROR;
        goto done;
    }

    cr_waker next = {NULL, NULL};
    if (!cr_waker_clone(poll_context->waker, &next)) {
        event->error = (cr_error){
            CR_ERROR_WAKER_CLONE_FAILED,
            "Waker clone provider returned null"
        };
        status = CR_POLL_ERROR;
        goto done;
    }

    test_publish_hook before_hook = event->before_publish_hook;
    event->before_publish_hook = NULL;
    if (before_hook != NULL) before_hook(event);

    cr_waker previous = event->registered_waker;
    event->registered_waker = next;
    event->publish_calls++;

    test_publish_hook after_hook = event->after_publish_hook;
    event->after_publish_hook = NULL;
    if (after_hook != NULL) after_hook(event);

    cr_waker_drop(&previous);

    if (event->ready) {
        cr_waker_drop(&event->registered_waker);
        *(int *)out_value = 42;
        status = CR_POLL_READY;
    } else {
        assert(cr_waker_is_valid(&event->registered_waker));
        status = CR_POLL_PENDING;
    }

done:
    event->poll_depth--;
    return status;
}

static const cr_error *test_event_error(const void *raw) {
    const test_event *event = (const test_event *)raw;
    return event->error.code != 0 ? &event->error : NULL;
}

static void test_event_drop(void *raw) {
    test_event *event = (test_event *)raw;
    event->operation_drops++;
    cr_waker_drop(&event->registered_waker);
}

static const cr_awaitable_vtable test_event_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    CR_POLL_CAP_WAKER,
    test_event_poll,
    test_event_error,
    test_event_drop,
    sizeof(int),
    _Alignof(int)
};

static cr_awaitable test_event_operation(test_event *event) {
    return (cr_awaitable){event, &test_event_vtable};
}

static cr_poll_context test_poll_context(const cr_waker *waker) {
    return (cr_poll_context){
        CR_POLL_CONTEXT_ABI_VERSION,
        sizeof(cr_poll_context),
        CR_POLL_CAP_WAKER,
        waker
    };
}

__async int await_test_event(test_event *event) {
    return __await test_event_operation(event);
}

static void test_readiness_before_registration(void) {
    test_event event;
    test_waker_state waker_state;
    test_event_init(&event);
    test_waker_state_init(&waker_state);
    cr_waker waker = test_waker_make(&waker_state);
    cr_poll_context poll_context = test_poll_context(&waker);
    cr_await_test_event_task task;

    test_event_signal(&event);
    cr_await_test_event_init(&task, &event);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_READY);
    assert(*cr_await_test_event_result(&task) == 42);
    assert(event.poll_calls == 1);
    assert(event.publish_calls == 0);
    assert(event.operation_drops == 1);
    assert(waker_state.clone_calls == 0);
    assert(waker_state.wake_calls == 0);
    assert(waker_state.drop_calls == 0);
    cr_await_test_event_drop(&task);

    cr_waker_drop(&waker);
    assert(waker_state.references == 0);
    assert(waker_state.drop_calls == 1);
}

static void test_readiness_before_waker_publication(void) {
    test_event event;
    test_waker_state waker_state;
    test_event_init(&event);
    test_waker_state_init(&waker_state);
    event.before_publish_hook = test_ready_before_publication;
    cr_waker waker = test_waker_make(&waker_state);
    cr_poll_context poll_context = test_poll_context(&waker);
    cr_await_test_event_task task;

    cr_await_test_event_init(&task, &event);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_READY);
    assert(*cr_await_test_event_result(&task) == 42);
    assert(event.poll_calls == 1);
    assert(event.max_poll_depth == 1);
    assert(event.publish_calls == 1);
    assert(event.hook_calls == 1);
    assert(event.signal_calls == 1);
    assert(event.operation_drops == 1);
    assert(!cr_waker_is_valid(&event.registered_waker));
    assert(waker_state.clone_calls == 1);
    assert(waker_state.wake_calls == 0);
    assert(waker_state.drop_calls == 1);
    assert(waker_state.references == 1);
    cr_await_test_event_drop(&task);

    cr_waker_drop(&waker);
    assert(waker_state.references == 0);
    assert(waker_state.drop_calls == 2);
}

static void test_readiness_after_waker_publication(void) {
    test_event event;
    test_waker_state waker_state;
    test_event_init(&event);
    test_waker_state_init(&waker_state);
    event.after_publish_hook = test_ready_after_publication;
    cr_waker waker = test_waker_make(&waker_state);
    cr_poll_context poll_context = test_poll_context(&waker);
    cr_await_test_event_task task;

    cr_await_test_event_init(&task, &event);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_READY);
    assert(*cr_await_test_event_result(&task) == 42);
    assert(event.poll_calls == 1);
    assert(event.max_poll_depth == 1);
    assert(event.publish_calls == 1);
    assert(event.hook_calls == 1);
    assert(event.signal_calls == 1);
    assert(event.operation_drops == 1);
    assert(!cr_waker_is_valid(&event.registered_waker));
    assert(waker_state.clone_calls == 1);
    assert(waker_state.wake_calls == 1);
    assert(waker_state.drop_calls == 1);
    assert(waker_state.references == 1);
    cr_await_test_event_drop(&task);

    cr_waker_drop(&waker);
    assert(waker_state.references == 0);
    assert(waker_state.drop_calls == 2);
}

static void test_readiness_after_pending(void) {
    test_event event;
    test_waker_state waker_state;
    test_event_init(&event);
    test_waker_state_init(&waker_state);
    cr_waker waker = test_waker_make(&waker_state);
    cr_poll_context poll_context = test_poll_context(&waker);
    cr_await_test_event_task task;

    cr_await_test_event_init(&task, &event);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_PENDING);
    assert(cr_waker_is_valid(&event.registered_waker));
    assert(waker_state.clone_calls == 1);
    assert(waker_state.references == 2);

    test_event_signal(&event);
    assert(waker_state.wake_calls == 1);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_READY);
    assert(*cr_await_test_event_result(&task) == 42);
    assert(!cr_waker_is_valid(&event.registered_waker));
    assert(waker_state.drop_calls == 1);
    assert(waker_state.references == 1);
    assert(event.operation_drops == 1);
    cr_await_test_event_drop(&task);

    cr_waker_drop(&waker);
    assert(waker_state.references == 0);
    assert(waker_state.drop_calls == 2);
}

static void test_replacement_publishes_before_old_drop(void) {
    test_event event;
    test_waker_state first_state;
    test_waker_state second_state;
    test_event_init(&event);
    test_waker_state_init(&first_state);
    test_waker_state_init(&second_state);
    cr_waker first_waker = test_waker_make(&first_state);
    cr_waker second_waker = test_waker_make(&second_state);
    cr_poll_context first_context = test_poll_context(&first_waker);
    cr_poll_context second_context = test_poll_context(&second_waker);
    cr_await_test_event_task task;

    cr_await_test_event_init(&task, &event);
    assert(cr_await_test_event_poll(&task, &first_context) == CR_POLL_PENDING);
    assert(event.registered_waker.state == &first_state);

    first_state.verify_replacement = true;
    first_state.event = &event;
    first_state.expected_registration_state = &second_state;
    assert(cr_await_test_event_poll(&task, &second_context) == CR_POLL_PENDING);
    assert(event.registered_waker.state == &second_state);
    assert(first_state.clone_calls == 1);
    assert(first_state.drop_calls == 1);
    assert(first_state.replacement_checks == 1);
    assert(first_state.references == 1);
    assert(second_state.clone_calls == 1);
    assert(second_state.references == 2);

    test_event_signal(&event);
    assert(second_state.wake_calls == 1);
    assert(cr_await_test_event_poll(&task, &second_context) == CR_POLL_READY);
    assert(second_state.drop_calls == 1);
    assert(second_state.references == 1);
    assert(event.operation_drops == 1);
    cr_await_test_event_drop(&task);

    cr_waker_drop(&first_waker);
    cr_waker_drop(&second_waker);
    assert(first_state.references == 0);
    assert(second_state.references == 0);
    assert(first_state.drop_calls == 2);
    assert(second_state.drop_calls == 2);
}

static void test_duplicate_and_spurious_wake(void) {
    test_event event;
    test_waker_state waker_state;
    test_event_init(&event);
    test_waker_state_init(&waker_state);
    cr_waker waker = test_waker_make(&waker_state);
    cr_poll_context poll_context = test_poll_context(&waker);
    cr_await_test_event_task task;

    cr_await_test_event_init(&task, &event);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_PENDING);
    assert(cr_waker_is_valid(&event.registered_waker));

    int polls_before = event.poll_calls;
    cr_waker_wake(&event.registered_waker);
    assert(event.poll_calls == polls_before);
    assert(waker_state.wake_calls == 1);

    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_PENDING);
    assert(cr_waker_is_valid(&event.registered_waker));
    assert(waker_state.clone_calls == 2);
    assert(waker_state.drop_calls == 1);
    assert(waker_state.references == 2);

    test_event_signal(&event);
    test_event_signal(&event);
    assert(waker_state.wake_calls == 3);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_READY);
    assert(*cr_await_test_event_result(&task) == 42);
    assert(waker_state.drop_calls == 2);
    assert(waker_state.references == 1);
    assert(event.max_poll_depth == 1);
    assert(event.operation_drops == 1);
    cr_await_test_event_drop(&task);

    cr_waker_drop(&waker);
    assert(waker_state.references == 0);
    assert(waker_state.drop_calls == 3);
}

static void test_missing_waker_capability(void) {
    test_event event;
    test_event_init(&event);
    cr_poll_context poll_context = {
        CR_POLL_CONTEXT_ABI_VERSION,
        sizeof(cr_poll_context),
        0u,
        NULL
    };
    cr_await_test_event_task task;

    cr_await_test_event_init(&task, &event);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_ERROR);
    assert(
        cr_await_test_event_error(&task)->code ==
        CR_ERROR_MISSING_POLL_CAPABILITY
    );
    assert(event.poll_calls == 0);
    assert(event.operation_drops == 1);
    cr_await_test_event_drop(&task);
}

static void test_invalid_waker_abi(void) {
    test_event event;
    test_event_init(&event);
    cr_waker invalid_waker = {NULL, &test_waker_vtable};
    cr_poll_context poll_context = test_poll_context(&invalid_waker);
    cr_await_test_event_task task;

    cr_await_test_event_init(&task, &event);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_ERROR);
    assert(
        cr_await_test_event_error(&task)->code ==
        CR_ERROR_INVALID_WAKER_ABI
    );
    assert(event.poll_calls == 1);
    assert(event.operation_drops == 1);
    cr_await_test_event_drop(&task);
}

static void test_waker_clone_failure(void) {
    test_event event;
    test_waker_state waker_state;
    test_event_init(&event);
    test_waker_state_init(&waker_state);
    waker_state.clone_returns_null = true;
    cr_waker waker = test_waker_make(&waker_state);
    cr_poll_context poll_context = test_poll_context(&waker);
    cr_await_test_event_task task;

    cr_await_test_event_init(&task, &event);
    assert(cr_await_test_event_poll(&task, &poll_context) == CR_POLL_ERROR);
    assert(
        cr_await_test_event_error(&task)->code ==
        CR_ERROR_WAKER_CLONE_FAILED
    );
    assert(event.poll_calls == 1);
    assert(event.publish_calls == 0);
    assert(event.operation_drops == 1);
    assert(waker_state.clone_calls == 1);
    assert(waker_state.wake_calls == 0);
    assert(waker_state.drop_calls == 0);
    assert(waker_state.references == 1);
    cr_await_test_event_drop(&task);

    cr_waker_drop(&waker);
    assert(waker_state.references == 0);
    assert(waker_state.drop_calls == 1);
}

int main(void) {
    test_readiness_before_registration();
    test_readiness_before_waker_publication();
    test_readiness_after_waker_publication();
    test_readiness_after_pending();
    test_replacement_publishes_before_old_drop();
    test_duplicate_and_spurious_wake();
    test_missing_waker_capability();
    test_invalid_waker_abi();
    test_waker_clone_failure();
    return 0;
}
