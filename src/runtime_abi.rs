//! Versioned C runtime ABI shared by generated code and project templates.

/// Runtime ABI version expected by generated C.
pub const CR_RUNTIME_ABI_VERSION: u32 = 3;

/// Returns the compiler-owned C11 runtime header.
#[must_use]
pub fn runtime_header() -> &'static str {
    r#"#ifndef CR_RUNTIME_H
#define CR_RUNTIME_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#define CR_RUNTIME_ABI_VERSION 3u

typedef uint32_t cr_poll_status;

#define CR_POLL_PENDING  0u
#define CR_POLL_YIELDED  1u
#define CR_POLL_READY    2u
#define CR_POLL_ERROR    3u
#define CR_POLL_CANCELED 4u

typedef struct cr_error {
    int32_t code;
    const char *message;
} cr_error;

typedef struct cr_waker cr_waker;

typedef struct cr_poll_context {
    uint32_t abi_version;
    uint32_t struct_size;
    uint64_t available_capabilities;
    const cr_waker *waker;
} cr_poll_context;

typedef struct cr_awaitable_vtable {
    uint32_t abi_version;
    uint32_t struct_size;
    uint64_t provided_flags;
    uint64_t required_context_capabilities;
    cr_poll_status (*poll)(
        void *state,
        const cr_poll_context *poll_context,
        void *out_value
    );
    const cr_error *(*error)(const void *state);
    void (*drop)(void *state);
    size_t value_size;
    size_t value_align;
} cr_awaitable_vtable;

typedef struct cr_awaitable {
    void *state;
    const cr_awaitable_vtable *vtable;
} cr_awaitable;

#define CR_POLL_CONTEXT_ABI_VERSION 1u
#define CR_AWAITABLE_VTABLE_ABI_VERSION 1u

#define CR_POLL_CONTEXT_V1_MIN_SIZE \
    (offsetof(cr_poll_context, waker) + \
     sizeof(((cr_poll_context *)0)->waker))

#define CR_AWAITABLE_VTABLE_V1_MIN_SIZE \
    (offsetof(cr_awaitable_vtable, value_align) + \
     sizeof(((cr_awaitable_vtable *)0)->value_align))

#define CR_AWAITABLE_VTABLE_DROP_PREFIX_SIZE \
    (offsetof(cr_awaitable_vtable, drop) + \
     sizeof(((cr_awaitable_vtable *)0)->drop))

#define CR_AWAITABLE_CAN_YIELD UINT64_C(1)
#define CR_POLL_CAP_WAKER UINT64_C(1)
#define CR_POLL_KNOWN_CAPABILITIES CR_POLL_CAP_WAKER

#define CR_ERROR_INVALID_POLL_CONTEXT        1101
#define CR_ERROR_INVALID_AWAITABLE_ABI       1102
#define CR_ERROR_MISSING_AWAITABLE_CALLBACK  1103
#define CR_ERROR_MISSING_POLL_CAPABILITY     1104
#define CR_ERROR_AWAITABLE_LAYOUT_MISMATCH   1105
#define CR_ERROR_INVALID_POLL_STATUS         1106
#define CR_ERROR_UNSUPPORTED_POLL_CAPABILITY 1107
#define CR_ERROR_MISSING_CHILD_ERROR          1108
#define CR_ERROR_INACTIVE_TASK_BINDING        1109

typedef void (*cr_cleanup_fn)(void *payload);

typedef struct cr_cleanup_entry {
    uint32_t scope;
    cr_cleanup_fn run;
    void *payload;
} cr_cleanup_entry;

typedef struct cr_cleanup_stack {
    cr_cleanup_entry *entries;
    size_t length;
    size_t capacity;
} cr_cleanup_stack;

static inline void cr_cleanup_stack_init(cr_cleanup_stack *stack) {
    stack->entries = NULL;
    stack->length = 0;
    stack->capacity = 0;
}

static inline bool cr_cleanup_push(
    cr_cleanup_stack *stack,
    uint32_t scope,
    cr_cleanup_fn run,
    const void *payload,
    size_t payload_size
) {
    void *copy = NULL;
    if (payload_size != 0) {
        copy = malloc(payload_size);
        if (copy == NULL) {
            return false;
        }
        memcpy(copy, payload, payload_size);
    }
    if (stack->length == stack->capacity) {
        size_t next_capacity = stack->capacity == 0 ? 4 : stack->capacity * 2;
        cr_cleanup_entry *next = (cr_cleanup_entry *)realloc(
            stack->entries,
            next_capacity * sizeof(*next)
        );
        if (next == NULL) {
            free(copy);
            return false;
        }
        stack->entries = next;
        stack->capacity = next_capacity;
    }
    stack->entries[stack->length++] = (cr_cleanup_entry){scope, run, copy};
    return true;
}

static inline void cr_cleanup_run_scope(
    cr_cleanup_stack *stack,
    uint32_t scope
) {
    while (stack->length != 0) {
        cr_cleanup_entry *entry = &stack->entries[stack->length - 1];
        if (entry->scope != scope) {
            break;
        }
        entry->run(entry->payload);
        free(entry->payload);
        stack->length--;
    }
}

static inline void cr_cleanup_run_all(cr_cleanup_stack *stack) {
    while (stack->length != 0) {
        cr_cleanup_entry *entry = &stack->entries[stack->length - 1];
        entry->run(entry->payload);
        free(entry->payload);
        stack->length--;
    }
}

static inline void cr_cleanup_stack_destroy(cr_cleanup_stack *stack) {
    cr_cleanup_run_all(stack);
    free(stack->entries);
    stack->entries = NULL;
    stack->capacity = 0;
}

static inline void cr_oom_abort(void) {
    abort();
}

#endif
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_exposes_versioned_poll_and_cleanup_abi() {
        let header = runtime_header();
        assert!(header.contains("CR_RUNTIME_ABI_VERSION 3u"));
        assert!(header.contains("typedef uint32_t cr_poll_status"));
        assert!(header.contains("typedef struct cr_poll_context"));
        assert!(header.contains("typedef struct cr_awaitable_vtable"));
        assert!(header.contains("const cr_awaitable_vtable *vtable"));
        assert!(header.contains("CR_AWAITABLE_VTABLE_V1_MIN_SIZE"));
        assert!(header.contains("CR_ERROR_INACTIVE_TASK_BINDING"));
        assert!(!header.contains("CR_AWAITABLE_OWNS_STATE"));
        assert!(header.contains("cr_cleanup_run_scope"));
        assert!(header.contains("cr_oom_abort"));
        assert_eq!(CR_RUNTIME_ABI_VERSION, 3);
    }
}
