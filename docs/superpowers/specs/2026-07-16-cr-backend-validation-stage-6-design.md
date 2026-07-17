# CR backend validation and SPI Stage 6 design

This document defines Stage 6 of CR coroutine development. Stage 6 validates
one completion backend and two readiness backends with the same socket receive
semantics before CR freezes any backend service provider interface.

> **Note:** This is a preview feature currently under active development.

The design builds on the approved
[coroutine architecture](2026-07-14-cr-coroutine-architecture-v3-design.md),
[RFC0001](../../rfcs/0001-core-coroutine-contract.md),
[RFC0002](../../rfcs/0002-waker-contract.md), and the completed
[Stage 5 design](2026-07-15-cr-waker-reference-executor-stage-5-design.md).
The validated stable prefix is defined by
[RFC0003](../../rfcs/0003-backend-core-and-net-receive-contract.md).

## Decision

Stage 6 uses a minimal backend core with versioned capability extensions. It
doesn't create a universal EventSource or force readiness and completion into
one public event union.

The first semantic extension is a one-shot receive operation over a borrowed,
already-connected native TCP socket. The operation uses a borrowed pinned
buffer, caller-owned opaque storage, asynchronous cancellation, and a
synchronous quiescence fence before destruction.

Stage 6 validates four providers:

- Windows IOCP as the completion model.
- Linux epoll as one readiness model.
- macOS kqueue as a second readiness implementation.
- A portable memory provider for deterministic conformance and
  `wasm32-wasi` compilation.

The memory provider doesn't count as one of the required real event models.
IOCP, epoll, and kqueue passed the common differential lifecycle on their real
hosts. Backend core v1 and net receive v1 are stable through RFC0003; reference
Provider implementations and the reference awaitable remain experimental.

## Goals

Stage 6 has focused native performance, lifecycle, and portability goals.

- Validate completion and readiness without coupling either model to task or
  executor layout.
- Preserve Stage 5 Waker ownership and lost-wakeup semantics.
- Keep backend control operations owner-thread only.
- Provide a thread-safe interrupt that breaks an owner pump wait.
- Support zero-copy receive into a borrowed pinned buffer.
- Avoid mandatory allocation in submit, cancel, completion, and quiescence.
- Preserve synchronous task drop through a synchronous destruction fence.
- Retain portable C11 public headers and a `wasm32-wasi` conformance path.
- Validate Windows, Linux, and macOS before freezing a common prefix.
- Keep backend-specific handles and operation state opaque.
- Publish reference providers through explicit static descriptors.
- Preserve core ABI v3, static typed await, CFG optimization, and existing
  generated task layout.

## Non-goals

Stage 6 deliberately excludes adjacent runtime and language work.

- Socket creation, connect, accept, send, DNS, TLS, or UDP APIs.
- File I/O, timer, GPU, browser, or WASI host networking backends.
- A general EventSource or one event union for every operation type.
- A dynamic plugin loader, DLL search policy, or package discovery ABI.
- More than one active receive on one imported socket.
- Backend-owned receive buffers or implicit buffer copies.
- Asynchronous drop or suspended cancellation cleanup.
- A stable executor, task, queue, observer, or thread-pool ABI.
- Work stealing or concurrent polling of one task.
- A backend that directly polls or resumes a task.
- New CR source syntax or a compiler-visible scheduling node.
- Direct LLVM IR, native machine-code, or direct WebAssembly emission.

## Architectural layers

The runtime keeps backend behavior below semantic awaitables and above native
operating-system services.

```text
Generated task or executor
        |
        | poll context and Waker
        v
Reference net-receive awaitable
        |
        | semantic receive operation
        v
Versioned net capability extension
        |
        | opaque operation storage
        v
Opaque backend core
        |
        +---- IOCP completion provider
        +---- epoll readiness provider
        +---- kqueue readiness provider
        +---- memory conformance provider
```

The layers have these responsibilities:

- The generated task owns coroutine state and cleanup.
- The executor decides when to poll a ready task.
- The receive awaitable owns readiness, Waker registration, result copying,
  and final readiness checks.
- The net extension owns receive submission, native event interpretation,
  cancellation requests, and quiescence.
- The backend core owns provider lifetime, extension discovery, owner pumping,
  and wait interruption.
