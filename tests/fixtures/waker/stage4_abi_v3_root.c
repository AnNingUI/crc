#include "cr_runtime.h"

#include <assert.h>
#include <stdlib.h>

typedef struct stage4_abi_v3_root_task {
    int state;
    cr_poll_status status;
    int result;
    int yielded;
    cr_error error;
} stage4_abi_v3_root_task;

static cr_poll_status stage4_abi_v3_root_poll(
    stage4_abi_v3_root_task *task,
    const cr_poll_context *poll_context
) {
    assert(task != NULL);
    assert(poll_context != NULL);
    assert(poll_context->abi_version == CR_POLL_CONTEXT_ABI_VERSION);
    assert(poll_context->struct_size >= CR_POLL_CONTEXT_V1_MIN_SIZE);
    assert(poll_context->available_capabilities == CR_POLL_CAP_WAKER);
    assert(poll_context->waker != NULL);

    if (task->status == CR_POLL_READY) return CR_POLL_READY;
    if (task->state == 0) {
        task->state = 1;
        task->yielded = 17;
        task->status = CR_POLL_YIELDED;
        return CR_POLL_YIELDED;
    }
    task->result = 42;
    task->status = CR_POLL_READY;
    return CR_POLL_READY;
}

static void stage4_abi_v3_root_drop(stage4_abi_v3_root_task *task) {
    if (task == NULL) return;
    if (task->status != CR_POLL_READY && task->status != CR_POLL_ERROR) {
        task->status = CR_POLL_CANCELED;
    }
}

static void stage4_abi_v3_root_destroy(stage4_abi_v3_root_task *task) {
    if (task == NULL) return;
    stage4_abi_v3_root_drop(task);
    free(task);
}

static void stage4_abi_v3_root_await_destroy(void *state) {
    stage4_abi_v3_root_destroy((stage4_abi_v3_root_task *)state);
}

static const cr_error *stage4_abi_v3_root_error(const void *state) {
    return &((const stage4_abi_v3_root_task *)state)->error;
}

static cr_poll_status stage4_abi_v3_root_await_poll(
    void *state,
    const cr_poll_context *poll_context,
    void *out_value
) {
    stage4_abi_v3_root_task *task = (stage4_abi_v3_root_task *)state;
    cr_poll_status status;
    if (task == NULL) return CR_POLL_ERROR;
    status = stage4_abi_v3_root_poll(task, poll_context);
    if (out_value != NULL && status == CR_POLL_READY) {
        *(int *)out_value = task->result;
    }
    if (out_value != NULL && status == CR_POLL_YIELDED) {
        *(int *)out_value = task->yielded;
    }
    return status;
}

static const cr_awaitable_vtable stage4_abi_v3_root_owning_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    CR_AWAITABLE_CAN_YIELD,
    CR_POLL_CAP_WAKER,
    stage4_abi_v3_root_await_poll,
    stage4_abi_v3_root_error,
    stage4_abi_v3_root_await_destroy,
    sizeof(int),
    _Alignof(int)
};

cr_awaitable stage4_abi_v3_root_into_awaitable(void) {
    stage4_abi_v3_root_task *task =
        (stage4_abi_v3_root_task *)calloc(1u, sizeof(*task));
    assert(task != NULL);
    task->status = CR_POLL_PENDING;
    return (cr_awaitable){task, &stage4_abi_v3_root_owning_vtable};
}
