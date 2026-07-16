# CR core ABI v3 Stage 2 design

This document defines the exact Stage 2 migration from Runtime ABI v2 to the
portable core ABI v3. Stage 2 performs the project's one approved public ABI
break, validates the new dynamic boundary, and keeps static-await code
generation disabled until Stage 3.

> **Note:** Stage 2 passed all conformance gates on July 14, 2026. Core ABI v3
> is the implemented runtime boundary, and the migration doesn't provide ABI v2
> compatibility.

This design refines the approved
[coroutine architecture](2026-07-14-cr-coroutine-architecture-v3-design.md)
and preserves the observable behavior in
[RFC0001](../../rfcs/0001-core-coroutine-contract.md).

## Decision

Stage 2 uses a hard ABI cut. The compiler, generated headers, project
templates, runtime header, examples, and tests move to ABI v3 together. The
repository doesn't retain a v2 emitter, compatibility macro, adapter, or
configuration switch.

The core reserves a nullable poll context and an opaque waker pointer, but it
doesn't define waker operations, an executor, or a reactor. Stage 5 requires a
separate Waker RFC before those capabilities can become stable.

Stage 2 continues to emit every await through the dynamic ABI v3 path. It
computes and validates the Stage 1 storage plan but doesn't activate
`Embedded`, `Boxed`, or typed cross-translation-unit dispatch. Stage 3 enables
those strategies independently.

## Goals

Stage 2 establishes one portable and extensible dynamic boundary. The
implementation must meet these goals:

- Give poll states and error codes fixed-width C representations.
- Add a nullable, versioned poll context without requiring scheduling support.
- Reduce each dynamic awaitable object to state and one shared vtable pointer.
- Validate an opaque awaitable before its first callback invocation.
- Preserve sticky terminal states, transient yield, and exactly-once cleanup.
- Preserve borrowed and owning adapter behavior with distinct static vtables.
- Compile and link as portable C11 for native and `wasm32-wasi` targets.
- Delete every ABI v2 production path after ABI v3 conformance passes.

## Non-goals

Stage 2 deliberately excludes work whose contract needs separate evidence:

- Static direct `child_poll` code generation.
- Embedded or boxed typed child code generation.
- Waker `wake`, `clone`, or `drop` operations.
- Executor, reactor, IOCP, libuv, GPU, or browser event APIs.
- Cross-thread polling or a required task concurrency guard.
- Strong dynamic C type identity beyond size and alignment.
- Asynchronous drop or graceful asynchronous cancellation.
- Direct LLVM IR, native machine code, or WebAssembly emission.

## Public core declarations

The runtime header exposes the complete core ABI. Public task and context
layouts remain opaque unless this section defines them explicitly.

```c
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
```

`cr_awaitable` is exactly two machine words for a matching target C ABI. The
target data model controls pointer, `size_t`, padding, and aggregate alignment.
Raw C pointers don't form a stable JavaScript or cross-WebAssembly-instance
host ABI.

## Versioning and minimum prefixes

Each extensible structure starts with `abi_version` and `struct_size`. The
runtime header defines version and minimum-prefix constants using `offsetof`
and the size of the last required field.

```c
#define CR_POLL_CONTEXT_ABI_VERSION 1u
#define CR_AWAITABLE_VTABLE_ABI_VERSION 1u

#define CR_POLL_CONTEXT_V1_MIN_SIZE \
    (offsetof(cr_poll_context, waker) + \
     sizeof(((cr_poll_context *)0)->waker))

#define CR_AWAITABLE_VTABLE_V1_MIN_SIZE \
    (offsetof(cr_awaitable_vtable, value_align) + \
     sizeof(((cr_awaitable_vtable *)0)->value_align))
```

A consumer accepts an append-only version when `abi_version` is at least the
minimum supported version and `struct_size` covers the complete prefix it
uses. It ignores unknown trailing bytes and unknown provided flags. It rejects
unknown required capability bits before calling poll.

The core defines these first capability and provided-flag bits:

```c
#define CR_AWAITABLE_CAN_YIELD UINT64_C(1)
#define CR_POLL_CAP_WAKER      UINT64_C(1)
#define CR_POLL_KNOWN_CAPABILITIES CR_POLL_CAP_WAKER
```