- The operating-system provider owns native handles, registrations, completion
  packets, and private operation state.

No layer below the awaitable stores a task or executor pointer. No backend
callback enters generated poll code.

## Stability policy

Stage 6 separates stable v1 prefixes from experimental implementations.

During the first implementation tasks, `cr_backend.h`, `cr_net.h`, Provider
descriptors, extension descriptors, and operation methods were experimental.
Task 9 proved their common behavior across the three native event models.

The final stabilization pass follows these rules:

- Freeze semantic behavior before memory layout.
- Remove methods that only one provider needs.
- Move raw event details into backend-private state or model-specific
  experimental extensions.
- Freeze only versioned prefixes exercised by every required provider.
- Keep all structs append-only after stabilization.
- Keep backend instances, sockets, and operation storage opaque.
- Keep provider implementations and reference awaitables independently
  replaceable.

Task 10 freezes the surviving append-only prefixes and semantics in RFC0003.
Backend instances, receive operations, native events, reference Provider
symbols, and reference awaitable state remain opaque or experimental.

## Backend core

The backend core is an opaque owner-driven service with versioned extension
discovery. RFC0003 stabilizes its v1 public prefixes and observable behavior.

```c
typedef struct cr_backend cr_backend;

typedef struct cr_extension_id {
    uint64_t high;
    uint64_t low;
} cr_extension_id;

typedef struct cr_backend_provider_desc cr_backend_provider_desc;
typedef struct cr_backend_pump_result cr_backend_pump_result;
```

The backend core provides these semantic operations:

- Create one opaque backend from a statically linked provider descriptor.
- Query one extension by 128-bit ID and requested ABI version.
- Pump at most `max_events` with a relative nanosecond timeout.
- Interrupt a blocked pump from any thread.
- Shut down and destroy the backend synchronously on its owner thread.

The backend core doesn't expose an event registration primitive. Registration
belongs to capability extensions such as the net extension.

### Provider descriptors

A provider descriptor has module lifetime and contains a versioned factory.
Stage 6 doesn't define dynamic loading.

Every extensible descriptor starts with:

- `abi_version`.
- `struct_size`.
- capability bits.
- a stable provider or extension identity.

The application explicitly links and selects a provider descriptor. The CR
compiler can package reference providers, but it doesn't discover installed
plugins or choose a provider at runtime.

### Extension discovery

Extension discovery uses a 128-bit identity and requested version. A provider
returns a descriptor pointer with module lifetime when it supports the request.

An unsupported extension is a normal capability miss. It doesn't close the
backend or report an internal failure. The consumer accepts a descriptor when
its ABI version and known minimum prefix are compatible.

Unknown descriptor tail fields and unknown capability bits are ignored. A
consumer can't infer behavior from structure size alone.

### Owner-thread contract

The thread that creates a backend becomes its owner. These operations are
owner-thread only:

- Extension query.
- Receive initialization and submission.
- Cancellation request.
- Pump.
- Quiescence.
- Operation destruction.
- Backend shutdown and destruction.

Calling an owner-only operation from another thread returns a stable error and
performs no partial action.

Only backend interrupt is thread-safe in the common core. Cross-thread receive
submission and cancellation remain future command-queue extensions.

The caller keeps the backend alive while another thread can call interrupt.
Backend shutdown and destruction can't run concurrently with a new interrupt
call unless a future reference-counted interrupt handle defines that lifetime.

### Pump contract

The owner pump accepts a relative `timeout_ns` and `max_events`.

- `timeout_ns == 0` performs a nonblocking pump.
- `timeout_ns == UINT64_MAX` waits without a time limit.
- Other values are relative durations.
- A provider can round the duration upward to its native clock granularity.
- `max_events == 0` is an invalid argument.
- One pump dispatches no more than `max_events` operation or interrupt events.

The pump returns a versioned result record with one reason:

- `Progress`: at least one event was dispatched.
- `Timeout`: no event arrived before the relative timeout.
- `Interrupted`: the thread-safe interrupt broke the wait.
- `Error`: the backend wait or dispatch path failed.

The record also carries the dispatched event count and portable and native
error fields. Timeout and interrupt aren't backend errors.

Reason selection follows this priority:

1. `Error` when wait or dispatch fails, including after earlier records were
   processed in the same pump.
