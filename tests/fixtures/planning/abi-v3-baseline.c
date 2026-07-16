#include "cr_runtime.h"

cr_awaitable external_value(int input);

typedef struct cr_child_value_task cr_child_value_task;
struct cr_child_value_task {
    uint32_t state;
    cr_poll_status status;
    cr_error error;
    cr_cleanup_stack cleanups;
    int cr_v_0_input;
    int result;
    int yielded;
};

#if CR_RUNTIME_ABI_VERSION != 3u
#error "generated CR code requires runtime ABI version 3"
#endif

void cr_child_value_init(cr_child_value_task *ctx, int input) {
    memset(ctx, 0, sizeof(*ctx));
    ctx->state = 0;
    ctx->status = CR_POLL_PENDING;
    cr_cleanup_stack_init(&ctx->cleanups);
    ctx->cr_v_0_input = input;
}

cr_poll_status cr_child_value_poll(cr_child_value_task *ctx, const cr_poll_context *poll_context) {
    if (ctx == NULL) return CR_POLL_ERROR;
    if (ctx->status == CR_POLL_READY || ctx->status == CR_POLL_ERROR || ctx->status == CR_POLL_CANCELED) return ctx->status;
    if (poll_context != NULL && (poll_context->abi_version < CR_POLL_CONTEXT_ABI_VERSION || poll_context->struct_size < CR_POLL_CONTEXT_V1_MIN_SIZE || ((poll_context->available_capabilities & CR_POLL_CAP_WAKER) != 0u && poll_context->waker == NULL))) {
        ctx->error = (cr_error){CR_ERROR_INVALID_POLL_CONTEXT, "invalid poll context"};
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
    if (ctx->status == CR_POLL_YIELDED) ctx->status = CR_POLL_PENDING;
    switch (ctx->state) {
    case 0: goto cr_b0;
    case 1: goto cr_b1;
    default:
        ctx->error = (cr_error){1001, "invalid coroutine state"};
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
cr_b0: ;
    ctx->yielded = ctx->cr_v_0_input;
    ctx->state = 1u;
    ctx->status = CR_POLL_YIELDED;
    return ctx->status;
cr_b1: ;
    ctx->result = (ctx->cr_v_0_input + 1);
    cr_cleanup_stack_destroy(&ctx->cleanups);
    ctx->status = CR_POLL_READY;
    ctx->state = UINT32_MAX;
    return ctx->status;
}

void cr_child_value_drop(cr_child_value_task *ctx) {
    if (ctx == NULL) return;
    cr_cleanup_stack_destroy(&ctx->cleanups);
    if (ctx->status != CR_POLL_READY && ctx->status != CR_POLL_ERROR) ctx->status = CR_POLL_CANCELED;
}

cr_child_value_task *cr_child_value_create(int input, cr_error *out_error) {
    cr_child_value_task *task = (cr_child_value_task *)malloc(sizeof(*task));
    if (task == NULL) {
        if (out_error != NULL) *out_error = (cr_error){1006, "async task allocation failed"};
        return NULL;
    }
    if (out_error != NULL) *out_error = (cr_error){0, NULL};
    cr_child_value_init(task, input);
    return task;
}

void cr_child_value_destroy(cr_child_value_task *task) {
    if (task == NULL) return;
    cr_child_value_drop(task);
    free(task);
}

const int *cr_child_value_result(const cr_child_value_task *task) { return &task->result; }
const int *cr_child_value_yielded(const cr_child_value_task *task) { return &task->yielded; }
const cr_error *cr_child_value_error(const cr_child_value_task *task) { return &task->error; }

static cr_poll_status cr_child_value_await_poll(void *state, const cr_poll_context *poll_context, void *out_value) {
    if (state == NULL) return CR_POLL_ERROR;
    cr_child_value_task *task = (cr_child_value_task *)state;
    cr_poll_status status = cr_child_value_poll(task, poll_context);
    if (out_value != NULL && status == CR_POLL_READY) *(int *)out_value = task->result;
    if (out_value != NULL && status == CR_POLL_YIELDED) *(int *)out_value = task->yielded;
    return status;
}

static const cr_error cr_child_value_await_null_error = {1006, "async task allocation failed"};
static const cr_error *cr_child_value_await_error(const void *state) { return state != NULL ? &((const cr_child_value_task *)state)->error : &cr_child_value_await_null_error; }
static void cr_child_value_await_drop(void *state) { cr_child_value_drop((cr_child_value_task *)state); }
static void cr_child_value_await_destroy(void *state) { cr_child_value_destroy((cr_child_value_task *)state); }
static const cr_awaitable_vtable cr_child_value_borrowed_awaitable_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    CR_AWAITABLE_CAN_YIELD,
    0u,
    cr_child_value_await_poll,
    cr_child_value_await_error,
    cr_child_value_await_drop,
    sizeof(int),
    _Alignof(int)
};

static const cr_awaitable_vtable cr_child_value_owning_awaitable_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    CR_AWAITABLE_CAN_YIELD,
    0u,
    cr_child_value_await_poll,
    cr_child_value_await_error,
    cr_child_value_await_destroy,
    sizeof(int),
    _Alignof(int)
};

