//! Experimental reference executor artifacts for Stage 5.

/// Compiler-owned executor artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutorArtifact {
    pub path: &'static str,
    pub contents: &'static str,
    pub kind: &'static str,
    pub is_source: bool,
}

const EXECUTOR_HEADER: &str = r#"#ifndef CR_EXECUTOR_H
#define CR_EXECUTOR_H

#include "cr_waker.h"

#define CR_EXECUTOR_API_VERSION 1u

#define CR_EXECUTOR_ERROR_OUT_OF_MEMORY    1201
#define CR_EXECUTOR_ERROR_CLOSED           1202
#define CR_EXECUTOR_ERROR_UNSUPPORTED      1203
#define CR_EXECUTOR_ERROR_INVALID_ARGUMENT 1204
#define CR_EXECUTOR_ERROR_BACKEND_FAILURE  1205

typedef struct cr_executor cr_executor;
typedef struct cr_executor_task cr_executor_task;

typedef void (*cr_executor_observer_fn)(
    void *user,
    cr_poll_status status,
    const void *value,
    const cr_error *error
);

cr_executor *cr_executor_create_single(cr_error *out_error);
cr_executor *cr_executor_create_threaded(cr_error *out_error);

bool cr_executor_spawn(
    cr_executor *executor,
    cr_awaitable *task,
    cr_executor_observer_fn observer,
    void *user,
    cr_error *out_error,
    cr_executor_task **out_task
);

size_t cr_executor_run_ready(cr_executor *executor);
bool cr_executor_wait_one(cr_executor *executor);
void cr_executor_request_shutdown(cr_executor *executor);
void cr_executor_cancel(cr_executor_task *task);
void cr_executor_task_release(cr_executor_task *task);
void cr_executor_shutdown(cr_executor *executor);
void cr_executor_destroy(cr_executor *executor);

/*
 * Observer callbacks run on the poll owner. The value and error pointers are
 * borrowed only for the matching callback. An observer must copy data it
 * retains and must not reenter run or cancel on the same executor.
 */

#endif
"#;

const EXECUTOR_INTERNAL_HEADER: &str = r#"#ifndef CR_EXECUTOR_INTERNAL_H
#define CR_EXECUTOR_INTERNAL_H

#include "cr_executor.h"

#include <assert.h>

#ifndef CR_EXECUTOR_MALLOC
#define CR_EXECUTOR_MALLOC malloc
#endif

#ifndef CR_EXECUTOR_CALLOC
#define CR_EXECUTOR_CALLOC calloc
#endif

#ifndef CR_EXECUTOR_FREE
#define CR_EXECUTOR_FREE free
#endif

#ifndef CR_EXECUTOR_WAIT_HOOK
#define CR_EXECUTOR_WAIT_HOOK(shared) ((void)(shared))
#endif

typedef struct cr_executor_shared cr_executor_shared;
typedef struct cr_executor_backend_ops cr_executor_backend_ops;

struct cr_executor_backend_ops {
    bool cross_thread;
    bool (*is_owner)(cr_executor_shared *shared);
    void (*shared_ref)(cr_executor_shared *shared);
    void (*shared_unref)(cr_executor_shared *shared);
    void (*task_ref)(cr_executor_task *task);
    void (*task_unref)(cr_executor_task *task);
    void (*activate)(cr_executor_task *task);
    bool (*deactivate)(cr_executor_task *task);
    void (*enqueue)(cr_executor_task *task);
    cr_executor_task *(*pop_ready)(cr_executor_shared *shared);
    cr_executor_task *(*first_active)(cr_executor_shared *shared);
    bool (*is_closing)(cr_executor_shared *shared);
    bool (*is_shutdown_complete)(cr_executor_shared *shared);
    void (*request_shutdown)(cr_executor_shared *shared);
    void (*mark_shutdown_complete)(cr_executor_shared *shared);
    bool (*wait_for_ready)(cr_executor_shared *shared);
    void (*destroy_backend)(cr_executor_shared *shared);
};

struct cr_executor {
    cr_executor_shared *shared;
};

struct cr_executor_task {
    size_t references;
    cr_executor_shared *shared;
    cr_executor_task *next_ready;
    cr_executor_task *next_active;
    bool active;
    bool queued;
    bool polling;
    cr_awaitable awaitable;
    void *result_allocation;
    void *result;
    cr_executor_observer_fn observer;
    void *observer_user;
    cr_waker owned_waker;
};

struct cr_executor_shared {
    size_t references;
    bool closing;
    bool shutdown_complete;
    bool running;
    cr_executor_task *ready_head;
    cr_executor_task *ready_tail;
    cr_executor_task *active_head;
    const cr_executor_backend_ops *backend;
    void *backend_state;
};

void cr_executor_internal_set_error(
    cr_error *out_error,
    int32_t code,
    const char *message
);

cr_executor *cr_executor_internal_create(
    const cr_executor_backend_ops *backend,
    void *backend_state,
    cr_error *out_error
);

const cr_executor_backend_ops *cr_executor_internal_single_backend(void);
void cr_executor_internal_free_task(cr_executor_task *task);
void cr_executor_internal_free_shared(cr_executor_shared *shared);

#endif
"#;

const EXECUTOR_COMMON_SOURCE: &str = r#"#include "cr_executor_internal.h"

#include <stdint.h>

void cr_executor_internal_free_shared(cr_executor_shared *shared) {
    assert(shared != NULL);
    if (shared->backend->destroy_backend != NULL) {
        shared->backend->destroy_backend(shared);
    }
    CR_EXECUTOR_FREE(shared);
}