2. `Progress` when at least one operation record was processed and no error
   occurred.
3. `Interrupted` when only interrupt control records were processed.
4. `Timeout` when no record was processed before the timeout.

The event count includes consumed operation and interrupt records. A readiness
record that produces `EAGAIN` still counts toward `max_events`, even though it
doesn't produce a terminal callback. This rule prevents stale or spurious
readiness from bypassing the fairness budget.

### Interrupt contract

Interrupt only makes a blocked owner pump return. It doesn't represent socket
readiness, create a receive completion, or wake a coroutine by itself.

The native implementations use these mechanisms:

- IOCP posts a private control packet.
- epoll uses a private `eventfd` or pipe registration.
- kqueue uses `EVFILT_USER` or a private pipe.
- The memory provider sets a deterministic interrupt record.

Duplicate interrupts can coalesce. Every effective interrupt eventually makes
a progressing owner pump return `Interrupted` or return after reporting prior
progress.

## Net receive extension

The first capability extension validates one-shot receive semantics over a
borrowed connected TCP socket.

The extension doesn't create, connect, accept, or close sockets. Test and
application code creates a connected socket and imports its native handle.

### Native socket handles

The extension imports a typed native handle represented by an explicit handle
kind and a `uintptr_t` value.

The first handle kinds represent:

- A WinSock `SOCKET`.
- A POSIX file descriptor.
- A memory-provider test socket.

The backend borrows the handle. It can associate or register the handle, but it
never closes it. The caller keeps the socket valid until every operation using
it is quiescent.

One imported socket can have at most one active receive in v1. A second submit
returns `Busy` without changing either operation.

### Caller-owned operation storage

The net extension reports required receive-operation storage size and
alignment. The caller provides one correctly aligned opaque region.

The caller can allocate this region from:

- Stack storage with sufficient alignment.
- An arena.
- An object pool.
- Heap storage chosen by the application.

The backend doesn't require a submit-time allocation. It can allocate backend
and socket tracking state during backend creation or explicit initialization.

The public ABI doesn't reserve fixed inline bytes for backend-private state.
This prevents a future backend from exceeding a frozen capacity.

### Borrowed pinned receive buffer

A receive operation borrows `buffer` and `buffer_size` from submit until its
terminal callback and quiescence complete.

The v1 receive contract requires `buffer_size > 0`. A zero-length request is an
invalid argument because it can't be distinguished from an orderly EOF result.

During that interval, the caller must not:

- Move or free the buffer.
- Reuse the buffer for another operation.
- Mutate the buffer concurrently with the backend.
- Close the imported socket.
- Move or free the opaque operation storage.

A successful receive returns the number of bytes written. `Ready` with zero
bytes represents an orderly TCP EOF.

Stage 6 doesn't add an owned-buffer adapter. Applications can build one above
the borrowed operation when they need ownership transfer.

### Operation lifecycle

One operation follows this lifecycle:

```text
Uninitialized
    |
    v
Initialized
    |
    v
Submitted ---------> Cancel requested
    |                      |
    +----------+-----------+
               v
      Ready | Error | Canceled
               |
               v
           Quiescent
               |
               +----> Reinitialized
               |
               v
            Destroyed
```

The lifecycle rules are:

- Submit succeeds at most once per initialization.
- Submit failure leaves the operation quiescent, reports a stable error, and
  produces no completion callback.
- Cancel is valid only for a submitted nonterminal operation.
- Repeated cancel requests before terminal completion are idempotent.
- Cancel requests a terminal outcome but doesn't choose the winner of a race.
- Every successful submit produces exactly one terminal callback.
- Quiesce is valid for submitted, terminal, or already-quiescent operations.
- Operation destruction is valid only after quiescence.
- Reuse requires quiescence followed by explicit reinitialization.

### Completion records

The provider delivers one versioned terminal completion record during owner
pump or quiescence processing.

The record contains:

- ABI version and structure size.
- Terminal kind: `Ready`, `Error`, or `Canceled`.
- `bytes_transferred` for `Ready`.
- A portable error category for `Error`.
- An optional native error domain and code.

The first portable error categories distinguish:

- Invalid argument.
- Unsupported capability.
- Busy operation or socket.
- Out of memory.
- Closed backend.
- Network failure.
- Internal backend failure.

