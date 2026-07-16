# RFC0002: Waker contract

This RFC defines the stable CR Waker extension. A Waker requests a future task
poll without exposing task layout, executor layout, readiness state, or a
backend API.

> **Note:** This contract was accepted on July 15, 2026. Its Stage 5
> implementation is pending. Reference executor APIs remain experimental.

This RFC extends
[RFC0001](0001-core-coroutine-contract.md) without changing core ABI v3 task
lifecycle, synchronous drop, or generated poll-context layout.

## Contract boundary

The Waker extension stabilizes observable scheduling-request behavior. It
doesn't stabilize a queue, task control block, executor, reactor, or event
source.

The stable contract covers these behaviors:

- Waker layout and append-only vtable compatibility.
- Handle validation, clone ownership, non-consuming wake, and drop.
- Duplicate, spurious, coalesced, and late wake behavior.
- Optional cross-thread callback safety and readiness visibility.
- Awaitable-owned registration and lost-wakeup prevention.
- Cancellation and terminal behavior for retained Waker clones.

Reference executor operations validate the contract but remain experimental.
Stage 6 must validate multiple event backends before CR stabilizes a reactor or
backend SPI.

## Core ABI compatibility

Waker v1 is a separate extension ABI. It doesn't require another core runtime
ABI break.

The compatibility rules are:

- `CR_RUNTIME_ABI_VERSION` remains `3`.
- `cr_poll_context` retains its v1 layout and minimum prefix.
- `cr_runtime.h` retains `typedef struct cr_waker cr_waker` as an opaque
  forward declaration.
- `CR_POLL_CAP_WAKER` remains the only Waker-related core poll capability.
- CR doesn't add a cross-thread Waker bit to
  `cr_poll_context.available_capabilities`.
- Cross-thread callback safety is advertised by the Waker vtable.

An old generated translation unit therefore receives only the capability bit
it already recognizes. Stage 5 executors can't require old code to accept an
unknown core capability.

## Stable Waker v1 declarations

The stable declarations live in `cr_waker.h`. The header includes
`cr_runtime.h` and completes the opaque Waker type.

The Waker v1 ABI is:

```c
typedef struct cr_waker_vtable {
    uint32_t abi_version;
    uint32_t struct_size;
    uint64_t provided_flags;
    void *(*clone_state)(void *state);
    void (*wake_by_ref)(void *state);
    void (*drop_state)(void *state);
} cr_waker_vtable;

typedef struct cr_waker {
    void *state;
    const cr_waker_vtable *vtable;
} cr_waker;
```

The stable constants and helper surface are:

```c
#define CR_WAKER_VTABLE_ABI_VERSION 1u
#define CR_WAKER_FLAG_CROSS_THREAD UINT64_C(1)

bool cr_waker_is_valid(const cr_waker *waker);
bool cr_waker_clone(const cr_waker *source, cr_waker *out_clone);
void cr_waker_wake(const cr_waker *waker);
void cr_waker_drop(cr_waker *waker);
```

The helpers are portable `static inline` C11 functions. A Waker consumer
doesn't need to link an additional stable runtime library.

The `cr_waker` handle contains exactly two machine words. It doesn't contain a
task pointer, executor pointer, callback inline storage, ownership flag, or
readiness mode.

## Version and validity contract

The v1 minimum prefix ends after `drop_state`. Waker v1 consumers accept
`abi_version >= CR_WAKER_VTABLE_ABI_VERSION` when `struct_size` covers the
complete v1 prefix.

Future compatible versions can append fields while preserving every v1 field
and its meaning. An incompatible revision must use a different extension ABI
type instead of reinterpreting the v1 prefix.

A valid Waker satisfies all of these conditions:

- The handle pointer is non-null.
- `state` and `vtable` are non-null.
- `abi_version` is at least v1.
- `struct_size` covers the v1 minimum prefix.
- `clone_state`, `wake_by_ref`, and `drop_state` are non-null.

Unknown `provided_flags` don't invalidate a structurally valid Waker. A
consumer that requires a known property checks the corresponding stable flag
before retaining the handle.

## Ownership and cloning

The `cr_poll_context` borrows its Waker for one poll call. An awaitable that
retains a Waker after returning from poll must own a clone.

Clone and drop follow these rules:

- Valid clone is infallible and doesn't allocate.
- `clone_state` returns non-null state with one new owned reference.
- A provider performs required allocation when it creates the original state,
  not during clone.