void cr_executor_internal_free_task(cr_executor_task *task) {
    assert(task != NULL);
    assert(!task->active);
    assert(!task->queued);
    assert(task->awaitable.state == NULL);
    assert(task->awaitable.vtable == NULL);
    assert(task->owned_waker.state == NULL);
    assert(task->owned_waker.vtable == NULL);
    CR_EXECUTOR_FREE(task->result_allocation);
    task->shared->backend->shared_unref(task->shared);
    CR_EXECUTOR_FREE(task);
}

static bool cr_executor_single_is_owner(cr_executor_shared *shared) {
    (void)shared;
    return true;
}

static void cr_executor_single_shared_ref(cr_executor_shared *shared) {
    assert(shared != NULL);
    assert(shared->references < SIZE_MAX);
    shared->references++;
}

static void cr_executor_single_shared_unref(cr_executor_shared *shared) {
    assert(shared != NULL);
    assert(shared->references > 0u);
    shared->references--;
    if (shared->references == 0u) {
        cr_executor_internal_free_shared(shared);
    }
}

static void cr_executor_single_task_ref(cr_executor_task *task) {
    assert(task != NULL);
    assert(task->references < SIZE_MAX);
    task->references++;
}

static void cr_executor_single_task_unref(cr_executor_task *task) {
    assert(task != NULL);
    assert(task->references > 0u);
    task->references--;
    if (task->references == 0u) cr_executor_internal_free_task(task);
}

static void cr_executor_single_activate(cr_executor_task *task) {
    cr_executor_shared *shared = task->shared;
    task->active = true;
    task->next_active = shared->active_head;
    shared->active_head = task;
}

static bool cr_executor_single_deactivate(cr_executor_task *task) {
    cr_executor_task **cursor;
    if (!task->active) return false;
    task->active = false;
    cursor = &task->shared->active_head;
    while (*cursor != NULL && *cursor != task) {
        cursor = &(*cursor)->next_active;
    }
    if (*cursor == task) *cursor = task->next_active;
    task->next_active = NULL;
    return true;
}

static void cr_executor_single_enqueue(cr_executor_task *task) {
    cr_executor_shared *shared = task->shared;
    if (!task->active || task->queued || shared->closing) return;

    cr_executor_single_task_ref(task);
    task->queued = true;
    task->next_ready = NULL;
    if (shared->ready_tail != NULL) {
        shared->ready_tail->next_ready = task;
    } else {
        shared->ready_head = task;
    }
    shared->ready_tail = task;
}

static cr_executor_task *cr_executor_single_pop_ready(
    cr_executor_shared *shared
) {
    cr_executor_task *task = shared->ready_head;
    if (task == NULL) return NULL;
    shared->ready_head = task->next_ready;
    if (shared->ready_head == NULL) shared->ready_tail = NULL;
    task->next_ready = NULL;
    assert(task->queued);
    task->queued = false;
    return task;
}

static cr_executor_task *cr_executor_single_first_active(
    cr_executor_shared *shared
) {
    return shared->active_head;
}

static bool cr_executor_single_is_closing(cr_executor_shared *shared) {
    return shared->closing;
}

static bool cr_executor_single_is_shutdown_complete(
    cr_executor_shared *shared
) {
    return shared->shutdown_complete;
}

static void cr_executor_single_request_shutdown(
    cr_executor_shared *shared
) {
    shared->closing = true;
}

static void cr_executor_single_mark_shutdown_complete(
    cr_executor_shared *shared
) {
    shared->shutdown_complete = true;
}

static void cr_executor_single_destroy_backend(
    cr_executor_shared *shared
) {
    (void)shared;
}

static const cr_executor_backend_ops cr_executor_single_backend = {
    false,
    cr_executor_single_is_owner,
    cr_executor_single_shared_ref,
    cr_executor_single_shared_unref,
    cr_executor_single_task_ref,
    cr_executor_single_task_unref,
    cr_executor_single_activate,
    cr_executor_single_deactivate,
    cr_executor_single_enqueue,
    cr_executor_single_pop_ready,
    cr_executor_single_first_active,
    cr_executor_single_is_closing,
    cr_executor_single_is_shutdown_complete,
    cr_executor_single_request_shutdown,
    cr_executor_single_mark_shutdown_complete,
    NULL,
    cr_executor_single_destroy_backend
};

const cr_executor_backend_ops *cr_executor_internal_single_backend(void) {
    return &cr_executor_single_backend;
}

static void *cr_executor_waker_clone_state(void *state) {
    cr_executor_task *task = (cr_executor_task *)state;
    task->shared->backend->task_ref(task);
    return task;
}

static void cr_executor_waker_wake_by_ref(void *state) {
    cr_executor_task *task = (cr_executor_task *)state;
    task->shared->backend->enqueue(task);
}

static void cr_executor_waker_drop_state(void *state) {
    cr_executor_task *task = (cr_executor_task *)state;
    task->shared->backend->task_unref(task);
}

static const cr_waker_vtable cr_executor_single_waker_vtable = {
    CR_WAKER_VTABLE_ABI_VERSION,
    sizeof(cr_waker_vtable),
    0u,
    cr_executor_waker_clone_state,
    cr_executor_waker_wake_by_ref,
    cr_executor_waker_drop_state
};