The native fields preserve WinSock, Win32, or errno diagnostics. Portable
control flow depends only on the terminal kind and portable category.

The completion record is borrowed for the callback. A consumer copies any
field it retains.

### Cancellation contract

Cancel is an asynchronous request. It doesn't guarantee that the completion
kind is `Canceled`.

If successful receive, EOF, network error, and cancellation race, exactly one
of these terminal outcomes wins:

- `Ready`.
- `Error`.
- `Canceled`.

The provider can't report more than one terminal callback, revive a terminal
operation, or access the receive buffer after quiescence.

### Quiescence contract

Quiescence is the synchronous destruction fence that preserves Stage 5 drop
semantics with asynchronous native cancellation.

When quiesce returns successfully:

- The terminal callback has been delivered when the operation was submitted.
- The provider no longer references the receive buffer.
- The provider no longer references the socket for that operation.
- The provider no longer references the opaque operation storage.
- No later callback can occur for that initialization generation.

Quiescence can block on the owner thread. It can dispatch native completion
records needed to retire the target operation, but it never polls a task.

When a completion queue yields records for other active operations while the
target is quiescing, the provider can deliver those callbacks and wakes before
continuing to wait for the target. User callback storage for every active
operation remains valid throughout quiescence.

The model-specific behavior is:

- epoll unregisters or disarms the interest and retires stale records.
- kqueue deletes or disables the filter and retires stale records.
- IOCP requests cancellation when needed and drains the matching completion
  before releasing operation storage.
- The memory provider deterministically completes or cancels its queued record.

Backend shutdown synchronously cancels and quiesces every active operation
before freeing backend state.

The caller keeps every active operation storage region, receive buffer, socket,
and callback target alive until backend shutdown or destruction returns.

## Reference receive awaitable

Stage 6 includes one experimental reference awaitable that composes the net
extension with RFC0002 Waker semantics.

The awaitable owns:

- Its readiness and terminal state.
- The retained Waker clone.
- A copied completion record.
- Whether the current operation generation is quiescent.
- The current operation generation.
- The caller-provided operation storage reference.
- The borrowed socket and buffer contract.

The backend operation doesn't own a Waker. It invokes the awaitable's terminal
completion callback on the owner thread. That callback copies the result,
marks the operation ready exactly once, and wakes the retained Waker.

### Poll sequence

The awaitable follows the RFC0002 registration sequence when the poll context
provides a valid Waker:

1. Check terminal readiness.
2. Quiesce a terminal operation that isn't yet quiescent.
3. Clear the Waker registration and return terminal progress only after
   quiescence.
4. Clone and publish the current Waker when the operation isn't ready and the
   poll context provides one.
5. Replace and drop any older Waker registration.
6. Initialize and submit the receive when it hasn't been submitted.
7. Recheck terminal readiness after registration and submit.
8. If the recheck observes terminal state, quiesce before returning it.
9. Return `Pending` with a valid registration on the Waker-aware path.

An immediate submit failure becomes a terminal error without returning
`Pending`.

Successful terminal poll, error propagation, and cancellation propagation all
cross the same quiescence fence. When poll returns terminal progress, user code
can safely reuse the buffer, close the socket after other operations quiesce,
and release the operation storage according to its ownership plan.

Manual polling remains valid when `poll_context == NULL` or no Waker capability
is available. In that path, the awaitable submits and rechecks readiness but
can return `Pending` without a registration. The caller must pump the backend
and poll again explicitly. The net-receive awaitable therefore doesn't require
`CR_POLL_CAP_WAKER` in its dynamic vtable; it uses a valid Waker when one is
available.

### Awaitable drop

Dropping an active receive awaitable performs this sequence synchronously:

1. Make later Waker calls unable to revive the task.
2. Request operation cancellation.
3. Quiesce the operation.
4. Drop the retained Waker clone.
5. Destroy or release the operation storage according to caller ownership.

A completion callback that occurs during quiescence can wake the retained
Waker. Stage 5 late-wake semantics make that wake a safe no-op after executor
cancellation has deactivated the task.

The awaitable doesn't suspend during drop and doesn't require async cleanup.

## Native provider behavior

Each native provider implements the same receive lifecycle while preserving
its own event model.

### Windows IOCP provider

