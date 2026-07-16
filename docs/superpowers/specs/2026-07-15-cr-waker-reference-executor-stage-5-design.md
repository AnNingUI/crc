# CR Waker and reference executor Stage 5 design

This document defines Stage 5 of CR coroutine development. Stage 5 stabilizes
the Waker semantic contract and Waker v1 extension ABI, then validates that
contract with manual, queued single-thread, and native cross-thread reference
execution paths.

> **Note:** This is a preview feature currently under active development.

The design builds on the approved
[coroutine architecture](2026-07-14-cr-coroutine-architecture-v3-design.md),
the completed
[Stage 4 design](2026-07-15-cr-coroutine-cfg-stage-4-design.md),
and [RFC0001](../../rfcs/0001-core-coroutine-contract.md).

## Decision

Stage 5 uses a contract-first, layered extension. The existing
`cr_runtime.h` remains the portable core ABI v3 boundary. A separate stable
`cr_waker.h` defines a two-word Waker handle and a versioned shared vtable.
Experimental reference executors consume that Waker ABI without becoming part
of the stable runtime contract.

The Waker requests a future poll. It doesn't represent readiness, resume a task
synchronously, know the task layout, or expose an executor. Each awaitable owns
its readiness state and prevents lost wakeups through registration followed by
a readiness recheck.

Stage 5 doesn't define a reactor or backend SPI. Stage 6 must validate at least
two materially different event models before it stabilizes a shared backend
prefix.

## Goals

Stage 5 has focused semantic and portability goals.

- Define versioned Waker ownership, cloning, waking, and dropping.
- Guarantee that wake never synchronously reenters the same task.
- Permit duplicate and spurious wakes and deterministic coalescing.
- Define a lost-wakeup-safe registration contract for awaitables.
- Make late wake after cancellation or terminal completion a safe no-op.
- Validate a single-thread queue without atomics or thread dependencies.
- Validate optional native cross-thread wake without concurrent task polling.
- Preserve manual null-context polling without new runtime cost.
- Preserve Stage 3 typed static await and Stage 4 context optimization.
- Compile the single-thread path as portable C11 for `wasm32-wasi`.

## Non-goals

Stage 5 deliberately excludes interfaces that don't yet have enough backend
evidence.

- Reactor, proactor, EventSource, or backend SPI stabilization.
- IOCP, libuv, epoll, kqueue, browser, GPU, timer, or socket APIs.
- Work stealing, a stable thread pool, or concurrent polling of one task.
- Channel, join, race, select, or generator semantics.
- Asynchronous drop or graceful asynchronous cancellation.
- A task pointer or executor pointer in the public Waker ABI.
- A new source-language keyword or compiler-visible scheduling primitive.
- A public ABI v4 or a change to core task and poll-context layout.
- Direct LLVM IR, native machine code, or direct WebAssembly emission.

## Compatibility boundary

Core ABI v3 remains compatible with existing generated code. Stage 5 defines
an extension rather than adding another core ABI break.

- `CR_RUNTIME_ABI_VERSION` remains `3`.
- `cr_poll_context` retains its existing layout, version, and minimum prefix.
- `cr_runtime.h` retains the opaque `typedef struct cr_waker cr_waker`.
- `CR_POLL_CAP_WAKER` remains the only Waker-related poll capability bit.
- Stage 5 doesn't add `CR_POLL_CAP_CROSS_THREAD_WAKER`.
- Cross-thread support is advertised by the Waker vtable, not the core poll
  context.
- Old generated code therefore accepts a Stage 5 executor poll context instead
  of rejecting an unknown available-capability bit.

The separate Waker extension has its own ABI version. Future Waker versions can
append fields to the vtable but can't reorder or reinterpret the v1 prefix.

## Public Waker v1 extension

The stable extension header is `cr_waker.h`. It includes `cr_runtime.h` and
completes the previously opaque Waker declaration.

The conceptual public declarations are:

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

The stable constants and helper surface are conceptually:

```c
#define CR_WAKER_VTABLE_ABI_VERSION 1u
#define CR_WAKER_FLAG_CROSS_THREAD UINT64_C(1)

bool cr_waker_is_valid(const cr_waker *waker);
bool cr_waker_clone(const cr_waker *source, cr_waker *out_clone);
void cr_waker_wake(const cr_waker *waker);
void cr_waker_drop(cr_waker *waker);
```

The helpers are portable `static inline` C11 functions in `cr_waker.h`. Using
the stable Waker ABI doesn't add a required runtime library symbol.

The v1 minimum prefix ends after `drop_state`. A consumer accepts
`abi_version >= CR_WAKER_VTABLE_ABI_VERSION` only when `struct_size` covers
that prefix and all three callbacks are non-null. Future versions may append
fields while preserving the v1 prefix. An incompatible revision must use a new
extension ABI type instead of reinterpreting this prefix. The handle remains
exactly two machine words.

`provided_flags` describes properties of this Waker implementation. Unknown
provided flags don't invalidate the handle. A consumer that needs a specific
property checks the matching stable bit before retaining the Waker.

## Waker ownership contract

The `cr_poll_context` borrows its `waker` pointer for one poll call. An
awaitable that retains a Waker after returning from poll must own a clone.

The ownership rules are:

- A valid clone operation is infallible and doesn't allocate.
- `clone_state` returns a non-null state with one new owned reference.
- `cr_waker_wake` doesn't consume or drop the handle.
- Every successful clone is passed to `cr_waker_drop` exactly once.
- `cr_waker_drop` clears the dropped handle to the null representation.
- The original executor-owned handle and all clones share one vtable.
- A provider that needs allocation performs it when it creates the original
  control block, not during clone.

Without `CR_WAKER_FLAG_CROSS_THREAD`, clone and drop execute on the executor
owner thread. With that flag, `clone_state`, `wake_by_ref`, and `drop_state`
must be safe from any thread and may run concurrently on distinct clones. The
vtable storage is immutable and has module lifetime. The executor-owned
original reference is released at terminal completion, cancellation, or
shutdown. Ticket references and retained clones keep the control block, but
never the task payload, alive.

`cr_waker_clone` returns `false` for a structurally invalid source or a provider
that violates the infallible clone contract by returning null. It leaves the
output handle null on failure.

The extension reserves these stable error codes for Waker-aware awaitables and
reference runtime diagnostics:

```c
#define CR_ERROR_INVALID_WAKER_ABI  1110
#define CR_ERROR_WAKER_CLONE_FAILED 1111
```

## Wake contract

Wake requests that the associated active task be polled again in the future.
It never directly calls the task poll function.

The observable rules are:

- Duplicate and spurious wakes are valid.
- An executor can coalesce multiple outstanding wakes into one queue entry.
- Wake during a poll can enqueue future work but can't reenter that poll.
- Before cancellation, terminal completion, or shutdown, a wake eventually
  produces another poll when the executor continues making progress.
- After cancellation, terminal completion, or shutdown, wake is a safe no-op.
- Wake can't revive a canceled or terminal task.
- Task poll remains single-owner and nonconcurrent.

A Waker without `CR_WAKER_FLAG_CROSS_THREAD` can be woken only on its executor
owner thread. A Waker with that flag can be woken from another thread. For a
cross-thread Waker, readiness writes sequenced before wake become visible to the
poll scheduled by that wake.

Every valid cross-thread wake publishes readiness before it attempts queue
coalescing. The owner acquires the corresponding publication before the next
relevant poll, even when the task was already queued or currently polling and
no second queue record is created. A wake that races terminal completion or
cancellation has no required observation because no later task poll occurs.

The contract defines this happens-before behavior without prescribing a
particular lock-free state machine or C memory-order expression.

## Control-block lifetime

The Waker retains an executor control block, not the coroutine task payload.
This separation prevents a late callback from accessing freed task storage.

The control block has semantic active, queued, polling, canceled, terminal,
and shutdown observations. Those observations don't become a public layout or
public enum.