static const cr_waker_vtable cr_executor_threaded_waker_vtable = {
    CR_WAKER_VTABLE_ABI_VERSION,
    sizeof(cr_waker_vtable),
    CR_WAKER_FLAG_CROSS_THREAD,
    cr_executor_waker_clone_state,
    cr_executor_waker_wake_by_ref,
    cr_executor_waker_drop_state
};

void cr_executor_internal_set_error(
    cr_error *out_error,
    int32_t code,
    const char *message
) {
    if (out_error != NULL) {
        out_error->code = code;
        out_error->message = message;
    }
}

cr_executor *cr_executor_internal_create(
    const cr_executor_backend_ops *backend,
    void *backend_state,
    cr_error *out_error
) {
    cr_executor *executor;
    cr_executor_shared *shared;

    cr_executor_internal_set_error(out_error, 0, NULL);
    executor = (cr_executor *)CR_EXECUTOR_MALLOC(sizeof(*executor));
    if (executor == NULL) {
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_OUT_OF_MEMORY,
            "executor allocation failed"
        );
        return NULL;
    }
    shared = (cr_executor_shared *)CR_EXECUTOR_CALLOC(1u, sizeof(*shared));
    if (shared == NULL) {
        CR_EXECUTOR_FREE(executor);
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_OUT_OF_MEMORY,
            "executor shared-state allocation failed"
        );
        return NULL;
    }
    shared->references = 1u;
    shared->backend = backend;
    shared->backend_state = backend_state;
    executor->shared = shared;
    return executor;
}

static bool cr_executor_is_power_of_two(size_t value) {
    return value != 0u && (value & (value - 1u)) == 0u;
}

static bool cr_executor_allocate_result(
    size_t size,
    size_t alignment,
    void **out_allocation,
    void **out_result
) {
    void *allocation;
    uintptr_t address;
    size_t remainder;
    size_t offset;

    *out_allocation = NULL;
    *out_result = NULL;
    if (size == 0u) return alignment == 0u;
    if (!cr_executor_is_power_of_two(alignment)) return false;
    if (size > SIZE_MAX - (alignment - 1u)) return false;

    allocation = CR_EXECUTOR_MALLOC(size + alignment - 1u);
    if (allocation == NULL) return false;
    address = (uintptr_t)allocation;
    remainder = (size_t)(address & (uintptr_t)(alignment - 1u));
    offset = remainder == 0u ? 0u : alignment - remainder;
    *out_allocation = allocation;
    *out_result = (void *)((unsigned char *)allocation + offset);
    return true;
}

static bool cr_executor_validate_awaitable(
    const cr_awaitable *awaitable,
    cr_error *out_error
) {
    const cr_awaitable_vtable *vtable;
    if (awaitable == NULL || awaitable->state == NULL ||
        awaitable->vtable == NULL) {
        cr_executor_internal_set_error(
            out_error,
            CR_ERROR_INVALID_AWAITABLE_ABI,
            "invalid root awaitable"
        );
        return false;
    }
    vtable = awaitable->vtable;
    if (vtable->abi_version < CR_AWAITABLE_VTABLE_ABI_VERSION ||
        vtable->struct_size < CR_AWAITABLE_VTABLE_V1_MIN_SIZE) {
        cr_executor_internal_set_error(
            out_error,
            CR_ERROR_INVALID_AWAITABLE_ABI,
            "invalid root awaitable ABI"
        );
        return false;
    }
    if (vtable->poll == NULL || vtable->drop == NULL) {
        cr_executor_internal_set_error(
            out_error,
            CR_ERROR_MISSING_AWAITABLE_CALLBACK,
            "root awaitable callback missing"
        );
        return false;
    }
    if ((vtable->required_context_capabilities &
         ~CR_POLL_KNOWN_CAPABILITIES) != 0u) {
        cr_executor_internal_set_error(
            out_error,
            CR_ERROR_UNSUPPORTED_POLL_CAPABILITY,
            "unsupported root poll capability"
        );
        return false;
    }
    if ((vtable->value_size == 0u && vtable->value_align != 0u) ||
        (vtable->value_size != 0u &&
         !cr_executor_is_power_of_two(vtable->value_align)) ||
        (vtable->value_size != 0u &&
         vtable->value_size > SIZE_MAX - (vtable->value_align - 1u))) {
        cr_executor_internal_set_error(
            out_error,
            CR_ERROR_AWAITABLE_LAYOUT_MISMATCH,
            "root awaitable value layout mismatch"
        );
        return false;
    }
    return true;
}

