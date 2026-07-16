//! Stable Waker v1 C extension ABI.

/// Waker vtable ABI version expected by Stage 5 consumers.
pub const CR_WAKER_VTABLE_ABI_VERSION: u32 = 1;

/// Returns the compiler-owned portable C11 Waker extension header.
#[must_use]
pub fn waker_header() -> &'static str {
    r#"#ifndef CR_WAKER_H
#define CR_WAKER_H

#include "cr_runtime.h"

typedef struct cr_waker_vtable {
    uint32_t abi_version;
    uint32_t struct_size;
    uint64_t provided_flags;
    void *(*clone_state)(void *state);
    void (*wake_by_ref)(void *state);
    void (*drop_state)(void *state);
} cr_waker_vtable;

struct cr_waker {
    void *state;
    const cr_waker_vtable *vtable;
};

#define CR_WAKER_VTABLE_ABI_VERSION 1u

#define CR_WAKER_VTABLE_V1_MIN_SIZE \
    (offsetof(cr_waker_vtable, drop_state) + \
     sizeof(((cr_waker_vtable *)0)->drop_state))

#define CR_WAKER_FLAG_CROSS_THREAD UINT64_C(1)

#define CR_ERROR_INVALID_WAKER_ABI  1110
#define CR_ERROR_WAKER_CLONE_FAILED 1111

static inline bool cr_waker_is_valid(const cr_waker *waker) {
    return waker != NULL &&
        waker->state != NULL &&
        waker->vtable != NULL &&
        waker->vtable->abi_version >= CR_WAKER_VTABLE_ABI_VERSION &&
        waker->vtable->struct_size >= CR_WAKER_VTABLE_V1_MIN_SIZE &&
        waker->vtable->clone_state != NULL &&
        waker->vtable->wake_by_ref != NULL &&
        waker->vtable->drop_state != NULL;
}

static inline bool cr_waker_clone(
    const cr_waker *source,
    cr_waker *out_clone
) {
    void *cloned_state;

    if (out_clone == NULL) return false;
    out_clone->state = NULL;
    out_clone->vtable = NULL;
    if (!cr_waker_is_valid(source)) return false;

    cloned_state = source->vtable->clone_state(source->state);
    if (cloned_state == NULL) return false;

    out_clone->state = cloned_state;
    out_clone->vtable = source->vtable;
    return true;
}

static inline void cr_waker_wake(const cr_waker *waker) {
    if (cr_waker_is_valid(waker)) {
        waker->vtable->wake_by_ref(waker->state);
    }
}

static inline void cr_waker_drop(cr_waker *waker) {
    if (waker == NULL) return;
    if (cr_waker_is_valid(waker)) {
        waker->vtable->drop_state(waker->state);
    }
    waker->state = NULL;
    waker->vtable = NULL;
}

#endif
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_exposes_versioned_two_word_waker_abi() {
        let header = waker_header();
        assert!(header.contains("#include \"cr_runtime.h\""));
        assert!(header.contains("typedef struct cr_waker_vtable"));
        assert!(header.contains("struct cr_waker"));
        assert!(header.contains("const cr_waker_vtable *vtable"));
        assert!(header.contains("CR_WAKER_VTABLE_V1_MIN_SIZE"));
        assert!(header.contains("CR_WAKER_FLAG_CROSS_THREAD"));
        assert!(header.contains("CR_ERROR_INVALID_WAKER_ABI"));
        assert!(header.contains("CR_ERROR_WAKER_CLONE_FAILED"));
        assert!(header.contains("static inline bool cr_waker_is_valid("));
        assert!(header.contains("static inline bool cr_waker_clone("));
        assert!(header.contains("static inline void cr_waker_wake("));
        assert!(header.contains("static inline void cr_waker_drop("));
        assert!(!header.contains("cr_executor"));
        assert_eq!(CR_WAKER_VTABLE_ABI_VERSION, 1);
    }
}