Cancellation and terminal completion follow this order:

1. Prevent new wake requests from creating effective runnable work.
2. Make existing ready-queue entries safe to skip.
3. Notify the observer of the terminal status when required.
4. Synchronously drop the task payload exactly once.
5. Retain the control block until executor, ticket, queue, and Waker references
   are released.

The implementation tracks these semantic ownership sources. Their concrete
representation remains experimental.

| Ownership source | Acquired | Released |
| --- | --- | --- |
| Active-task reference | Successful spawn | Terminal, cancel, or shutdown |
| Caller ticket | Successful spawn | `cr_executor_task_release` |
| Ready-queue reference | Effective enqueue | Dequeue, cancel drain, or shutdown |
| Executor-owned Waker | Successful spawn | Terminal, cancel, or shutdown |
| Retained Waker clone | `cr_waker_clone` | Matching `cr_waker_drop` |

Every source retains the control block. The control block retains the shared
executor state, while only the active-task reference owns the task payload.

A race between wake and cancellation can either enqueue a record that the owner
later skips or observe cancellation and do nothing. It can't poll after payload
destruction, run cleanup twice, or resurrect the task.

## Lost-wakeup registration contract

Readiness belongs to the concrete awaitable. Waker and executor code don't
interpret edge-triggered, level-triggered, completion, channel, or join state.

A Waker-aware awaitable uses this semantic sequence:

1. Check readiness.
2. Clone and publish the current Waker when the operation isn't ready.
3. Replace and drop any older registered Waker.
4. Recheck readiness after publication.
5. Clear the registration and return terminal progress when ready.
6. Return `CR_POLL_PENDING` only when the operation remains unready and owns a
   valid registration.

This sequence handles all event positions:

- An event before publication is observed by the second readiness check.
- An event during publication either wakes the published Waker or is observed
  by the second check.
- An event after `Pending` wakes the retained Waker.

The public ABI doesn't prescribe the awaitable's mutex, atomic state, callback
registration API, or readiness representation. Reference helpers can manage
Waker replacement and ownership, but the awaitable still owns synchronization
and the final readiness recheck.

## Execution paths

Stage 5 validates one stable Waker contract through three execution paths. Only
the Waker extension is stable.

### Manual polling

Manual polling continues to pass a null poll context. It constructs no Waker,
allocates no executor control block, and adds no queue or atomic operation.

The caller chooses when to poll again after `CR_POLL_PENDING`. This path remains
the minimum runtime, embedded, and differential-testing baseline.

### Queued single-thread executor

The default reference executor owns a FIFO ready queue and is the only poll
owner. Wake appends a control block only when it isn't already queued.

The single-thread implementation uses ordinary reference counts and state
fields. It doesn't include `<stdatomic.h>`, create threads, or depend on a
platform event API.

The executor handles poll results as follows:

- `CR_POLL_PENDING` leaves the task dormant until wake.
- `CR_POLL_YIELDED` notifies the observer and appends the task to the queue
  tail.
- `CR_POLL_READY`, `CR_POLL_ERROR`, and `CR_POLL_CANCELED` notify the observer,
  finalize the task, and prevent future effective wake.

This implementation is the required WebAssembly path.

### Native cross-thread reference path

The native cross-thread executor retains one poll-owner thread. Other producer,
I/O callback, or test threads can wake a task through a thread-safe ready queue.

The first implementation favors auditability over lock-free performance:

- Atomic reference counting protects the control block.
- A mutex protects queue mutation and duplicate-wake coalescing.
- A condition variable lets the owner wait for ready work or shutdown.
- Windows and POSIX synchronization live in separate experimental sources.
- The threaded source isn't built for WebAssembly.

Stage 5 doesn't add work stealing, multiple poll workers, or concurrent task
polling.

## Experimental executor surface

Reference executor declarations live in `cr_executor.h` and remain
experimental. Their names, layout, and source organization can change in Stage
6 without breaking Waker v1.