bool cr_executor_spawn(
    cr_executor *executor,
    cr_awaitable *task_source,
    cr_executor_observer_fn observer,
    void *user,
    cr_error *out_error,
    cr_executor_task **out_task
) {
    cr_executor_task *task;
    cr_executor_shared *shared;
    void *result_allocation;
    void *result;

    cr_executor_internal_set_error(out_error, 0, NULL);
    if (out_task != NULL) *out_task = NULL;
    if (executor == NULL || executor->shared == NULL || out_task == NULL) {
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_INVALID_ARGUMENT,
            "invalid executor spawn argument"
        );
        return false;
    }
    shared = executor->shared;
    if (!shared->backend->is_owner(shared)) {
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_INVALID_ARGUMENT,
            "executor spawn must run on the poll owner"
        );
        return false;
    }
    if (shared->backend->is_closing(shared)) {
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_CLOSED,
            "executor is closed"
        );
        return false;
    }
    if (!cr_executor_validate_awaitable(task_source, out_error)) return false;
    if (!cr_executor_allocate_result(
            task_source->vtable->value_size,
            task_source->vtable->value_align,
            &result_allocation,
            &result
        )) {
        int32_t code = CR_EXECUTOR_ERROR_OUT_OF_MEMORY;
        const char *message = "root result allocation failed";
        if (task_source->vtable->value_size == 0u ||
            !cr_executor_is_power_of_two(task_source->vtable->value_align) ||
            task_source->vtable->value_size >
                SIZE_MAX - (task_source->vtable->value_align - 1u)) {
            code = CR_ERROR_AWAITABLE_LAYOUT_MISMATCH;
            message = "root awaitable value layout mismatch";
        }
        cr_executor_internal_set_error(out_error, code, message);
        return false;
    }

    task = (cr_executor_task *)CR_EXECUTOR_CALLOC(1u, sizeof(*task));
    if (task == NULL) {
        CR_EXECUTOR_FREE(result_allocation);
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_OUT_OF_MEMORY,
            "executor task allocation failed"
        );
        return false;
    }

    task->shared = shared;
    shared->backend->shared_ref(shared);
    task->references = 1u; /* Active-task reference. */
    shared->backend->task_ref(task); /* Caller ticket. */
    shared->backend->task_ref(task); /* Executor-owned Waker. */
    task->awaitable = *task_source;
    task->result_allocation = result_allocation;
    task->result = result;
    task->observer = observer;
    task->observer_user = user;
    task->owned_waker = (cr_waker){
        task,
        shared->backend->cross_thread
            ? &cr_executor_threaded_waker_vtable
            : &cr_executor_single_waker_vtable
    };
    shared->backend->activate(task);

    task_source->state = NULL;
    task_source->vtable = NULL;
    shared->backend->enqueue(task);
    *out_task = task;
    return true;
}

static void cr_executor_notify(
    cr_executor_task *task,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    if (task->observer != NULL) {
        task->observer(task->observer_user, status, value, error);
    }
}

static void cr_executor_finish(
    cr_executor_task *task,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    if (!task->shared->backend->deactivate(task)) return;
    cr_executor_notify(task, status, value, error);

    if (task->awaitable.vtable != NULL &&
        task->awaitable.vtable->drop != NULL) {
        task->awaitable.vtable->drop(task->awaitable.state);
    }
    task->awaitable.state = NULL;
    task->awaitable.vtable = NULL;
    CR_EXECUTOR_FREE(task->result_allocation);
    task->result_allocation = NULL;
    task->result = NULL;
    cr_waker_drop(&task->owned_waker);
    task->shared->backend->task_unref(task);
}

static void cr_executor_internal_shutdown(cr_executor_shared *shared) {
    cr_executor_task *queued;
    cr_executor_task *active;
    if (shared->backend->is_shutdown_complete(shared)) return;
    shared->backend->request_shutdown(shared);
    while ((active = shared->backend->first_active(shared)) != NULL) {
        cr_executor_finish(
            active,
            CR_POLL_CANCELED,
            NULL,
            NULL
        );
    }
    while ((queued = shared->backend->pop_ready(shared)) != NULL) {
        shared->backend->task_unref(queued);
    }
    shared->backend->mark_shutdown_complete(shared);
}

static bool cr_executor_poll_one(cr_executor_shared *shared) {
    cr_executor_task *task;

    for (;;) {
        cr_poll_context poll_context;
        cr_poll_status status;
        const cr_error *task_error;
        cr_error synthetic_error;

        task = shared->backend->pop_ready(shared);
        if (task == NULL) return false;
        if (!task->active) {
            shared->backend->task_unref(task);
            continue;
        }
        shared->backend->task_unref(task);

        poll_context = (cr_poll_context){
            CR_POLL_CONTEXT_ABI_VERSION,
            sizeof(cr_poll_context),
            CR_POLL_CAP_WAKER,
            &task->owned_waker
        };
        task->polling = true;
        status = task->awaitable.vtable->poll(
            task->awaitable.state,
            &poll_context,
            task->result
        );
        task->polling = false;

        if (status == CR_POLL_PENDING) return true;
        if (status == CR_POLL_YIELDED) {
            cr_executor_notify(task, status, task->result, NULL);
            shared->backend->enqueue(task);
            return true;
        }
        if (status == CR_POLL_READY) {
            cr_executor_finish(task, status, task->result, NULL);
            return true;
        }
        if (status == CR_POLL_ERROR) {
            task_error = task->awaitable.vtable->error != NULL
                ? task->awaitable.vtable->error(task->awaitable.state)
                : NULL;
            if (task_error == NULL) {
                synthetic_error = (cr_error){
                    CR_ERROR_MISSING_CHILD_ERROR,
                    "root awaitable error without details"
                };
                task_error = &synthetic_error;
            }
            cr_executor_finish(task, status, NULL, task_error);
            return true;
        }
        if (status == CR_POLL_CANCELED) {
            cr_executor_finish(task, status, NULL, NULL);
            return true;
        }

        synthetic_error = (cr_error){
            CR_ERROR_INVALID_POLL_STATUS,
            "root awaitable returned an invalid poll status"
        };
        cr_executor_finish(task, CR_POLL_ERROR, NULL, &synthetic_error);
        return true;
    }
}

size_t cr_executor_run_ready(cr_executor *executor) {
    cr_executor_shared *shared;
    size_t polls = 0u;

    if (executor == NULL || executor->shared == NULL) return 0u;
    shared = executor->shared;
    if (!shared->backend->is_owner(shared) || shared->running) return 0u;
    shared->running = true;
    while (!shared->backend->is_closing(shared) &&
           cr_executor_poll_one(shared)) {
        polls++;
    }
    if (shared->backend->is_closing(shared)) {
        cr_executor_internal_shutdown(shared);
    }
    shared->running = false;
    return polls;
}