cr_awaitable cr_child_value_as_awaitable(cr_child_value_task *task) {
    return (cr_awaitable){task, &cr_child_value_borrowed_awaitable_vtable};
}

cr_awaitable cr_child_value_into_awaitable(cr_child_value_task *task) {
    return (cr_awaitable){task, &cr_child_value_owning_awaitable_vtable};
}



typedef struct cr_representative_task cr_representative_task;
struct cr_representative_task {
    uint32_t state;
    cr_poll_status status;
    cr_error error;
    cr_cleanup_stack cleanups;
    cr_child_value_task cr_v_1_bound;
    bool cr_v_1_bound_active;
    uint64_t cr_v_1_bound_generation;
    int cr_v_2_first;
    int cr_v_3_second;
    int cr_v_4_third;
    int cr_v_5_input;
    cr_child_value_task cr_child_1;
    bool cr_child_1_active;
    int cr_await_0_result;
    int cr_await_1_result;
    cr_awaitable cr_await_2;
    bool cr_await_2_active;
    int cr_await_2_result;
    int result;
    int yielded;
};

#if CR_RUNTIME_ABI_VERSION != 3u
#error "generated CR code requires runtime ABI version 3"
#endif

typedef struct cr_representative_cr_v_1_bound_cleanup_payload {
    cr_child_value_task *slot;
    bool *active;
    uint64_t *generation;
    uint64_t captured_generation;
} cr_representative_cr_v_1_bound_cleanup_payload;
static void cr_representative_cr_v_1_bound_cleanup(void *raw) {
    cr_representative_cr_v_1_bound_cleanup_payload *payload = (cr_representative_cr_v_1_bound_cleanup_payload *)raw;
    if (!*payload->active || *payload->generation != payload->captured_generation) return;
    cr_child_value_drop(payload->slot);
    *payload->active = false;
}

void cr_representative_init(cr_representative_task *ctx, int input) {
    memset(ctx, 0, sizeof(*ctx));
    ctx->state = 0;
    ctx->status = CR_POLL_PENDING;
    cr_cleanup_stack_init(&ctx->cleanups);
    ctx->cr_v_5_input = input;
}