`CR_AWAITABLE_CAN_YIELD` is advisory. A child that returns
`CR_POLL_YIELDED` without the flag remains valid, and the parent handles the
status normally. The flag can guide future allocation or scheduling decisions
but doesn't change safety, ownership, or status validity.

Stage 2 doesn't interpret `cr_waker`. A non-null context that advertises
`CR_POLL_CAP_WAKER` must contain a non-null waker pointer; otherwise the
context is structurally invalid. A later Waker RFC defines the pointed-to
object and its lifetime operations.

## Generated public task API

Generated `.h` files expose an opaque task type and functions with the ABI v3
poll signature. A source declaration such as `__async R work(T value)` becomes
the following public shape:

```c
typedef struct cr_work_task cr_work_task;

cr_work_task *cr_work_create(T value, cr_error *out_error);
cr_poll_status cr_work_poll(
    cr_work_task *task,
    const cr_poll_context *poll_context
);
const R *cr_work_result(const cr_work_task *task);
const R *cr_work_yielded(const cr_work_task *task);
const cr_error *cr_work_error(const cr_work_task *task);
cr_awaitable cr_work_as_awaitable(cr_work_task *task);
cr_awaitable cr_work_into_awaitable(cr_work_task *task);
void cr_work_destroy(cr_work_task *task);
```

A `void` task omits result and yielded accessors. A null task poll returns
`CR_POLL_ERROR` without producing an inspectable task error. Calling an
accessor is valid only when the last returned status satisfies RFC0001.

The public poll checks an existing sticky terminal state before validating a
new poll context. Repeated sequential terminal polls therefore return the same
status without cleanup, user work, or context-dependent failure.

## Poll context contract

A null poll context selects manual polling. It has zero available capabilities
and no waker. A non-null context is borrowed for one poll call only; a task or
child must not retain its address.

Before entering a nonterminal task state machine, generated code validates a
non-null context as follows:

1. Require `abi_version >= CR_POLL_CONTEXT_ABI_VERSION`.
2. Require `struct_size >= CR_POLL_CONTEXT_V1_MIN_SIZE`.
3. Reject `CR_POLL_CAP_WAKER` when `waker == NULL`.

Failure records `CR_ERROR_INVALID_POLL_CONTEXT`, follows the task's normal
error-cleanup path, and returns sticky `CR_POLL_ERROR`.

The same context pointer passes through dynamic child polls. Stage 2 performs
no scheduling operation and doesn't require threads, atomics, thread-local
storage, or an operating-system event API.

## Dynamic awaitable activation

A dynamic awaitable is move-only after it transfers into a parent slot. The
source object can't be independently copied, polled, or dropped after transfer.
The parent validates structural safety before marking the slot active.

Activation follows this exact order:

1. Reject a null vtable with `CR_ERROR_INVALID_AWAITABLE_ABI`. Don't invoke a
   callback through an unvalidated table.
2. Read only the vtable header until its version and size establish a safe
   prefix. Reject a version below v1 or a prefix that doesn't reach `drop`.
3. When the prefix safely reaches `drop` but remains shorter than the complete
   v1 minimum, invoke a non-null drop once, then report
   `CR_ERROR_INVALID_AWAITABLE_ABI`.
4. Reject a null mandatory `poll` or `drop` callback with
   `CR_ERROR_MISSING_AWAITABLE_CALLBACK`. Invoke drop only when its field is
   inside the validated prefix and non-null.
5. Compare `value_size` and `value_align` with the compiler-expected result
   layout. On mismatch, invoke drop once and report
   `CR_ERROR_AWAITABLE_LAYOUT_MISMATCH`.
6. Mark the slot active only after every structural and layout check succeeds.

The pointer provider must make the vtable header readable. No C ABI can safely
recover from an arbitrary invalid pointer. A null or structurally unreadable
provider object is outside the valid caller contract beyond the checks above.

## Dynamic polling and status validation