bool cr_executor_wait_one(cr_executor *executor) {
    cr_executor_shared *shared;
    bool polled = false;
    if (executor == NULL || executor->shared == NULL) return false;
    shared = executor->shared;
    if (!shared->backend->is_owner(shared) || shared->running ||
        shared->backend->wait_for_ready == NULL) {
        return false;
    }

    shared->running = true;
    for (;;) {
        if (shared->backend->is_closing(shared)) break;
        if (!shared->backend->wait_for_ready(shared)) break;
        if (cr_executor_poll_one(shared)) {
            polled = true;
            break;
        }
    }
    if (shared->backend->is_closing(shared)) {
        cr_executor_internal_shutdown(shared);
        polled = false;
    }
    shared->running = false;
    return polled;
}

void cr_executor_request_shutdown(cr_executor *executor) {
    if (executor != NULL && executor->shared != NULL) {
        executor->shared->backend->request_shutdown(executor->shared);
    }
}

void cr_executor_cancel(cr_executor_task *task) {
    if (task == NULL ||
        !task->shared->backend->is_owner(task->shared) ||
        task->polling || task->shared->running) {
        return;
    }
    cr_executor_finish(task, CR_POLL_CANCELED, NULL, NULL);
}

void cr_executor_task_release(cr_executor_task *task) {
    if (task != NULL && task->shared->backend->is_owner(task->shared)) {
        task->shared->backend->task_unref(task);
    }
}

void cr_executor_shutdown(cr_executor *executor) {
    if (executor == NULL || executor->shared == NULL ||
        !executor->shared->backend->is_owner(executor->shared) ||
        executor->shared->running) {
        return;
    }
    cr_executor_internal_shutdown(executor->shared);
}

void cr_executor_destroy(cr_executor *executor) {
    cr_executor_shared *shared;
    if (executor == NULL) return;
    shared = executor->shared;
    if (shared != NULL) {
        if (!shared->backend->is_owner(shared)) return;
        assert(!shared->running);
        cr_executor_internal_shutdown(shared);
        shared->backend->shared_unref(shared);
    }
    executor->shared = NULL;
    CR_EXECUTOR_FREE(executor);
}
"#;

const EXECUTOR_SINGLE_SOURCE: &str = r#"#include "cr_executor_internal.h"

cr_executor *cr_executor_create_single(cr_error *out_error) {
    return cr_executor_internal_create(
        cr_executor_internal_single_backend(),
        NULL,
        out_error
    );
}
"#;

const EXECUTOR_THREADED_STUB_SOURCE: &str = r#"#include "cr_executor_internal.h"

cr_executor *cr_executor_create_threaded(cr_error *out_error) {
    cr_executor_internal_set_error(
        out_error,
        CR_EXECUTOR_ERROR_UNSUPPORTED,
        "threaded executor is unsupported by this runtime module"
    );
    return NULL;
}
"#;

const EXECUTOR_THREADED_WINDOWS_SOURCE: &str = r#"#include "cr_executor_internal.h"

#ifndef _WIN32_WINNT
#define _WIN32_WINNT 0x0600
#endif
#define WIN32_LEAN_AND_MEAN
#include <windows.h>

typedef struct cr_executor_windows_backend {
    CRITICAL_SECTION lock;
    CONDITION_VARIABLE condition;
    DWORD owner;
} cr_executor_windows_backend;

static cr_executor_windows_backend *win_backend(
    cr_executor_shared *shared
) {
    return (cr_executor_windows_backend *)shared->backend_state;
}

static void win_lock(cr_executor_shared *shared) {
    EnterCriticalSection(&win_backend(shared)->lock);
}

static void win_unlock(cr_executor_shared *shared) {
    LeaveCriticalSection(&win_backend(shared)->lock);
}

static bool win_is_owner(cr_executor_shared *shared) {
    return GetCurrentThreadId() == win_backend(shared)->owner;
}

static void win_shared_ref(cr_executor_shared *shared) {
#if defined(_WIN64)
    InterlockedIncrement64((volatile LONG64 *)&shared->references);
#else
    InterlockedIncrement((volatile LONG *)&shared->references);
#endif
}