The IOCP provider represents the completion model.

It uses:

- `CreateIoCompletionPort` to own the completion queue and associate sockets.
- An overlapped WinSock receive operation.
- `GetQueuedCompletionStatus` or an equivalent batch API for owner pumping.
- `CancelIoEx` for cancellation requests.
- `PostQueuedCompletionStatus` for pump interruption.

The operation remains live until its completion packet is drained. Quiescence
can't free the overlapped state merely because cancellation was requested.

The provider reports bytes and native WinSock or Win32 diagnostics through the
common completion record.

### Linux epoll provider

The epoll provider represents readiness.

It uses:

- A nonblocking connected socket.
- `epoll` registration with one active receive per socket.
- One-shot or explicitly rearmed readiness.
- A nonblocking `recv` after readable readiness.
- A private `eventfd` or pipe for interrupt.

Readable readiness followed by `EAGAIN` is not terminal. The provider rearms
the operation and waits for another readiness event.

Cancellation removes or disarms the registration. Generation identity prevents
stale epoll records from completing reused storage.

### macOS kqueue provider

The kqueue provider is a second readiness implementation and a separate
portability gate.

It uses:

- A nonblocking connected socket.
- `EVFILT_READ` registration.
- One-shot or explicitly rearmed filters.
- A nonblocking `recv` after readable readiness.
- `EVFILT_USER` or a private pipe for interrupt.

Readiness followed by `EAGAIN` remains pending. Filter deletion, stale event
retirement, EOF flags, and generation identity receive independent tests rather
than inheriting epoll assumptions.

## Memory conformance provider

The memory provider validates portable ABI and lifecycle behavior without
claiming to model native networking.

It uses ordinary C11 state and deterministic test hooks. It supports:

- Extension discovery.
- Storage layout queries.
- Memory test handles.
- Receive submit.
- Controlled completion.
- Cancellation races.
- Pump timeout, progress, and interrupt.
- Quiescence and backend shutdown.

The provider creates no native thread and imports no native socket or event
API. It can use portable C11 atomics only for the thread-safe interrupt state.
Stage 5's single-thread executor remains atomics-free.

The provider compiles and links for `wasm32-wasi` without shared memory or the
WebAssembly threads feature. The WASI gate verifies that portable C11 atomic
source use doesn't turn into a shared-memory requirement.

This provider doesn't make browser or WASI host networking part of Stage 6.

## Project configuration and artifacts

Project packaging uses a list because future applications can link more than
one provider family.

```toml
[runtime]
executor = "manual"
backends = []
```

The first compiler-owned selections are:

```toml
backends = ["memory-conformance"]
backends = ["native-net"]
```

Selection follows these rules:

- An empty list publishes no Stage 6 provider implementation.
- `memory-conformance` publishes the portable conformance provider.
- `native-net` selects IOCP on Windows.
- `native-net` selects epoll on Linux.
- `native-net` selects kqueue on macOS.
- `native-net` is rejected for `wasm32-wasi` and unknown custom targets.
- Duplicate backend selections are rejected.
- Shared core headers and sources are published once when multiple selections
  require them.
- Artifact ordering is deterministic.
- A failed selection or build preserves the last complete published artifact
  set.

The stable public and experimental implementation artifact set can include:

- `include/cr_backend.h`.
- `include/cr_net.h`.
- Internal backend and net headers under `runtime/`.
- A shared backend core source.
- A shared reference net-receive awaitable source.
- One selected provider source.

The memory provider remains portable C11. Native provider artifacts are absent,
not conditionally executed, on unsupported targets.

Build-system integration adds only selected dependencies:

- Windows net projects link WinSock.
- Linux epoll projects add no thread dependency for the provider itself.
- macOS kqueue projects use system interfaces without a background thread.
- WASI memory projects add no native thread, socket, or atomic dependency.

## Compatibility boundary

Stage 6 extends the runtime without changing established coroutine contracts.

These invariants remain fixed:

- `CR_RUNTIME_ABI_VERSION` remains `3`.
- `cr_runtime.h` remains byte-compatible with Stage 4 and Stage 5.
- Waker v1 remains stable and unchanged.
- `cr_poll_context` keeps only the known Waker capability.
- Manual null-context polling remains valid.
- Static children remain typed and directly polled.
- Dynamic awaitables remain the type-erased boundary.
- Synchronous drop and cleanup ownership remain unchanged.
- Reference executor APIs and layouts remain experimental.
- Backend code never requires concurrent polling of one task.