cr_poll_status cr_representative_poll(cr_representative_task *ctx, const cr_poll_context *poll_context) {
    if (ctx == NULL) return CR_POLL_ERROR;
    if (ctx->status == CR_POLL_READY || ctx->status == CR_POLL_ERROR || ctx->status == CR_POLL_CANCELED) return ctx->status;
    if (poll_context != NULL && (poll_context->abi_version < CR_POLL_CONTEXT_ABI_VERSION || poll_context->struct_size < CR_POLL_CONTEXT_V1_MIN_SIZE || ((poll_context->available_capabilities & CR_POLL_CAP_WAKER) != 0u && poll_context->waker == NULL))) {
        ctx->error = (cr_error){CR_ERROR_INVALID_POLL_CONTEXT, "invalid poll context"};
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
    if (ctx->status == CR_POLL_YIELDED) ctx->status = CR_POLL_PENDING;
    switch (ctx->state) {
    case 0: goto cr_b0;
    case 1: goto cr_b1;
    case 2: goto cr_b3;
    case 3: goto cr_b5;
    default:
        ctx->error = (cr_error){1001, "invalid coroutine state"};
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
cr_b0: ;
    if (ctx->cr_v_1_bound_active) {
        cr_child_value_drop(&ctx->cr_v_1_bound);
        ctx->cr_v_1_bound_active = false;
    }
    ctx->cr_v_1_bound_generation++;
    int cr_binding_1_arg_0 = ctx->cr_v_5_input;
    cr_child_value_init(&ctx->cr_v_1_bound, cr_binding_1_arg_0);
    ctx->cr_v_1_bound_active = true;
    cr_representative_cr_v_1_bound_cleanup_payload cr_binding_payload_1 = {&ctx->cr_v_1_bound, &ctx->cr_v_1_bound_active, &ctx->cr_v_1_bound_generation, ctx->cr_v_1_bound_generation};
    if (!cr_cleanup_push(&ctx->cleanups, 1u, cr_representative_cr_v_1_bound_cleanup, &cr_binding_payload_1, sizeof(cr_binding_payload_1))) {
        cr_representative_cr_v_1_bound_cleanup(&cr_binding_payload_1);
        ctx->error = (cr_error){1002, "cleanup allocation failed"};
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
    goto cr_b1;
cr_b1: ;
    if (!ctx->cr_v_1_bound_active) {
        ctx->error = (cr_error){1109, "inactive task binding"};
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
    ctx->state = 1u;
    cr_poll_status cr_await_0_status = cr_child_value_poll(&ctx->cr_v_1_bound, poll_context);
    if (cr_await_0_status == CR_POLL_PENDING) return cr_await_0_status;
    if (cr_await_0_status == CR_POLL_READY) {
        ctx->cr_await_0_result = ctx->cr_v_1_bound.result;
        goto cr_b2;
    }
    if (cr_await_0_status == CR_POLL_ERROR) {
        ctx->error = ctx->cr_v_1_bound.error;
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
    if (cr_await_0_status == CR_POLL_CANCELED) {
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_CANCELED;
        return ctx->status;
    }
    if (cr_await_0_status == CR_POLL_YIELDED) {
        ctx->yielded = ctx->cr_v_1_bound.yielded;
        ctx->status = CR_POLL_YIELDED;
        return ctx->status;
    }
    ctx->error = (cr_error){1106, "invalid awaitable poll status"};
    cr_cleanup_run_all(&ctx->cleanups);
    ctx->status = CR_POLL_ERROR;
    return ctx->status;
cr_b2: ;
    ctx->cr_v_2_first = ctx->cr_await_0_result;
    goto cr_b3;
cr_b3: ;
    if (!ctx->cr_child_1_active) {
        int cr_child_1_arg_0 = ctx->cr_v_2_first;
        cr_child_value_init(&ctx->cr_child_1, cr_child_1_arg_0);
        ctx->cr_child_1_active = true;
    }
    ctx->state = 2u;
    cr_poll_status cr_await_1_status = cr_child_value_poll(&ctx->cr_child_1, poll_context);
    if (cr_await_1_status == CR_POLL_PENDING) return cr_await_1_status;
    if (cr_await_1_status == CR_POLL_READY) {
        ctx->cr_await_1_result = ctx->cr_child_1.result;
        cr_child_value_drop(&ctx->cr_child_1);
        ctx->cr_child_1_active = false;
        goto cr_b4;
    }
    if (cr_await_1_status == CR_POLL_ERROR) {
        ctx->error = ctx->cr_child_1.error;
        cr_child_value_drop(&ctx->cr_child_1);
        ctx->cr_child_1_active = false;
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
    if (cr_await_1_status == CR_POLL_CANCELED) {
        cr_child_value_drop(&ctx->cr_child_1);
        ctx->cr_child_1_active = false;
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_CANCELED;
        return ctx->status;
    }
    if (cr_await_1_status == CR_POLL_YIELDED) {
        ctx->yielded = ctx->cr_child_1.yielded;
        ctx->status = CR_POLL_YIELDED;
        return ctx->status;
    }
    cr_child_value_drop(&ctx->cr_child_1);
    ctx->cr_child_1_active = false;
    ctx->error = (cr_error){1106, "invalid awaitable poll status"};
    cr_cleanup_run_all(&ctx->cleanups);
    ctx->status = CR_POLL_ERROR;
    return ctx->status;
cr_b4: ;
    ctx->cr_v_3_second = ctx->cr_await_1_result;
    goto cr_b5;
cr_b5: ;
    if (!ctx->cr_await_2_active) {
        ctx->cr_await_2 = external_value(ctx->cr_v_3_second);
        if (ctx->cr_await_2.vtable == NULL || ctx->cr_await_2.vtable->abi_version < CR_AWAITABLE_VTABLE_ABI_VERSION || ctx->cr_await_2.vtable->struct_size < CR_AWAITABLE_VTABLE_DROP_PREFIX_SIZE) {
            ctx->error = (cr_error){1102, "invalid awaitable ABI"};
            cr_cleanup_run_all(&ctx->cleanups);
            ctx->status = CR_POLL_ERROR;
            return ctx->status;
        }
        if (ctx->cr_await_2.vtable->struct_size < CR_AWAITABLE_VTABLE_V1_MIN_SIZE) {
            if (ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
            ctx->error = (cr_error){1102, "invalid awaitable ABI"};
            cr_cleanup_run_all(&ctx->cleanups);
            ctx->status = CR_POLL_ERROR;
            return ctx->status;
        }
        if (ctx->cr_await_2.vtable->poll == NULL || ctx->cr_await_2.vtable->drop == NULL) {
            if (ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
            ctx->error = (cr_error){1103, "awaitable callback missing"};
            cr_cleanup_run_all(&ctx->cleanups);
            ctx->status = CR_POLL_ERROR;
            return ctx->status;
        }
        if (ctx->cr_await_2.vtable->value_size != sizeof(int) || ctx->cr_await_2.vtable->value_align != _Alignof(int)) {
            if (ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
            ctx->error = (cr_error){1105, "awaitable value layout mismatch"};
            cr_cleanup_run_all(&ctx->cleanups);
            ctx->status = CR_POLL_ERROR;
            return ctx->status;
        }
        ctx->cr_await_2_active = true;
    }
    ctx->state = 3u;
    if ((ctx->cr_await_2.vtable->required_context_capabilities & ~CR_POLL_KNOWN_CAPABILITIES) != 0u) {
        if (ctx->cr_await_2.vtable != NULL && ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
        ctx->cr_await_2_active = false;
        ctx->error = (cr_error){1107, "unsupported poll capability"};
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
    if ((ctx->cr_await_2.vtable->required_context_capabilities & ~(poll_context != NULL ? poll_context->available_capabilities : 0u)) != 0u) {
        if (ctx->cr_await_2.vtable != NULL && ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
        ctx->cr_await_2_active = false;
        ctx->error = (cr_error){1104, "missing poll capability"};
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
    cr_poll_status cr_await_2_status = ctx->cr_await_2.vtable->poll(ctx->cr_await_2.state, poll_context, &ctx->cr_await_2_result);
    if (cr_await_2_status == CR_POLL_PENDING) return cr_await_2_status;
    if (cr_await_2_status == CR_POLL_READY) {
        if (ctx->cr_await_2.vtable != NULL && ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
        ctx->cr_await_2_active = false;
        goto cr_b6;
    }
    if (cr_await_2_status == CR_POLL_ERROR) {
        const cr_error *cr_await_2_error = ctx->cr_await_2.vtable->error != NULL ? ctx->cr_await_2.vtable->error(ctx->cr_await_2.state) : NULL;
        ctx->error = cr_await_2_error != NULL ? *cr_await_2_error : (cr_error){CR_ERROR_MISSING_CHILD_ERROR, "awaitable error without details"};
        if (ctx->cr_await_2.vtable != NULL && ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
        ctx->cr_await_2_active = false;
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_ERROR;
        return ctx->status;
    }
    if (cr_await_2_status == CR_POLL_CANCELED) {
        if (ctx->cr_await_2.vtable != NULL && ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
        ctx->cr_await_2_active = false;
        cr_cleanup_run_all(&ctx->cleanups);
        ctx->status = CR_POLL_CANCELED;
        return ctx->status;
    }
    if (cr_await_2_status == CR_POLL_YIELDED) {
        ctx->yielded = ctx->cr_await_2_result;
        ctx->status = CR_POLL_YIELDED;
        return ctx->status;
    }
    if (ctx->cr_await_2.vtable != NULL && ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
    ctx->cr_await_2_active = false;
    ctx->error = (cr_error){1106, "invalid awaitable poll status"};
    cr_cleanup_run_all(&ctx->cleanups);
    ctx->status = CR_POLL_ERROR;
    return ctx->status;
cr_b6: ;
    ctx->cr_v_4_third = ctx->cr_await_2_result;
    ctx->result = ctx->cr_v_4_third;
    cr_cleanup_stack_destroy(&ctx->cleanups);
    ctx->status = CR_POLL_READY;
    ctx->state = UINT32_MAX;
    return ctx->status;
}

void cr_representative_drop(cr_representative_task *ctx) {
    if (ctx == NULL) return;
    if (ctx->cr_child_1_active) {
        cr_child_value_drop(&ctx->cr_child_1);
        ctx->cr_child_1_active = false;
    }
    if (ctx->cr_await_2_active) {
        if (ctx->cr_await_2.vtable != NULL && ctx->cr_await_2.vtable->drop != NULL) ctx->cr_await_2.vtable->drop(ctx->cr_await_2.state);
        ctx->cr_await_2_active = false;
    }
    cr_cleanup_stack_destroy(&ctx->cleanups);
    if (ctx->status != CR_POLL_READY && ctx->status != CR_POLL_ERROR) ctx->status = CR_POLL_CANCELED;
}

cr_representative_task *cr_representative_create(int input, cr_error *out_error) {
    cr_representative_task *task = (cr_representative_task *)malloc(sizeof(*task));
    if (task == NULL) {
        if (out_error != NULL) *out_error = (cr_error){1006, "async task allocation failed"};
        return NULL;
    }
    if (out_error != NULL) *out_error = (cr_error){0, NULL};
    cr_representative_init(task, input);
    return task;
}

void cr_representative_destroy(cr_representative_task *task) {
    if (task == NULL) return;
    cr_representative_drop(task);
    free(task);
}

const int *cr_representative_result(const cr_representative_task *task) { return &task->result; }
const int *cr_representative_yielded(const cr_representative_task *task) { return &task->yielded; }
const cr_error *cr_representative_error(const cr_representative_task *task) { return &task->error; }

static cr_poll_status cr_representative_await_poll(void *state, const cr_poll_context *poll_context, void *out_value) {
    if (state == NULL) return CR_POLL_ERROR;
    cr_representative_task *task = (cr_representative_task *)state;
    cr_poll_status status = cr_representative_poll(task, poll_context);
    if (out_value != NULL && status == CR_POLL_READY) *(int *)out_value = task->result;
    if (out_value != NULL && status == CR_POLL_YIELDED) *(int *)out_value = task->yielded;
    return status;
}

static const cr_error cr_representative_await_null_error = {1006, "async task allocation failed"};
static const cr_error *cr_representative_await_error(const void *state) { return state != NULL ? &((const cr_representative_task *)state)->error : &cr_representative_await_null_error; }
static void cr_representative_await_drop(void *state) { cr_representative_drop((cr_representative_task *)state); }
static void cr_representative_await_destroy(void *state) { cr_representative_destroy((cr_representative_task *)state); }
static const cr_awaitable_vtable cr_representative_borrowed_awaitable_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    CR_AWAITABLE_CAN_YIELD,
    0u,
    cr_representative_await_poll,
    cr_representative_await_error,
    cr_representative_await_drop,
    sizeof(int),
    _Alignof(int)
};

static const cr_awaitable_vtable cr_representative_owning_awaitable_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    CR_AWAITABLE_CAN_YIELD,
    0u,
    cr_representative_await_poll,
    cr_representative_await_error,
    cr_representative_await_destroy,
    sizeof(int),
    _Alignof(int)
};

cr_awaitable cr_representative_as_awaitable(cr_representative_task *task) {
    return (cr_awaitable){task, &cr_representative_borrowed_awaitable_vtable};
}

cr_awaitable cr_representative_into_awaitable(cr_representative_task *task) {
    return (cr_awaitable){task, &cr_representative_owning_awaitable_vtable};
}