static void win_shared_unref(cr_executor_shared *shared) {
#if defined(_WIN64)
    if (InterlockedDecrement64((volatile LONG64 *)&shared->references) == 0) {
#else
    if (InterlockedDecrement((volatile LONG *)&shared->references) == 0) {
#endif
        cr_executor_internal_free_shared(shared);
    }
}

static void win_task_ref(cr_executor_task *task) {
#if defined(_WIN64)
    InterlockedIncrement64((volatile LONG64 *)&task->references);
#else
    InterlockedIncrement((volatile LONG *)&task->references);
#endif
}

static void win_task_unref(cr_executor_task *task) {
#if defined(_WIN64)
    if (InterlockedDecrement64((volatile LONG64 *)&task->references) == 0) {
#else
    if (InterlockedDecrement((volatile LONG *)&task->references) == 0) {
#endif
        cr_executor_internal_free_task(task);
    }
}

static void win_activate(cr_executor_task *task) {
    cr_executor_shared *shared = task->shared;
    win_lock(shared);
    task->active = true;
    task->next_active = shared->active_head;
    shared->active_head = task;
    win_unlock(shared);
}

static bool win_deactivate(cr_executor_task *task) {
    cr_executor_shared *shared = task->shared;
    cr_executor_task **cursor;
    bool active;
    win_lock(shared);
    active = task->active;
    if (active) {
        task->active = false;
        cursor = &shared->active_head;
        while (*cursor != NULL && *cursor != task) {
            cursor = &(*cursor)->next_active;
        }
        if (*cursor == task) *cursor = task->next_active;
        task->next_active = NULL;
    }
    win_unlock(shared);
    return active;
}

static void win_enqueue(cr_executor_task *task) {
    cr_executor_shared *shared = task->shared;
    win_lock(shared);
    if (task->active && !task->queued && !shared->closing) {
        win_task_ref(task);
        task->queued = true;
        task->next_ready = NULL;
        if (shared->ready_tail != NULL) {
            shared->ready_tail->next_ready = task;
        } else {
            shared->ready_head = task;
        }
        shared->ready_tail = task;
        WakeConditionVariable(&win_backend(shared)->condition);
    }
    win_unlock(shared);
}

static cr_executor_task *win_pop_ready(cr_executor_shared *shared) {
    cr_executor_task *task;
    win_lock(shared);
    task = shared->ready_head;
    if (task != NULL) {
        shared->ready_head = task->next_ready;
        if (shared->ready_head == NULL) shared->ready_tail = NULL;
        task->next_ready = NULL;
        task->queued = false;
    }
    win_unlock(shared);
    return task;
}

static cr_executor_task *win_first_active(cr_executor_shared *shared) {
    cr_executor_task *task;
    win_lock(shared);
    task = shared->active_head;
    win_unlock(shared);
    return task;
}

static bool win_is_closing(cr_executor_shared *shared) {
    bool closing;
    win_lock(shared);
    closing = shared->closing;
    win_unlock(shared);
    return closing;
}

static bool win_is_shutdown_complete(cr_executor_shared *shared) {
    bool complete;
    win_lock(shared);
    complete = shared->shutdown_complete;
    win_unlock(shared);
    return complete;
}

static void win_request_shutdown(cr_executor_shared *shared) {
    win_lock(shared);
    shared->closing = true;
    WakeAllConditionVariable(&win_backend(shared)->condition);
    win_unlock(shared);
}

static void win_mark_shutdown_complete(cr_executor_shared *shared) {
    win_lock(shared);
    shared->shutdown_complete = true;
    WakeAllConditionVariable(&win_backend(shared)->condition);
    win_unlock(shared);
}

static bool win_wait_for_ready(cr_executor_shared *shared) {
    cr_executor_windows_backend *backend = win_backend(shared);
    bool ready;
    win_lock(shared);
    while (shared->ready_head == NULL && !shared->closing) {
        CR_EXECUTOR_WAIT_HOOK(shared);
        if (!SleepConditionVariableCS(&backend->condition, &backend->lock, INFINITE)) {
            break;
        }
    }
    ready = !shared->closing && shared->ready_head != NULL;
    win_unlock(shared);
    return ready;
}

static void win_destroy_backend(cr_executor_shared *shared) {
    cr_executor_windows_backend *backend = win_backend(shared);
    DeleteCriticalSection(&backend->lock);
    CR_EXECUTOR_FREE(backend);
}

static const cr_executor_backend_ops win_backend_ops = {
    true,
    win_is_owner,
    win_shared_ref,
    win_shared_unref,
    win_task_ref,
    win_task_unref,
    win_activate,
    win_deactivate,
    win_enqueue,
    win_pop_ready,
    win_first_active,
    win_is_closing,
    win_is_shutdown_complete,
    win_request_shutdown,
    win_mark_shutdown_complete,
    win_wait_for_ready,
    win_destroy_backend
};

cr_executor *cr_executor_create_threaded(cr_error *out_error) {
    cr_executor_windows_backend *backend =
        (cr_executor_windows_backend *)CR_EXECUTOR_CALLOC(1u, sizeof(*backend));
    cr_executor *executor;
    if (backend == NULL) {
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_OUT_OF_MEMORY,
            "threaded backend allocation failed"
        );
        return NULL;
    }
    InitializeCriticalSection(&backend->lock);
    InitializeConditionVariable(&backend->condition);
    backend->owner = GetCurrentThreadId();
    executor = cr_executor_internal_create(&win_backend_ops, backend, out_error);
    if (executor == NULL) {
        DeleteCriticalSection(&backend->lock);
        CR_EXECUTOR_FREE(backend);
    }
    return executor;
}
"#;

const EXECUTOR_THREADED_POSIX_SOURCE: &str = r#"#include "cr_executor_internal.h"

#include <pthread.h>

typedef struct cr_executor_posix_backend {
    pthread_mutex_t lock;
    pthread_cond_t condition;
    pthread_t owner;
} cr_executor_posix_backend;

static cr_executor_posix_backend *posix_backend(
    cr_executor_shared *shared
) {
    return (cr_executor_posix_backend *)shared->backend_state;
}

static void posix_lock(cr_executor_shared *shared) {
    (void)pthread_mutex_lock(&posix_backend(shared)->lock);
}

static void posix_unlock(cr_executor_shared *shared) {
    (void)pthread_mutex_unlock(&posix_backend(shared)->lock);
}

static bool posix_is_owner(cr_executor_shared *shared) {
    return pthread_equal(pthread_self(), posix_backend(shared)->owner) != 0;
}

static void posix_shared_ref(cr_executor_shared *shared) {
    (void)__atomic_add_fetch(&shared->references, 1u, __ATOMIC_ACQ_REL);
}

static void posix_shared_unref(cr_executor_shared *shared) {
    if (__atomic_sub_fetch(&shared->references, 1u, __ATOMIC_ACQ_REL) == 0u) {
        cr_executor_internal_free_shared(shared);
    }
}

static void posix_task_ref(cr_executor_task *task) {
    (void)__atomic_add_fetch(&task->references, 1u, __ATOMIC_ACQ_REL);
}

static void posix_task_unref(cr_executor_task *task) {
    if (__atomic_sub_fetch(&task->references, 1u, __ATOMIC_ACQ_REL) == 0u) {
        cr_executor_internal_free_task(task);
    }
}

static void posix_activate(cr_executor_task *task) {
    cr_executor_shared *shared = task->shared;
    posix_lock(shared);
    task->active = true;
    task->next_active = shared->active_head;
    shared->active_head = task;
    posix_unlock(shared);
}

static bool posix_deactivate(cr_executor_task *task) {
    cr_executor_shared *shared = task->shared;
    cr_executor_task **cursor;
    bool active;
    posix_lock(shared);
    active = task->active;
    if (active) {
        task->active = false;
        cursor = &shared->active_head;
        while (*cursor != NULL && *cursor != task) {
            cursor = &(*cursor)->next_active;
        }
        if (*cursor == task) *cursor = task->next_active;
        task->next_active = NULL;
    }
    posix_unlock(shared);
    return active;
}

static void posix_enqueue(cr_executor_task *task) {
    cr_executor_shared *shared = task->shared;
    posix_lock(shared);
    if (task->active && !task->queued && !shared->closing) {
        posix_task_ref(task);
        task->queued = true;
        task->next_ready = NULL;
        if (shared->ready_tail != NULL) {
            shared->ready_tail->next_ready = task;
        } else {
            shared->ready_head = task;
        }
        shared->ready_tail = task;
        (void)pthread_cond_signal(&posix_backend(shared)->condition);
    }
    posix_unlock(shared);
}

static cr_executor_task *posix_pop_ready(cr_executor_shared *shared) {
    cr_executor_task *task;
    posix_lock(shared);
    task = shared->ready_head;
    if (task != NULL) {
        shared->ready_head = task->next_ready;
        if (shared->ready_head == NULL) shared->ready_tail = NULL;
        task->next_ready = NULL;
        task->queued = false;
    }
    posix_unlock(shared);
    return task;
}

static cr_executor_task *posix_first_active(cr_executor_shared *shared) {
    cr_executor_task *task;
    posix_lock(shared);
    task = shared->active_head;
    posix_unlock(shared);
    return task;
}

static bool posix_is_closing(cr_executor_shared *shared) {
    bool closing;
    posix_lock(shared);
    closing = shared->closing;
    posix_unlock(shared);
    return closing;
}

static bool posix_is_shutdown_complete(cr_executor_shared *shared) {
    bool complete;
    posix_lock(shared);
    complete = shared->shutdown_complete;
    posix_unlock(shared);
    return complete;
}

static void posix_request_shutdown(cr_executor_shared *shared) {
    posix_lock(shared);
    shared->closing = true;
    (void)pthread_cond_broadcast(&posix_backend(shared)->condition);
    posix_unlock(shared);
}

static void posix_mark_shutdown_complete(cr_executor_shared *shared) {
    posix_lock(shared);
    shared->shutdown_complete = true;
    (void)pthread_cond_broadcast(&posix_backend(shared)->condition);
    posix_unlock(shared);
}

static bool posix_wait_for_ready(cr_executor_shared *shared) {
    cr_executor_posix_backend *backend = posix_backend(shared);
    bool ready;
    posix_lock(shared);
    while (shared->ready_head == NULL && !shared->closing) {
        CR_EXECUTOR_WAIT_HOOK(shared);
        if (pthread_cond_wait(&backend->condition, &backend->lock) != 0) {
            break;
        }
    }
    ready = !shared->closing && shared->ready_head != NULL;
    posix_unlock(shared);
    return ready;
}

static void posix_destroy_backend(cr_executor_shared *shared) {
    cr_executor_posix_backend *backend = posix_backend(shared);
    (void)pthread_cond_destroy(&backend->condition);
    (void)pthread_mutex_destroy(&backend->lock);
    CR_EXECUTOR_FREE(backend);
}

static const cr_executor_backend_ops posix_backend_ops = {
    true,
    posix_is_owner,
    posix_shared_ref,
    posix_shared_unref,
    posix_task_ref,
    posix_task_unref,
    posix_activate,
    posix_deactivate,
    posix_enqueue,
    posix_pop_ready,
    posix_first_active,
    posix_is_closing,
    posix_is_shutdown_complete,
    posix_request_shutdown,
    posix_mark_shutdown_complete,
    posix_wait_for_ready,
    posix_destroy_backend
};

cr_executor *cr_executor_create_threaded(cr_error *out_error) {
    cr_executor_posix_backend *backend =
        (cr_executor_posix_backend *)CR_EXECUTOR_CALLOC(1u, sizeof(*backend));
    cr_executor *executor;
    if (backend == NULL) {
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_OUT_OF_MEMORY,
            "threaded backend allocation failed"
        );
        return NULL;
    }
    if (pthread_mutex_init(&backend->lock, NULL) != 0) {
        CR_EXECUTOR_FREE(backend);
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_BACKEND_FAILURE,
            "threaded backend synchronization initialization failed"
        );
        return NULL;
    }
    if (pthread_cond_init(&backend->condition, NULL) != 0) {
        (void)pthread_mutex_destroy(&backend->lock);
        CR_EXECUTOR_FREE(backend);
        cr_executor_internal_set_error(
            out_error,
            CR_EXECUTOR_ERROR_BACKEND_FAILURE,
            "threaded backend condition initialization failed"
        );
        return NULL;
    }
    backend->owner = pthread_self();
    executor = cr_executor_internal_create(&posix_backend_ops, backend, out_error);
    if (executor == NULL) {
        (void)pthread_cond_destroy(&backend->condition);
        (void)pthread_mutex_destroy(&backend->lock);
        CR_EXECUTOR_FREE(backend);
    }
    return executor;
}
"#;