Before every child poll, including resumed polls, generated code verifies that
all `required_context_capabilities` bits are available in the current poll
context. It first rejects any required bit outside
`CR_POLL_KNOWN_CAPABILITIES` with
`CR_ERROR_UNSUPPORTED_POLL_CAPABILITY`, even when a caller mirrors that unknown
bit in `available_capabilities`. Missing known bits produce
`CR_ERROR_MISSING_POLL_CAPABILITY`. Both failures enter the parent's sticky
error path. An edge-owned child drops immediately before lexical cleanup. A
declaration-owned binding remains active until normal lexical error cleanup
reaches its registered position, preserving its LIFO order with `__defer`.

The parent handles validated callback results as follows:

- `CR_POLL_PENDING` retains the active child and returns pending.
- `CR_POLL_YIELDED` retains the child and returns transient yielded.
- `CR_POLL_READY` copies the value, drops an edge-owned child, and continues.
  A declaration-owned binding remains active with its sticky child result.
- `CR_POLL_ERROR` copies the child error, then runs origin-aware error cleanup.
- `CR_POLL_CANCELED` runs origin-aware cancellation cleanup.
- Any other status reports `CR_ERROR_INVALID_POLL_STATUS` and runs
  origin-aware error cleanup.

Origin-aware failure cleanup drops an edge-owned child immediately because it
is the active suspension operation. It leaves a declaration-owned binding
active until the lexical cleanup stack reaches that binding, where drop runs
exactly once in reverse dynamic-activation order with surrounding defers.

An error callback is optional. When a child returns `CR_POLL_ERROR` without a
non-null callback and error record, the parent copies a compiler-owned static
`CR_ERROR_MISSING_CHILD_ERROR` fallback, runs origin-aware error cleanup, and
becomes sticky error. That cleanup drops an edge-owned child immediately and
leaves a binding to its lexical cleanup owner. Every ABI-provided error message
has immutable module lifetime so copying `cr_error` by value before cleanup is
safe.

## Ownership and drop

Generated adapters use separate static vtables because ownership isn't stored
in each awaitable object.

- `*_as_awaitable` borrows task storage. Its vtable drop synchronously
  finalizes active work but doesn't free the task allocation.
- `*_into_awaitable` consumes a heap task. Its vtable drop calls destroy,
  finalizes active work, and frees through the allocating module.

The parent slot's active state guarantees at-most-once vtable drop. A callback
that frees state doesn't need to tolerate a second invocation. Task drop stays
synchronous and can't poll or suspend.

A declaration-owned task binding uses one opaque dynamic child slot during
Stage 2. The declaration activates and validates that slot when execution
reaches the declaration, even when no later await reaches it. Every await of
the binding polls the same slot and observes its sticky terminal state without
reinitialization or edge-owned drop. Lexical scope exit, error, cancellation,
parent drop, or declaration reexecution finalizes the active binding generation
exactly once. Reexecution finalizes the previous generation before activating
the next one. An await reached through a control-flow path that skipped binding
activation reports a parent protocol error instead of polling uninitialized
storage. The stable error is `CR_ERROR_INACTIVE_TASK_BINDING`.

An edge-owned direct await continues to use a separate opaque dynamic slot. It
drops its child when that edge reaches a terminal status, and reentering the
edge creates a new generation. Stage 2 therefore consumes `ChildOrigin` for
lifetime selection even though both origins still use vtable dispatch.

Dropping a pending or yielded parent first finalizes its active edge-owned
child. It then executes declaration-owned child cleanups and `__defer`
callbacks in reverse dynamic-activation order. Ready and error tasks keep their
terminal status when retained storage is dropped. Public destroy can run once
and invalidates the pointer.

## Stable validation errors

The runtime header publishes these compiler protocol errors:

```c
#define CR_ERROR_INVALID_POLL_CONTEXT        1101
#define CR_ERROR_INVALID_AWAITABLE_ABI       1102
#define CR_ERROR_MISSING_AWAITABLE_CALLBACK  1103
#define CR_ERROR_MISSING_POLL_CAPABILITY     1104
#define CR_ERROR_AWAITABLE_LAYOUT_MISMATCH   1105
#define CR_ERROR_INVALID_POLL_STATUS         1106
#define CR_ERROR_UNSUPPORTED_POLL_CAPABILITY 1107
#define CR_ERROR_MISSING_CHILD_ERROR          1108
#define CR_ERROR_INACTIVE_TASK_BINDING        1109
```