- `cr_waker_clone` leaves `out_clone` null on structural or provider failure.
- Every successful clone has exactly one matching `cr_waker_drop`.
- `cr_waker_drop` invokes `drop_state` and clears the handle.
- `cr_waker_wake` doesn't consume, clear, or drop the handle.
- The shared vtable is immutable and has module lifetime.

Concurrent use of the same mutable handle object violates the caller contract.
Distinct owned clones can be used concurrently only when the Waker advertises
`CR_WAKER_FLAG_CROSS_THREAD`.

## Wake behavior

Wake requests another future poll of the associated active task. It never
directly invokes the task poll function.

The wake rules are:

- Duplicate wakes are valid.
- Spurious wakes are valid.
- An executor can coalesce several wakes into one ready-queue record.
- Wake during poll can request later work but can't reenter that poll.
- Before cancellation, terminal completion, or shutdown, wake eventually
  produces another poll when the executor continues making progress.
- After cancellation, terminal completion, or shutdown, wake is a safe no-op.
- Wake can't revive a canceled or terminal task.
- One owner executes each task poll, and task poll remains nonconcurrent.

The contract doesn't require a poll for every wake. It requires enough future
polling to observe all readiness published by valid wakes before the task
becomes terminal or canceled.

## Cross-thread behavior

A Waker without `CR_WAKER_FLAG_CROSS_THREAD` is confined to its executor owner
thread. Its clone, wake, and drop callbacks run only on that thread.

A Waker with `CR_WAKER_FLAG_CROSS_THREAD` has stronger requirements:

- `clone_state`, `wake_by_ref`, and `drop_state` are safe from any thread.
- Distinct clones can invoke those callbacks concurrently.
- Readiness writes sequenced before wake become visible to the poll requested
  by that wake.
- Every wake publishes readiness before queue coalescing.
- The owner acquires the publication before the next relevant poll even when
  the task was already queued or currently polling.

A wake that races cancellation or terminal completion has no visibility
obligation when no later task poll occurs. It must still remain memory-safe and
must not access destroyed task payload storage.

This RFC defines happens-before behavior, not one required atomic state
machine, queue algorithm, or C memory-order expression.

## Lost-wakeup prevention

Readiness belongs to each concrete awaitable. Waker and executor code don't
interpret readiness, completion, channel, join, edge-triggered, or
level-triggered state.

A Waker-aware awaitable uses this sequence when it may return `Pending`:

1. Check readiness.
2. Clone and publish the current Waker when the operation isn't ready.
3. Replace and drop any previous registered Waker.
4. Recheck readiness after publishing the new registration.
5. Clear the registration and return progress when the operation is ready.
6. Return `CR_POLL_PENDING` only when it remains unready and owns a valid
   registration.

This sequence covers every event position:

- An event before publication is observed by the second readiness check.
- An event during publication either wakes the published Waker or is observed
  by the second readiness check.
- An event after `Pending` wakes the retained Waker.

The awaitable chooses its own synchronization and callback-registration
mechanism. This RFC doesn't require a mutex, atomic variable, lock-free state
machine, or shared EventSource base.

## Cancellation, terminal completion, and shutdown

The Waker retains scheduling state, not coroutine task payload storage. A
provider must separate those lifetimes so a late callback can't read a dropped
task.

Cancellation and terminal completion follow these semantic rules:

1. Prevent new wake requests from creating effective runnable work.
2. Make an existing queue record safe to skip.
3. Synchronously drop the task payload exactly once.
4. Keep scheduling state alive while tickets, queue records, or Waker clones
   remain.
5. Make every later wake a no-op.

A race between wake and cancellation can enqueue a record that the owner later
skips, or it can observe cancellation and do nothing. It can't poll after
payload destruction, run cleanup twice, or resurrect the task.

Hard task drop remains synchronous and can't suspend. Waker support doesn't add
asynchronous drop, graceful cancellation cleanup, or executor-owned deferred
destruction of the task payload.

Reference threaded executors use a nonblocking shutdown request. A producer
thread can mark shared scheduling state closing and wake the poll owner. Only
the owner thread cancels tasks, invokes observers, drops payloads, completes
shutdown, and destroys the public executor handle.

## Reference executor relationship

Stage 5 validates Waker v1 with manual, single-thread, and native cross-thread
reference execution. Those executor APIs and layouts remain experimental.

Reference executors follow these semantic ownership sources:

| Ownership source | Acquired | Released |
| --- | --- | --- |
| Active-task reference | Successful spawn | Terminal, cancel, or shutdown |
| Caller ticket | Successful spawn | Ticket release |
| Ready-queue reference | Effective enqueue | Dequeue, cancel drain, or shutdown |
| Executor-owned Waker | Successful spawn | Terminal, cancel, or shutdown |
| Retained Waker clone | Successful clone | Matching Waker drop |

Only the active-task reference owns the task payload. Every listed source can
retain the scheduling control block and shared executor state.

The thread that creates a reference executor is its poll owner. Spawn, poll,
cancel, shutdown, payload drop, and destroy run on that owner. A threaded
producer can use cross-thread Waker operations and request shutdown, but it
can't poll or destroy the task.

Executor API names, ticket layout, queue layout, synchronization objects, and
observer representation aren't stabilized by this RFC.

## Error classification

The Waker extension reserves stable error codes for Waker-aware awaitables and
reference runtime diagnostics:

```c
#define CR_ERROR_INVALID_WAKER_ABI  1110
#define CR_ERROR_WAKER_CLONE_FAILED 1111
```

`CR_ERROR_INVALID_WAKER_ABI` reports structural validation failure.
`CR_ERROR_WAKER_CLONE_FAILED` reports a provider that violates the infallible
clone contract by returning null.

A missing `CR_POLL_CAP_WAKER` continues to use the existing
`CR_ERROR_MISSING_POLL_CAPABILITY`. Cross-thread support doesn't add a core
poll-context capability or core error code.

Duplicate, spurious, coalesced, late, and post-terminal wakes aren't errors.

## Compatibility classification

CR uses these compatibility classes after this RFC:

- **Implemented core contract:** RFC0001 and core ABI v3 behavior.
- **Accepted stable extension contract:** Waker v1 semantics and append-only
  extension ABI defined here.
- **Experimental implementation:** Reference executor API, layout, queues,
  thread adapters, and test event primitives.
- **Future design:** Reactor, backend SPI, channels, join, race, select,
  generator integration, and graceful asynchronous cancellation.

Waker v1 implementation can proceed without weakening RFC0001. A future Waker
change that weakens this RFC requires a new approved RFC.

## Conformance

Executable C tests define Waker conformance. Internal Rust state tests don't
replace the source-to-source runtime boundary.

The conformance suite must cover these cases:

- Two-word handle layout and exact v1 minimum prefix.
- Null, version-zero, truncated, and missing-callback vtables.
- Append-only future versions with a valid v1 prefix.
- Exact original, clone, wake, and drop accounting.
- A clone provider that violates the non-null result contract.
- Readiness before registration, during publication, and after `Pending`.
- Replacement and drop of an older registered Waker.
- Duplicate, spurious, coalesced, and wake-during-poll behavior.
- Cancellation, terminal completion, and shutdown with retained clones.
- Cross-thread clone, wake, drop, and readiness visibility.
- A blocked owner released by a producer shutdown request.
- Manual null-context behavior unchanged.
- Static child context forwarding without dynamic dispatch fallback.
- Native single-thread, native cross-thread, and `wasm32-wasi` compilation.
- An unchanged Stage 4 generated object linked into a Stage 5 executor app.

Race tests use deterministic barriers and controllable hooks. Timing sleeps
don't define conformance.

## Non-goals

This RFC doesn't stabilize or require these features:

- A reactor, proactor, EventSource, or backend SPI.
- Timer, socket, IOCP, libuv, epoll, kqueue, browser, or GPU APIs.
- A stable executor, queue, ticket, observer, or thread-pool ABI.
- Work stealing or concurrent polling of one task.
- A task pointer or executor pointer in `cr_waker`.
- Allocation during valid clone.
- Asynchronous drop or suspended cancellation cleanup.
- WebAssembly threads or atomics for the portable single-thread path.
- New CR source syntax or a compiler-visible scheduling node.

## Next steps

Stage 5 completed `cr_waker.h`, lost-wakeup conformance, portable and native
reference executors, cancellation races, old-object compatibility, and required
`wasm32-wasi` validation.

The approved
[Stage 6 design](../superpowers/specs/2026-07-16-cr-backend-validation-stage-6-design.md)
and
[implementation plan](../superpowers/plans/2026-07-16-cr-backend-validation-stage-6-implementation.md)
validate IOCP completion, epoll readiness, and kqueue readiness before freezing
a common backend prefix.