const PORTABLE_BASE_ARTIFACTS: [ExecutorArtifact; 4] = [
    ExecutorArtifact {
        path: "include/cr_executor.h",
        contents: EXECUTOR_HEADER,
        kind: "executor-header",
        is_source: false,
    },
    ExecutorArtifact {
        path: "runtime/cr_executor_internal.h",
        contents: EXECUTOR_INTERNAL_HEADER,
        kind: "executor-internal",
        is_source: false,
    },
    ExecutorArtifact {
        path: "runtime/cr_executor_common.c",
        contents: EXECUTOR_COMMON_SOURCE,
        kind: "executor-source",
        is_source: true,
    },
    ExecutorArtifact {
        path: "runtime/cr_executor_single.c",
        contents: EXECUTOR_SINGLE_SOURCE,
        kind: "executor-source",
        is_source: true,
    },
];

const THREADED_STUB_ARTIFACT: ExecutorArtifact = ExecutorArtifact {
    path: "runtime/cr_executor_threaded_stub.c",
    contents: EXECUTOR_THREADED_STUB_SOURCE,
    kind: "executor-source",
    is_source: true,
};

const THREADED_WINDOWS_ARTIFACT: ExecutorArtifact = ExecutorArtifact {
    path: "runtime/cr_executor_threaded_windows.c",
    contents: EXECUTOR_THREADED_WINDOWS_SOURCE,
    kind: "executor-source",
    is_source: true,
};