Stage 6 doesn't add a backend pointer to `cr_waker`, `cr_poll_context`,
generated task contexts, or the stable awaitable vtable.

## Diagnostics and failure behavior

The implementation reports contract violations before unsafe native access.

Required diagnostics include:

- Unsupported backend selection for the configured target.
- Duplicate or unknown compiler-owned backend selection.
- Unsupported extension ID or requested ABI version.
- Incompatible descriptor prefix.
- Invalid operation storage size or alignment.
- Wrong-thread owner-only operation.
- Unsupported native handle kind.
- Invalid or closed native socket handle when detectable.
- Zero-sized receive buffer when forbidden by the selected operation contract.
- Second active receive on one socket.
- Submit after submit without reinitialization.
- Reuse or destruction before quiescence.
- Backend shutdown after partial teardown.
- Pump with `max_events == 0`.
- Timeout conversion overflow.
- Provider error without a portable category.

Errors don't partially submit an operation or consume caller-owned storage.
Sticky terminal behavior remains in the reference awaitable.

## Verification strategy

Stage 6 uses the same semantic cases across every provider and adds native
model-specific cases.

### Common provider conformance

Every provider must pass these cases:

- Extension query success and capability miss.
- Older known prefix acceptance and truncated prefix rejection.
- Unknown tail and capability tolerance.
- Storage size and alignment validation.
- Submit and terminal callback exactly once.
- Data ready before submit.
- Data ready after submit and before pump.
- Data ready while the owner blocks in pump.
- Duplicate native notifications.
- Success, error, EOF, and cancellation outcomes.
- Cancel racing success and error.
- Cancel followed by quiescence.
- Terminal completion queued while awaitable drop begins.
- No callback after quiescence.
- Guard bytes unchanged outside the receive range.
- Second receive on one socket rejected as busy.
- Interrupt breaking an infinite wait.
- Duplicate interrupt coalescing.
- `max_events` fairness.
- Backend destruction with active operations.
- Complete allocation and handle cleanup.

Tests use barriers, hooks, socket handoff, and explicit state observations. They
don't use sleep duration as correctness evidence.

### Readiness-specific conformance

epoll and kqueue additionally prove readiness semantics.

- Readable readiness followed by `EAGAIN` remains pending.
- Rearming doesn't lose data that arrives at each registration boundary.
- Stale readiness from an older generation can't complete reused storage.
- EOF flags and zero-byte receive agree.
- Cancellation removes or disables the native interest before quiescence.

### Completion-specific conformance

IOCP additionally proves completion semantics.

- Immediate and deferred overlapped completion both deliver one callback.
- `CancelIoEx` doesn't permit operation storage to die before packet drain.
- A completion packet queued before cancellation can win as `Ready`.
- A canceled operation can complete as `Canceled` exactly once.
- Private interrupt packets can't be mistaken for receive completions.

### Waker and executor composition

The reference awaitable runs through manual polling, the single-thread
executor, and the native-threaded executor where supported.

Tests prove:

- Waker registration follows RFC0002.
- A backend callback only wakes and never polls.
- Duplicate completion wake calls coalesce safely.
- Cancellation deactivates the task before drop-driven quiescence.
- A wake during quiescence is a safe late wake.
- Task payload, operation storage, buffer, socket, and backend lifetimes remain
  separate.

### Allocation and performance evidence

Allocator hooks and counters record hot-path behavior.

The required properties are:

- Operation storage is caller allocated after a size/alignment query.
- Submit doesn't require allocation.
- Cancel doesn't require allocation.
- Native completion dispatch doesn't require allocation.
- Quiescence doesn't require allocation.
- Backend creation and explicit tracking setup account for every allocation.
- Backend destruction releases every allocation and native handle.

Performance records include system-call count, dispatched events, wake calls,
queue coalescing, and allocation count. Stage 6 doesn't freeze specific
benchmark numbers as ABI requirements.

### WebAssembly conformance

The memory provider and public headers compile as portable C11 for
`wasm32-wasi`.

The required gate:

- Selects `memory-conformance` before CR compilation.
- Compiles every generated and runtime source with pinned WASI Clang.
- Denies C warnings.
- Links the final module.
- Validates it with pinned `wasm-tools`.
- Rejects IOCP, epoll, and kqueue artifacts.
- Rejects native threads, shared WebAssembly memory, WebAssembly thread
  features, and native socket headers.
- Exercises pump, interrupt, completion, cancel, and quiescence state machines.

Native execution remains the behavioral gate for real sockets. Browser and
WASI host networking remain future provider projects.

### Existing regression gates

Every implementation task continues to pass the established project gates.

```text
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
pnpm run grammar:test
native generated-project compile and run
CMake and Meson generated-project compile and run
required wasm32-wasi compile, link, and validation
ABI v3 and malformed-object conformance
Stage 3 static-await regressions
Stage 4 CFG and context-layout regressions
Stage 5 Waker, executor, lifetime, and race regressions
frozen Stage 4 object linked into the current runtime
```

## Staged implementation

Stage 6 keeps contract, portable state, and native providers isolated so one
model can't silently define the shared interface.

1. Freeze this approved design and create the detailed implementation plan.
2. Add experimental backend identities, records, state validation, and target
   selection without publishing native behavior.
3. Add the portable memory provider and required native and WASI conformance.
4. Add the experimental reference receive awaitable and quiescence lifecycle.
5. Add the Windows IOCP provider and deterministic completion tests.
6. Add the Linux epoll provider and deterministic readiness tests.
7. Add the macOS kqueue provider and independent readiness tests.
8. Run cross-provider differential, cancellation, destruction, and allocation
   analysis.
9. Remove noncommon operations and write the stable backend-core RFC candidate.
10. Publish selected artifacts and complete CMake and Meson integration.
11. Run every Stage 0 through Stage 6 native, ABI, race, and WASI gate.

No native provider begins before the memory provider and reference awaitable
prove the common state machine. No prefix becomes stable before all three
native providers pass their supported-host gates.

## Stop conditions

Stop and amend the design instead of broadening implementation when any of
these conditions occurs:

- Correctness requires changing core ABI v3 or Waker v1.
- A backend needs a task or executor pointer in the common ABI.
- A backend must directly poll or resume a coroutine.
- Correctness requires concurrent polling of one task.
- Safe cancellation requires asynchronous drop.
- Quiescence can't guarantee that borrowed storage and buffers are unreferenced.
- Receive correctness requires backend-owned copies in the mandatory path.
- A fixed public inline storage capacity becomes necessary.
- IOCP, epoll, and kqueue can't share an honest completion record.
- One provider requires raw event fields in the common prefix.
- Portable headers require native socket, thread, or atomic declarations.
- `wasm32-wasi` requires a native provider or WebAssembly threads.
- Correctness tests require sleeps or machine timing.
- More than one active receive per socket becomes necessary for basic proof.
- Socket creation, connect, accept, send, DNS, TLS, timer, or file I/O becomes
  necessary to validate receive semantics.
- Dynamic plugin loading becomes necessary to validate static providers.

## Acceptance criteria

Stage 6 completes only when backend validation supports these conclusions:

- IOCP, epoll, and kqueue implement the same one-shot receive lifecycle.
- Readiness and completion remain private provider details.
- The common backend prefix contains only operations used by every provider.
- Extension discovery is versioned and independent of provider layout.
- Pump timeout, interrupt, progress, and error semantics are deterministic.
- Borrowed pinned buffers remain valid through terminal completion and
  quiescence.
- Cancellation produces exactly one terminal outcome.
- Quiescence preserves synchronous task drop and prevents later native access.
- The reference awaitable owns Waker registration and readiness.
- Submit, cancel, completion, and quiescence don't require allocation.
- Manual projects retain their existing output and runtime cost.
- Native projects publish exactly one target provider.
- WASI projects compile the memory provider and no native provider.
- Core ABI v3, Waker v1, static dispatch, CFG optimization, and frozen-object
  compatibility remain unchanged.
- A final RFC documents the stable semantic prefix or explicitly records why
  stabilization is deferred.

## Next steps

After review and user approval of this written specification, create the Stage
6 implementation plan. The first implementation task must add experimental
identities and conformance records without publishing a stable backend ABI.