The conceptual types are:

```c
typedef struct cr_executor cr_executor;
typedef struct cr_executor_task cr_executor_task;

typedef void (*cr_executor_observer_fn)(
    void *user,
    cr_poll_status status,
    const void *value,
    const cr_error *error
);
```

The conceptual operations are:

```c
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
```

Spawn consumes and clears the source awaitable only after successful control
block creation. A failed spawn leaves the input unchanged.

`cr_executor_create_single` constructs the portable FIFO implementation.
`cr_executor_create_threaded` constructs the native cross-thread
implementation and returns null when that module isn't supported.
`cr_executor_wait_one` is an owner-thread operation for the threaded executor;
it blocks until it polls one ready task or observes shutdown. The single-thread
executor uses only `cr_executor_run_ready` and never blocks.

The thread that creates an executor is its poll owner. Spawn, run, wait, cancel,
shutdown, and destroy are owner-thread operations. The threaded executor permits
other threads to call only Waker operations and
`cr_executor_request_shutdown`. A shutdown request is nonblocking: it marks the
shared state closing and signals the condition variable. The owner wakes from
`cr_executor_wait_one`, performs synchronous task cancellation and payload drop,
then returns `false` from the wait operation.

`cr_executor_run_ready` also observes a pending shutdown request between polls
and performs the same owner-thread shutdown sequence. `cr_executor_destroy`
can't race `run_ready` or `wait_one` because all three require the same owner
thread. A producer that requests shutdown must finish using the public executor
handle before the owner destroys it; retained Waker clones remain safe because
they own the separate shared state.

The single-thread library provides a portable
`cr_executor_create_threaded` stub that returns null with an unsupported error.
Targets without the threaded implementation therefore retain one linkable
experimental executor surface without importing native thread dependencies.

The executor treats the root task as a genuine dynamic boundary. Generated
applications can pass an owning `*_into_awaitable` adapter. Nested eligible
static awaits remain embedded or boxed typed children and continue calling
their concrete poll symbols directly.

Spawn validates the awaitable vtable before consuming it. For a non-void root,
the executor allocates an aligned result buffer using `value_size` and
`value_align`. It overallocates and adjusts the result pointer when the
platform allocator's default alignment is insufficient. A zero-sized result
uses a null output pointer. The buffer remains owned by the control block and
is valid only during the matching observer call.

Spawn rejects a nonzero result whose alignment is zero, isn't a power of two,
or overflows the aligned allocation calculation. These failures use the
existing awaitable layout-mismatch error.

Observer value and error pointers remain valid only during the callback. The
observer copies data it needs to retain. Observer callbacks run on the poll
owner thread and can't reenter run or cancel operations on the same executor.

Reference cancellation is an owner-thread operation. Stage 5 doesn't stabilize
a cross-thread cancellation API. Cross-thread tests race an external wake with
owner-thread cancellation.

The executor owns one active-task reference. A successful spawn returns one
independent ticket reference to the caller. Releasing that ticket never cancels
the task and is valid before or after terminal completion. Queue records and
retained Waker clones hold control-block references as needed. Terminal
completion or explicit cancellation invokes the observer exactly once, drops
the payload exactly once, and releases the executor's active-task reference.

Cancel is idempotent while the caller owns a valid ticket. Cancel after a
terminal status doesn't invoke the observer again. The caller can't use a
ticket after `cr_executor_task_release`.

Shutdown rejects new spawn, marks the shared executor state closed, cancels
every active task exactly once, invalidates or drains queue records, and
releases the executor's active-task references. `cr_executor_destroy` releases
the executor handle and performs shutdown first when needed. Shared queue state
remains alive until the last ticket, queue, or Waker reference releases it. A
wake after destroy sees the closed state and performs no queue access.

Only the owner executes `cr_executor_shutdown`. Other threads use
`cr_executor_request_shutdown` and never cancel or drop task payloads directly.

## Compiler and generated-code integration