const THREADED_POSIX_ARTIFACT: ExecutorArtifact = ExecutorArtifact {
    path: "runtime/cr_executor_threaded_posix.c",
    contents: EXECUTOR_THREADED_POSIX_SOURCE,
    kind: "executor-source",
    is_source: true,
};

const PORTABLE_ARTIFACTS: [ExecutorArtifact; 5] = [
    PORTABLE_BASE_ARTIFACTS[0],
    PORTABLE_BASE_ARTIFACTS[1],
    PORTABLE_BASE_ARTIFACTS[2],
    PORTABLE_BASE_ARTIFACTS[3],
    THREADED_STUB_ARTIFACT,
];

/// Returns the complete portable reference executor artifact set.
#[must_use]
pub fn portable_artifacts() -> &'static [ExecutorArtifact] {
    &PORTABLE_ARTIFACTS
}

/// Returns the portable base plus the target-appropriate native backend.
#[must_use]
pub fn native_threaded_artifacts(target: &crate::config::TargetConfig) -> Vec<ExecutorArtifact> {
    let backend = match target {
        crate::config::TargetConfig::WindowsMsvc | crate::config::TargetConfig::WindowsGnu => {
            THREADED_WINDOWS_ARTIFACT
        }
        crate::config::TargetConfig::LinuxGnu
        | crate::config::TargetConfig::LinuxMusl
        | crate::config::TargetConfig::Macos => THREADED_POSIX_ARTIFACT,
        crate::config::TargetConfig::Host if cfg!(windows) => THREADED_WINDOWS_ARTIFACT,
        crate::config::TargetConfig::Host if cfg!(unix) => THREADED_POSIX_ARTIFACT,
        crate::config::TargetConfig::Host => THREADED_STUB_ARTIFACT,
        crate::config::TargetConfig::Wasm32Wasi | crate::config::TargetConfig::Custom(_) => {
            THREADED_STUB_ARTIFACT
        }
    };
    let mut artifacts = PORTABLE_BASE_ARTIFACTS.to_vec();
    artifacts.push(backend);
    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_artifact_set_is_complete_and_thread_free() {
        let paths = portable_artifacts()
            .iter()
            .map(|artifact| artifact.path)
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            vec![
                "include/cr_executor.h",
                "runtime/cr_executor_internal.h",
                "runtime/cr_executor_common.c",
                "runtime/cr_executor_single.c",
                "runtime/cr_executor_threaded_stub.c",
            ]
        );
        let portable_sources = portable_artifacts()
            .iter()
            .filter(|artifact| artifact.is_source)
            .map(|artifact| artifact.contents)
            .collect::<String>()
            .to_ascii_lowercase();
        for forbidden in [
            "stdatomic",
            "pthread_",
            "createthread",
            "critical_section",
            "condition_variable",
        ] {
            assert!(!portable_sources.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn public_header_keeps_executor_layout_opaque() {
        assert!(EXECUTOR_HEADER.contains("typedef struct cr_executor cr_executor;"));
        assert!(EXECUTOR_HEADER.contains("cr_executor_spawn("));
        assert!(EXECUTOR_HEADER.contains("cr_executor_run_ready("));
        assert!(EXECUTOR_HEADER.contains("borrowed only for the matching callback"));
        assert!(!EXECUTOR_HEADER.contains("struct cr_executor {"));
    }
}