Generated code stores these errors in the same sticky parent error path used
for child errors and allocation failures. Messages are compiler-owned static
UTF-8 strings.

## Emission migration

Stage 2 replaces the complete ABI surface in one repository change sequence.
Intermediate commits or task checkpoints may fail old fixtures, but each task
must end with a coherent named gate.

The implementation changes these layers:

1. Replace the runtime header declarations and ABI version.
2. Change internal and public task poll functions to accept poll context.
3. Generate one borrowed and one owning static vtable per async function.
4. Store only `state` and `vtable` in each emitted `cr_awaitable` value.
5. Move dynamic result layout checks to first slot activation.
6. Pass the current poll context through nested dynamic polls.
7. Regenerate `.hr` public headers, templates, examples, and C callers.
8. Replace the ABI v2 golden artifact with an ABI v3 artifact.
9. Delete ABI v2 flags, layouts, adapters, and tests.

The C emitter explicitly ignores `AwaitStorage::Embedded` and
`AwaitStorage::Boxed` during Stage 2. This restriction prevents static dispatch
from obscuring ABI conformance failures.

## Conformance strategy

Stage 2 adds executable product-boundary tests rather than relying only on Rust
structure tests.

### Layout conformance

Native and WebAssembly C fixtures verify the public layouts for their target
data models. Tests cover `sizeof`, `_Alignof`, `offsetof`, version constants,
minimum-prefix macros, and the two-word awaitable invariant. Target-specific
goldens record exact values only for a pinned target triple and toolchain.

### Protocol conformance

Generated-C fixtures exercise malformed but readable objects. They cover:

- Null and undersized poll contexts.
- A Waker capability bit with a null waker.
- Null, short, and unsupported vtable prefixes.
- Missing mandatory callbacks.
- Result size and alignment mismatch.
- Missing required context capabilities.
- Unknown required capability bits when absent and when mirrored by the caller.
- Unknown callback poll status.
- Missing child error callback or error record.
- A yielded status with and without the advisory `CAN_YIELD` flag.

Each failure case asserts the stable error code, sticky parent status, and
origin-aware exact drop count. The two valid yield cases assert
`CR_POLL_YIELDED`, retained active state, and no drop until terminal completion
or parent drop.

### Lifecycle conformance

Existing RFC0001 fixtures migrate to the two-word awaitable API and add
context parameters. They continue to prove pending, transient yield, sticky
terminal states, copy-before-drop error propagation, cancellation, borrowed
and owning adapters, and exactly-once child and defer cleanup.

Task-binding fixtures additionally prove repeated awaits of one terminal child,
cleanup of a never-awaited binding, finalization on every scope-exit path,
generation replacement after declaration reexecution, and exactly-once drop.
They include a control-flow fixture that skips declaration activation and
asserts `CR_ERROR_INACTIVE_TASK_BINDING` without polling uninitialized storage.

### Project and target gates

The final gate includes:

- Native generated-C execution with warnings treated as errors.
- CMake and Meson generated-project execution.
- Portable C11 compilation and linking for pinned `wasm32-wasi`.
- Pinned `wasm-tools validate` in required mode.
- Public header compilation and ABI layout fixtures.
- Rust formatting, all-target tests, checks, and Clippy with warnings denied.

Negative searches reject ABI v2 version constants, per-object callback fields,
`CR_AWAITABLE_OWNS_STATE`, one-argument task poll declarations, and callback
invocations that bypass vtable validation.

## Completion criteria

Stage 2 completes only when all of these conditions hold:

- Every generated public and internal poll signature accepts poll context.
- Every dynamic awaitable is a two-word state/vtable pair.
- Every malformed-object fixture reaches the specified sticky error safely.
- Borrowed and owning adapters use distinct shared vtables.
- Native and required-mode WebAssembly gates pass.
- ABI v2 production code and compatibility switches no longer exist.
- Stage 1 planning remains complete, and Stage 3 static dispatch remains off.

## Next steps

Stage 2 is complete. Create and approve the Stage 3 implementation plan before
enabling embedded, boxed, or cross-unit typed static-await dispatch.