Stage 5 requires no source syntax and no new coroutine lowering. The current
compiler already threads one borrowed poll context through public tasks,
embedded children, boxed children, cross-unit typed children, and dynamic
awaitables.

Compiler work is limited to extension packaging and conformance checks:

- Emit or install `cr_waker.h` as a separate stable extension header.
- Package experimental single-thread executor sources independently.
- Package native threaded sources only for supported native targets.
- Keep generated task poll signatures and task context layouts unchanged.
- Keep static child poll sites free of `cr_awaitable` construction and vtable
  dispatch.
- Keep manual projects valid when they don't include or link executor files.

Waker-aware third-party awaitables include `cr_waker.h`, require the existing
`CR_POLL_CAP_WAKER`, validate the handle before cloning, and inspect
`CR_WAKER_FLAG_CROSS_THREAD` only when their callback model requires it.

## Failure and shutdown behavior

Failures are explicit and preserve move ownership.

- Invalid Waker structure produces `CR_ERROR_INVALID_WAKER_ABI` in a reference
  Waker-aware awaitable.
- A clone provider that returns null produces
  `CR_ERROR_WAKER_CLONE_FAILED`.
- A missing core Waker capability continues to use
  `CR_ERROR_MISSING_POLL_CAPABILITY`.
- A spawn allocation failure returns an executor error and leaves the source
  awaitable unconsumed.
- Shutdown rejects new spawn, makes later wake ineffective, and synchronously
  cancels remaining tasks on the owner thread.
- Queue records that outlive cancellation are skipped without reading task
  payload storage.

Duplicate wake, spurious wake, wake after cancellation, and wake after terminal
completion aren't errors.

## Native-first and WebAssembly behavior

Native performance remains the primary implementation priority, while the
portable single-thread contract remains mandatory.

The single-thread wake path must be constant-time, allocate no memory, and use
no atomics. Control-block allocation happens at spawn. An intrusive or otherwise
preallocated ready record prevents allocation during wake.

The cross-thread reference path can use a mutex and condition variable. Stage 5
doesn't claim lock-free performance. Measurements from later native backends can
justify specialized queue implementations without changing Waker v1.

The required `wasm32-wasi` gate compiles the core runtime, Waker extension,
single-thread executor, and a Waker-aware event fixture. It excludes threaded
sources and WebAssembly atomics. Native execution remains the primary behavior
gate, and pinned `wasm-tools` validates the linked module.

## Conformance strategy

Stage 5 requires ABI, lifecycle, race, generated-C, native, and WebAssembly
evidence. Timing sleeps don't define race correctness; deterministic barriers
and controllable event hooks drive every interleaving.

### Waker ABI tests

The stable extension tests cover these cases:

- A two-machine-word `cr_waker` layout.
- Exact v1 prefix size and callback offsets.
- Null, version-zero, truncated, and missing-callback vtables.
- Successful clone and exactly-once drop accounting.
- A clone provider that violates the non-null contract.
- Unknown provided flags that don't invalidate the v1 prefix.
- A future append-only version whose v1 prefix remains accepted.
- Cross-thread clone, wake, and drop on distinct retained handles.
- Core `cr_poll_context` size and offsets unchanged from Stage 4.

### Lost-wakeup tests

The controllable event awaitable tests readiness before registration, during
publication, and after `Pending`. It also tests Waker replacement, old-clone
drop, duplicate wake, spurious wake, and wake during poll without reentrancy.

### Executor lifecycle tests

Reference executor tests cover FIFO order, queue coalescing, yield requeue,
ready, error, cancellation, shutdown, observer pointer lifetime, failed spawn,
and exact awaitable drop behavior.

Control-block tests retain an external Waker clone across cancel, terminal
completion, and shutdown. The task payload drops once, late wake is a no-op, and
the control block releases after the last ticket and Waker reference.

### Cross-thread tests

Native tests use deterministic barriers to race wake against registration,
cancel, completion, and shutdown. They verify readiness visibility and prove
that one owner thread performs every poll.

Coalescing tests publish a distinct readiness write before every duplicate
wake. The next relevant poll must observe all writes even when the task was
already queued or polling.

A shutdown test blocks the owner in `cr_executor_wait_one`, requests shutdown
from a producer thread, and proves that the owner wakes, cancels each task once,
and returns before destroy releases the public executor handle. Unsupported
targets compile and link the portable threaded-constructor stub.

The supported Windows and POSIX test paths compile independently. The default
single-thread build remains free of native synchronization dependencies.

### Compiler and project tests

Generated-code tests prove that the poll context reaches static and dynamic
children unchanged. Static parent poll bodies remain free of dynamic adapter
construction and indirect child poll calls.

A compatibility fixture compiles an unchanged Stage 4 generated translation
unit that includes only `cr_runtime.h`, then links it into a Stage 5 executor
application. The executor supplies only the existing `CR_POLL_CAP_WAKER` bit,
so the old task accepts the context and runs without recompilation.

Generated native projects execute manual and queued Waker fixtures. The
required WASI project selects the single-thread executor, compiles and links the
portable C11 output, and passes module validation.

Every task must continue to pass these existing project gates:

```text
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
pnpm run grammar:test
native generated-project compile and run
CMake and Meson generated-project compile and run
required wasm32-wasi compile, link, and validation
ABI v3 and Stage 3/4 regression suites
```

## Staged implementation

Stage 5 is split so semantic failures appear before executor complexity.

1. Write and approve RFC0002 for Waker semantics and compatibility.
2. Add Waker v1 ABI declarations, helpers, and conformance tests.
3. Add a controllable event awaitable and deterministic lost-wakeup tests.
4. Add the experimental single-thread FIFO executor.
5. Add cancellation, terminal, shutdown, and retained-clone lifecycle tests.
6. Add the native cross-thread reference queue and deterministic race tests.
7. Package the Waker and single-thread executor in generated projects.
8. Add required WASI compilation, linking, and validation.
9. Run final native, ABI, grammar, Stage 3/4, and WebAssembly gates.

The executor implementation doesn't start until RFC0002 and the Waker v1 ABI
tests are approved.

## Stop conditions

Stop and amend this design instead of broadening implementation when any of
these conditions occurs:

- Waker support requires changing the core `cr_poll_context` layout.
- Compatibility requires passing a new unknown core capability bit to old
  generated code.
- Wake correctness requires synchronous task reentry.
- One task must be polled concurrently by multiple workers.
- Waker clone requires allocation or can fail for resource exhaustion.
- Cancellation requires asynchronous drop or suspended cleanup.
- A late wake can observe freed task payload storage.
- The default single-thread path requires atomics or native thread APIs.
- `wasm32-wasi` requires threaded sources or WebAssembly atomics.
- Static await requires dynamic adapter construction or indirect child poll.
- Correctness depends on a reactor, EventSource, timer, socket, or backend SPI.
- Timing sleeps are required to make race tests pass.
- Experimental executor layout must become stable to complete Waker tests.

## Acceptance criteria

Stage 5 completes only when the stable Waker contract and all reference paths
meet these criteria:

- Waker v1 is versioned, two words, cloneable, droppable, and non-reentrant.
- Duplicate, spurious, late, and cross-thread wake behavior matches the RFC.
- Lost-wakeup tests cover every registration boundary deterministically.
- Task payload and control-block lifetimes remain separate and exactly owned.
- Manual polling remains behaviorally and structurally unchanged.
- The single-thread executor uses no atomics and passes native and WASI gates.
- The cross-thread reference path preserves one poll owner and visibility.
- Core ABI v3 layout, Stage 3 dispatch, and Stage 4 optimization remain intact.
- Executor APIs remain explicitly experimental.
- Reactor and backend SPI behavior remain absent.

## Next steps

After independent review and user approval of this design, create a detailed
Stage 5 implementation plan. The first implementation task writes RFC0002 and
its executable Waker ABI conformance tests. Don't implement executor behavior
before those gates pass.
