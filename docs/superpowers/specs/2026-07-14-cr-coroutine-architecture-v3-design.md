# CR coroutine architecture and ABI v3 design

This document defines the approved direction for CR coroutine lowering and its
next public runtime ABI. CR remains a native-first source-to-source compiler,
while portable C11 output must remain compilable for WebAssembly targets.

> **Note:** Core ABI v3 is implemented and passes its native and WebAssembly
> conformance gates. Static-await dispatch and scheduling extensions remain
> staged work.

## Decision summary

CR adopts a layered architecture that keeps compiler-private optimization
separate from the stable runtime boundary. The design makes the following
decisions:

- ABI v2 is removed and has no compatibility path in production code.
- Native execution and native performance remain the primary target.
- Portable C11 output must compile for supported WebAssembly toolchains.
- A statically resolved await uses direct typed dispatch.
- Only a genuinely dynamic await uses the type-erased runtime ABI.
- Await target resolution and storage planning remain orthogonal CIR concepts.
- Task, context, embedded slot, and CIR layouts remain compiler-private.
- Public poll states, ownership, cancellation, and drop behavior are stable
  semantic contracts.
- A task can't be polled concurrently or reentrantly.
- Core ABI v3 doesn't require threads, atomics, TLS, or an operating-system
  event API.
- Waker, executor, and reactor interfaces remain capability-based extensions.
- `__defer` and task drop are synchronous and can't suspend.
- Full SSA, Memory SSA, a stable reactor SPI, and a direct WebAssembly backend
  are outside the initial ABI v3 migration.

## Scope and compatibility

The migration improves coroutine representation, generated task calls, and the
dynamic await boundary without replacing the current front end or scoped CFG.

### Goals

The implementation must meet these goals:

- Preserve the current Tree-sitter, identity HIR, scoped CFG, scope-exit,
  coroutine, liveness, and C emission architecture.
- Remove allocation and dynamic dispatch from eligible static awaits.
- Support finite task layouts for recursive and mutually recursive async calls.
- Preserve errors, cancellation, yields, and exactly-once cleanup behavior.
- Add coroutine-specific CFG optimization without requiring full SSA.
- Define a versioned dynamic awaitable that can evolve through vtable prefixes
  and capability negotiation.
- Keep the core runtime valid for native C11 and C-to-WebAssembly toolchains.
- Establish executable C conformance tests for every stable ABI behavior.

### Non-goals

The initial migration doesn't include these features:

- A stable IOCP, libuv, epoll, kqueue, GPU, or browser event SPI.
- A general-purpose native optimizer that duplicates Clang, GCC, or MSVC.
- Full SSA or Memory SSA for arbitrary C source.
- A stable generator or stream ABI with different yield and return types.
- A direct LLVM IR, native machine code, or WebAssembly emitter.
- Binary compatibility between different target data models, such as native
  64-bit C and `wasm32`.

### ABI migration summary

ABI v3 replaces ABI v2 as one explicit pre-1.0 compatibility break. This
matrix defines the public migration surface:

| Surface | ABI v2 legacy | ABI v3 implemented |
| --- | --- | --- |
| Poll status | C enum | Fixed `uint32_t` constants |
| Task poll | `poll(task)` | `poll(task, poll_context)` |
| Awaitable | State, callbacks, layout, flags | State and vtable pointer |
| Callback metadata | Repeated per object | Shared versioned vtable |
| Layout check | Before every poll | Once at opaque-slot activation |
| Borrowed adapter | `*_as_awaitable` | Retained with v3 vtable |
| Owning adapter | `*_into_awaitable` | Retained with v3 vtable |
| Public task layout | Opaque | Opaque |
| Internal context | Compiler-private | Compiler-private |

## Layered architecture

CR separates language semantics, compiler-private lowering, the portable core
ABI, and optional scheduling capabilities.

```text
CR language contract
  -> Tree-sitter CST
  -> identity-based HIR
  -> scoped CFG
  -> scope-exit and defer lowering
  -> module symbol index and static await graph
  -> call graph and SCC analysis
  -> await target resolution and storage planning
  -> coroutine IR and liveness
  -> coroutine CFG optimization
  -> portable C11 or native computed-goto C

Static target                         Dynamic target
  -> embedded or boxed typed slot       -> opaque await slot
  -> direct init/poll/drop               -> ABI v3 vtable poll/drop

Portable core ABI v3
  -> optional executor and waker extension
  -> experimental native or WebAssembly host adapters
```

The language contract and public core ABI are versioned. The compiler can
change task fields, child storage, block numbering, and resume-state layout
without creating a public ABI break.

## Coroutine IR

The existing scoped CFG remains the control-flow foundation. Coroutine
lowering adds stable await-edge identities and planning data instead of
encoding storage policy in source expressions or C emission.

### Await representation

An await keeps target identity separate from storage policy. The conceptual
representation is:

```rust
struct AwaitTerminator {
    id: AwaitEdgeId,
    instance: ChildInstanceId,
    continuation: BlockId,
    span: SourceSpan,
}

struct ChildInstance {
    id: ChildInstanceId,
    origin: ChildOrigin,
    target: AwaitTarget,
    storage: AwaitStorage,
    result: ResultLayout,
    ownership: AwaitOwnership,
}

enum ChildOrigin {
    Direct(AwaitEdgeId),
    Binding(DeclarationId),
}

enum AwaitTarget {
    Static(FunctionId),
    Dynamic(ValueId),
}

enum AwaitStorage {
    Unplanned,
    Embedded(ChildSlotId),
    Boxed(TypedSlotId),
    Opaque(AwaitSlotId),
}
```

`Static` identifies a generated async function even when storage must remain
boxed. `Dynamic` means that the compiler can't select one concrete poll symbol.
The storage planner replaces `Unplanned` before C emission. Target and storage
remain orthogonal properties of each child instance.

### Task bindings and child-instance ownership

A source declaration such as `__async R task = work(args);` owns one child
instance. The declaration activates that instance when control executes the
declaration, and every `__await task` edge references the same
`ChildInstanceId`.

Task-binding behavior follows these rules:

- A direct `__await work(args)` creates an edge-owned child instance on the
  first entry to that await edge.
- A task binding creates a declaration-owned child instance when its
  declaration executes, even if no later await reaches it.
- `ChildInstanceId` identifies a compile-time storage site. An activation flag
  and monotonically conceptual generation distinguish dynamic executions of
  that site; the generation doesn't need to occupy a runtime field.
- Multiple awaits of one binding poll the same child instance. Sticky terminal
  behavior returns the same terminal value without reinitializing the child.
- Awaiting a declaration-owned child doesn't destroy it at the await edge.
- The binding's lexical scope exit finalizes its child exactly once, whether it
  was never awaited, remains pending, or reached a terminal state.
- Conditional declarations carry an activation flag so an unexecuted binding
  isn't finalized.
- Reexecuting a binding declaration first finalizes an active previous
  generation, then initializes a new generation in the same storage.
- Awaiting an inactive binding produces a parent protocol error instead of
  reading uninitialized storage. This covers a goto that skipped activation.
- A boxed binding retains its typed pointer until scope exit. An embedded
  binding retains its compiler-private child context in the parent.

Storage planning is therefore per `ChildInstanceId`. Await-edge identities
remain necessary for control flow, diagnostics, call-graph edges, and direct
edge-owned instances, but they don't independently own declaration storage.

### Module symbol index

Project compilation builds an async symbol index before lowering individual
translation units. The index assigns stable `FunctionId` values and records
whether each context layout is visible in the current translation unit.

The first implementation must resolve these cases:

- A definition in the current translation unit has a visible context layout.
- A declaration from a generated CR header has an opaque context layout but
  known typed create, poll, result, error, and destroy symbols.
- An ordinary expression with no resolved CR async target remains dynamic.
- Standalone `compile_source` builds a local index and doesn't assume project
  metadata that the caller didn't provide.

### Call graph and cycle breaking

The compiler builds a graph of static child-instantiation edges and runs Tarjan
SCC analysis. Storage planning happens per `ChildInstanceId`, not per function.

The planner applies these rules in order:

1. Select `Opaque` for a dynamic target.
2. Select `Boxed` when a static target has an opaque cross-translation-unit
   context layout.
3. Select enough deterministic static edges inside each cyclic SCC as `Boxed`
   to make the embedded-layout graph acyclic.
4. Select `Embedded` for the remaining static edges with visible layouts.
5. Permit later policy passes to box additional edges for context-size or
   target-specific reasons without changing target dispatch.

Cycle breaking doesn't promise a globally minimum feedback-edge set. The
planner first removes instances that already require boxed or opaque storage.
It then considers remaining static instances in this stable key order:

```text
normalized caller project path
caller function source start
child-instance source start
normalized callee symbol key
```

The planner adds each candidate to the embedded graph only when that addition
doesn't create a cycle; otherwise it selects `Boxed`. This produces a
deterministic maximal acyclic embedded subgraph. It isn't required to find the
maximum possible subgraph. A simple recursive function therefore contains a
typed pointer to its recursive child instead of embedding its own context.

### Static dispatch

Both embedded and boxed static targets use direct generated symbols. Storage
choice doesn't force dynamic dispatch.

An embedded child follows this conceptual sequence:

```c
child_init(&parent->child_slot, arguments);
status = child_poll(&parent->child_slot, poll_context);
child_drop(&parent->child_slot);
```

A boxed child uses typed allocation and public or internal entry points:

```c
parent->child = child_create(arguments, &parent->error);
status = child_poll(parent->child, poll_context);
child_destroy(parent->child);
```

The generated parent retains each child generation across pending polls. An
edge-owned direct child is finalized and marked inactive when it reaches any
terminal status. Reentering that direct await edge, such as on the next loop
iteration, creates a new generation. A declaration-owned binding remains alive
after ready so later awaits observe its sticky terminal result; its scope exit
finalizes it. Error and cancellation leave the parent through scope cleanup,
which also finalizes the binding. A failed boxed create becomes a parent error
without polling or dropping a null child.

### Coroutine CFG optimizer

The optimizer contains focused passes whose correctness can be tested
independently. It doesn't perform general C alias or scalar optimization.

The initial pass order is:

1. Remove unreachable blocks and matching resume states.
2. Forward trivial goto blocks when scope and cleanup semantics permit it.
3. Merge empty blocks with compatible scope stacks and source mappings.
4. Compact resume-state numbering.
5. Remove state stores proven redundant by resume-edge analysis.
6. Reuse await slots whose active lifetimes don't interfere.
7. Recompute context fields from liveness after CFG changes.

Every pass must preserve C evaluation order, `volatile` behavior, source spans,
cleanup edges, active-child ownership, and exactly-once defer execution. Generic
scalar optimization remains the downstream C compiler's responsibility.

## Core runtime ABI v3

Core ABI v3 defines the minimum portable runtime contract. This section fixes
the observable semantics; final C declarations must receive executable ABI
layout tests before the version is declared stable.

### Versioning and target data models

ABI v3 uses fixed-width integer types for status values, error codes, versions,
flags, and capability bits. Object sizes and alignments use the target C data
model. Binary compatibility is only promised for matching target triples,
calling conventions, data models, and ABI versions.

Public extensible tables begin with an ABI version and structure size. A
consumer must ignore unknown trailing fields and reject an unsupported required
capability. Generated C retains a compile-time core ABI version guard.

ABI v3 uses these compatibility rules:

- `CR_RUNTIME_ABI_VERSION` must equal `3` for generated core code.
- Each extensible structure has its own version constant and minimum prefix
  size.
- A consumer accepts any append-only revision at or above its minimum supported
  revision when `struct_size` covers the complete required prefix it uses.
- A consumer ignores fields beyond the implemented prefix.
- A provider advertises behavior through provided flags.
- A provider lists required poll-context features separately through required
  capability bits.
- A consumer rejects unknown required capabilities before the first poll.
- Unknown provided flags don't imply a requirement and can be ignored.

The first vtable version uses
`CR_AWAITABLE_VTABLE_ABI_VERSION == 1`. Its minimum-size macro covers the bytes
through `value_align`. The first context version uses
`CR_POLL_CONTEXT_ABI_VERSION == 1`, with a minimum-size macro that covers the
bytes through `waker`.

Core ABI v3 defines the error record exactly as follows:

```c
typedef struct cr_error {
    int32_t code;
    const char *message;
} cr_error;
```

The target C ABI determines pointer size and structure padding. Conformance
fixtures record `sizeof`, `_Alignof`, and member offsets for every public
non-opaque structure on each supported target data model.

### Poll status

`cr_poll_status` has a fixed 32-bit representation and stable numeric values.
It exposes these states:

```c
typedef uint32_t cr_poll_status;

#define CR_POLL_PENDING  0u
#define CR_POLL_YIELDED  1u
#define CR_POLL_READY    2u
#define CR_POLL_ERROR    3u
#define CR_POLL_CANCELED 4u
```

The state lifecycle is:

```text
Created -> Pending* <-> Yielded -> Ready
                     \-> Error
                     \-> Canceled
```

`READY`, `ERROR`, and `CANCELED` are sticky terminal states. Polling valid task
storage after a terminal result returns the same status without side effects.
`YIELDED` is transient, and the next poll resumes execution. An error or
cancellation can't transition back to pending.

### Generated public task API

Generated headers expose opaque task types. Context layout and internal
init/drop functions remain private to the defining translation unit.

The public shape for `__async R work(T value)` is:

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

A null `poll_context` selects manual polling with no registered waker. A null
task returns `CR_POLL_ERROR`; it doesn't create an inspectable task error.
Applications must call the matching accessor only for its status.

A `void` task omits both result and yielded accessors. The borrowed
`*_as_awaitable` adapter retains caller-owned task storage. Its vtable drop
cancels active work but doesn't free storage. The owning
`*_into_awaitable` adapter consumes a heap task returned by create. Its vtable
drop finalizes and frees that task.

Concurrent or reentrant poll is an unchecked caller contract violation. Core
ABI v3 doesn't require an in-task guard, error status, or synchronization for
that misuse. Debug runtimes can diagnose it, but generated release layouts don't
reserve a guard field. This rule doesn't apply to repeated sequential terminal
polls, which remain valid and sticky.

The CR ABI leaves behavior undefined after a concurrent or reentrant poll
violation. Implementations must remain memory-safe for calls that satisfy the
nonconcurrency precondition; they don't need to recover from violating calls.

### Drop, destroy, and cancellation

Task drop is synchronous, non-suspending, and idempotent while task storage
remains valid. Public destroy finalizes and frees a heap task and can be called
only once.

Dropping a pending or yielded task performs these actions:

1. Finalize the currently pending edge-owned child, if one exists.
2. Execute declaration-owned child cleanups and `__defer` callbacks in one
   reverse dynamic-activation order.
3. Mark retained task storage as canceled.
4. Return without scheduling more asynchronous work.

Task-binding activation registers a lexical cleanup action at the same point
where a defer registration would enter the dynamic cleanup order. Reexecuting a
binding first removes and runs its previous generation's cleanup before
registering the new generation. Normal scope exit, return, error propagation,
cancellation, and parent drop all use the same LIFO lexical cleanup order.

An edge-owned direct child isn't a lexical resource. If it remains active, it
is the operation at the current suspension point and therefore activated after
all lexical cleanups that can be reached before that suspension. Finalizing it
before the lexical cleanup stack preserves reverse activation order.

Dropping a ready or error task doesn't replace its terminal status. Polling or
accessing a task after public destroy is invalid because its storage no longer
exists.

`__defer` can't contain a suspension. An operation that requires asynchronous
shutdown must expose explicit asynchronous control flow before task drop. A
future graceful-cancellation feature can poll an explicit cleanup path, but
hard drop always remains synchronous.

### Dynamic awaitable

A dynamic awaitable is a move-only pair of state and a shared versioned vtable.
Per-type callbacks and result layout move out of each awaitable instance.

The conceptual ABI is:

```c
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

#define CR_AWAITABLE_CAN_YIELD UINT64_C(1)
#define CR_POLL_CAP_WAKER      UINT64_C(1)
```

The vtable v1 minimum prefix ends after `value_align`. A consumer rejects a
vtable whose version isn't supported or whose `struct_size` doesn't cover that
prefix. Later versions can append fields but can't reorder or reinterpret the
v1 prefix. The awaitable object remains exactly two machine words in ABI v3.
Borrowed and owning adapters use different vtables when drop behavior differs.

An awaitable transfers into one task slot and can't be copied or polled
independently after transfer. The parent invokes its vtable drop at most once.
The callback itself doesn't need to tolerate a second invocation after freeing
state. Parent active-slot state provides exactly-once behavior.

After an error poll, the parent copies the reported `cr_error` before invoking
vtable drop. A null error callback or null error pointer after
`CR_POLL_ERROR` is a protocol error with a compiler-defined fallback error.

`value_size` and `value_align` validate layout, not semantic C type identity.
The compiler checks layout once when activating an opaque slot, before the first
poll. A future capability can add stronger type identity without enlarging the
two-word awaitable object.

### Poll context and waker extension point

Core ABI v3 reserves a versioned `cr_poll_context` argument so adding a waker
doesn't require another public poll signature break. The core permits a null
context and doesn't require a waker implementation.

The v1 context prefix is:

```c
typedef struct cr_waker cr_waker;

typedef struct cr_poll_context {
    uint32_t abi_version;
    uint32_t struct_size;
    uint64_t available_capabilities;
    const cr_waker *waker;
} cr_poll_context;
```

`CR_POLL_CONTEXT_ABI_VERSION` is `1`, and its minimum v1 size ends after
`waker`. The context is borrowed and remains valid only for the duration of one
poll call. A null context has zero available capabilities and no waker. Before
polling an opaque awaitable, generated code verifies that every bit in
`required_context_capabilities` is present in `available_capabilities`.

The `cr_waker` declaration remains opaque in the core header until its separate
RFC defines a versioned vtable. Core v3 code can pass the borrowed pointer
through child polls without inspecting it.

A later waker RFC must define these observable behaviors:

- Wake requests a future poll and never directly reenters the same task.
- Duplicate and spurious wakes are valid and can be coalesced.
- Registration followed by a readiness recheck prevents lost wakeups.
- Published readiness becomes visible to the poll scheduled by its wake.
- Waker clone and drop rules keep callback state alive.
- Cross-thread wake is available only when the waker advertises that
  capability.

The RFC must not prescribe one lock-free state machine. A single-thread native
or WebAssembly executor can use ordinary queues, while a native thread-pool
executor can use atomics internally.

### ABI validation failures

Generated code converts a structurally invalid core object into a sticky parent
error before executing user work. Validation uses these stable error codes:

```c
#define CR_ERROR_INVALID_POLL_CONTEXT       1101
#define CR_ERROR_INVALID_AWAITABLE_ABI       1102
#define CR_ERROR_MISSING_AWAITABLE_CALLBACK  1103
#define CR_ERROR_MISSING_POLL_CAPABILITY     1104
#define CR_ERROR_AWAITABLE_LAYOUT_MISMATCH   1105
#define CR_ERROR_INVALID_POLL_STATUS         1106
```

Public task poll first returns an existing sticky terminal status. Otherwise it
validates a non-null context before entering the task state machine. A context
version below v1 or a `struct_size` shorter than the v1 prefix produces
`CR_ERROR_INVALID_POLL_CONTEXT`, runs the task's normal error cleanup path, and
returns sticky `CR_POLL_ERROR`.

Opaque awaitable activation applies this order:

1. Reject a null vtable, a version below v1, or a structure shorter than the v1
   prefix through `drop` with `CR_ERROR_INVALID_AWAITABLE_ABI`. Don't read or
   invoke any callback because a compatible callback offset isn't established.
2. Reject a recognized append-only version whose structure reaches `drop` but
   remains shorter than the complete v1 minimum with
   `CR_ERROR_INVALID_AWAITABLE_ABI`. Invoke `drop` only when that known v1
   callback is non-null.
3. Reject a null mandatory `poll` or `drop` callback with
   `CR_ERROR_MISSING_AWAITABLE_CALLBACK`. Invoke `drop` only when the validated
   prefix contains a non-null drop callback.
4. Reject an incompatible result layout with
   `CR_ERROR_AWAITABLE_LAYOUT_MISMATCH`, then invoke the validated drop
   callback.
5. Mark the slot active only after structural validation succeeds.

Before every callback poll, including resumed polls of an active slot, generated
code verifies `required_context_capabilities` against the current poll context.
A missing bit produces `CR_ERROR_MISSING_POLL_CAPABILITY`, invokes the validated
drop exactly once, and enters the parent's normal sticky-error path.

A null vtable or unusable prefix is a provider protocol defect. The parent
enters sticky error without dereferencing the invalid table. It can't safely
release unknown state, so state recovery remains the malformed provider's
responsibility. All well-formed vtables must provide non-null poll and drop
callbacks, including a no-op drop when no resource release is needed.

If a validated callback returns an unknown poll status, the parent records
`CR_ERROR_INVALID_POLL_STATUS`, invokes drop exactly once, runs normal error
cleanup, and becomes sticky error. The error callback is optional, but a
validated awaitable that returns `CR_POLL_ERROR` without a non-null error record
uses a compiler-defined static fallback message.

### Errors and allocation ownership

Errors use fixed numeric codes and immutable UTF-8 messages with module
lifetime. Every `cr_error.message` returned through the core ABI must remain
valid until its defining native module or WebAssembly instance is unloaded.
This lets a parent copy `cr_error` by value before dropping a failed child.
Dynamic operation-specific data belongs in numeric codes or a future versioned
error-detail extension; ABI v3 doesn't expose borrowed dynamic message buffers.

Memory must be released by the module or adapter that allocated it. Public
create/destroy pairs don't transfer raw allocation responsibility to callers.
An owning dynamic awaitable frees through its vtable drop; a borrowed adapter
doesn't free caller-owned task storage.

## Native and WebAssembly portability

Native performance is the primary optimization target, but the portable core
must remain acceptable input for C-to-WebAssembly toolchains.

Portable core code must not require these facilities:

- GNU computed goto.
- Native thread creation or thread-local state.
- C11 atomics when no thread-safe capability is selected.
- OS file descriptors, handles, sockets, or event loops.
- `setjmp` or `longjmp` for coroutine suspension.
- A stable host pointer representation across the WebAssembly boundary.

The portable backend uses standard C11 switch and goto control flow. Native
builds can continue to select computed goto. WebAssembly host integration uses
adapters or imported functions; raw C vtable pointers don't cross into
JavaScript as a public host ABI.

The required WebAssembly gate uses a pinned WASI SDK Clang release and the
`wasm32-wasi` target. Stage 0 adds a repository-owned version file and CI setup
for that release. The fixture is the generated portable C11 example, including
its generated CR source, public driver, and runtime header.

The pins live at `tools/wasi-sdk.version` and `tools/wasm-tools.version`. Stage
0 can't complete until both files contain exact tested versions and CI installs
those versions rather than an unpinned latest release.

The gate is equivalent to this command shape:

```text
$WASI_SDK_PATH/bin/clang
  --target=wasm32-wasi
  --sysroot=$WASI_SDK_PATH/share/wasi-sysroot
  -std=c11
  -Icrc/dist/include
  crc/dist/*.c src/main.c
  -o crc-demo.wasm
```

CI then validates the linked module with pinned `wasm-tools`. Native execution
tests remain the primary behavioral gate. A `wasmtime` execution test is added
when the fixture's console contract and pinned runtime are available. Browser
and Emscripten builds aren't initial release gates.

## Optional runtime capabilities

Executor, waker, and reactor support evolves above the portable core. These
interfaces remain experimental until multiple distinct backends validate them.

The stabilization requirements are:

- Implement and test the waker contract with both manual and queued polling.
- Validate a native cross-thread wake path without permitting concurrent task
  poll.
- Validate at least two backend models with materially different completion
  behavior before stabilizing a reactor SPI.
- Include ABI version, structure size, and capability negotiation in every
  extensible backend descriptor.
- Keep backend-specific handles and operation layouts opaque.

Readiness, completion, channel, join, race, and generator semantics don't
become waker modes. Each operation owns its event semantics; the waker only
requests another poll.

## Diagnostics and failure behavior

The migration must fail explicitly when it can't preserve the contract. It
must not silently convert an invalid static plan into unsafe generated C.

Required diagnostics include:

- An unresolved static async declaration that lacks callable public symbols.
- An impossible or incomplete child ownership plan.
- An unsupported dynamic awaitable ABI version or required capability.
- A result-layout mismatch before the first dynamic poll.
- A failed boxed child allocation propagated into the parent error.
- An unsupported WebAssembly backend selection, such as computed goto.

Compiler diagnostics retain source spans through call-graph, planning, and CFG
optimization passes. A failed project build keeps the last complete published
artifact set.

## Verification strategy

Each phase adds tests at its own boundary and must pass the complete project
gate before the next phase begins.

### Semantic and ABI conformance

Executable C fixtures must verify these contracts:

- Repeated pending polls.
- Yield access followed by resume.
- Sticky ready, error, and canceled polls.
- Error detail lifetime through parent propagation.
- Cancellation during an active embedded, boxed, and opaque child.
- Exactly-once child finalization and defer execution.
- Idempotent internal task drop and single-use public destroy.
- Borrowed and owning dynamic awaitables.
- One-time result-layout validation.
- Null task and null poll-context behavior.
- Invalid poll-context and dynamic-vtable prefixes.
- Version-zero, pre-drop-truncated, and post-drop-truncated vtables.
- Missing mandatory callbacks and poll-context capabilities.
- Static error-message lifetime after child finalization.

### Static await planning

Compiler and generated-C tests must verify these plans:

- A nonrecursive same-unit await is embedded and directly polled.
- A self-recursive await produces a finite context with one boxed typed edge.
- A mutually recursive SCC breaks enough deterministic edges to remain finite.
- A cross-unit known async call is boxed but directly polled.
- A genuine dynamic await uses the vtable path.
- A failed boxed create doesn't poll or drop a null child.
- A task binding initializes once at declaration activation.
- Multiple awaits of one task binding reuse its sticky child instance.
- Scope exit finalizes an activated but never-awaited task binding exactly once.
- A direct await in a loop creates one generation per completed iteration.
- A backward jump that reexecutes a binding finalizes the previous generation.
- Interleaved task bindings and defers execute in reverse activation order.

The first performance milestone requires generated code for an eligible static
await to contain no child allocation, no dynamic awaitable construction, and no
indirect poll call.

### CFG optimizer

Every optimizer pass receives before-and-after structural tests and executable
differential fixtures. Tests must cover loops, switch fallthrough, user goto,
VLA diagnostics, nested defer, error propagation, cancellation, and yield.

### Project gates

Every implementation phase must pass these existing and new gates:

```text
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
pnpm run grammar:test
native generated-project compile and run
CMake and Meson generated-project compile and run
WebAssembly generated-project compile and link
ABI conformance C fixtures
```

MSVC execution remains a CI gate when the local toolchain isn't installed.
WebAssembly execution remains conditional until an agreed runtime is available.

## Staged migration plan

The migration keeps each behavioral change isolated behind a verified stage.
No stage starts until the previous stage passes all applicable gates.

### Stage 0: Specification and baseline

This stage turns approved semantics into reviewable and executable contracts.

- Finalize this design and RFC0001 core contract.
- Record ABI v2 deprecation and ABI v3 incompatibility.
- Add baseline C fixtures for existing sticky terminal and cleanup behavior.
- Classify current behavior as required, experimental, or accidental.
- Pin the WASI SDK and `wasm-tools` versions used by the WebAssembly gate.

### Stage 1: CIR and await planning

This stage adds planning data without changing generated runtime behavior.

- Add `FunctionId` and `AwaitEdgeId` identities.
- Add `ChildInstanceId`, origin, target, and unplanned storage representations.
- Build the project async symbol index and static await graph.
- Add Tarjan SCC analysis and deterministic cycle-edge selection.
- Resolve declaration-owned task bindings separately from edge-owned direct
  calls.
- Initially lower all plans through the existing output path to isolate IR
  correctness.

### Stage 2: Core ABI v3

This stage performs the one approved public ABI break.

- Replace poll enums with fixed-width status values.
- Add the nullable versioned poll context.
- Replace the per-instance callback layout with state and shared vtable.
- Move result layout to the vtable and validate once at slot activation.
- Regenerate public headers, templates, manifests, and C callers.
- Delete the ABI v2 path after ABI v3 conformance and project tests pass.

### Stage 3: Static await

This stage enables plans one storage strategy at a time.

- Enable embedded same-unit direct awaits.
- Enable boxed direct awaits for recursive cycle edges.
- Enable cross-unit typed direct awaits through generated public symbols.
- Retain opaque vtable dispatch only for dynamic targets.
- Add allocation and indirect-call assertions to generated-code tests.

### Stage 4: Coroutine CFG optimization

This stage adds focused optimization passes in the approved pass order.

- Add one pass and its differential tests at a time.
- Re-run liveness after control-flow changes.
- Enable await-slot reuse only after ownership interference tests pass.
- Connect configured optimization levels only to proven pass sets.

### Stage 5: Waker and reference executor

This stage starts only after a separate waker RFC is approved.

- Define versioned waker ownership and observable wake behavior.
- Implement manual, queued single-thread, and native cross-thread reference
  paths.
- Add lost-wakeup, duplicate-wake, cancellation-race, and lifetime tests.
- Keep cross-thread wake optional so the core remains WebAssembly-compatible.

### Stage 6: Backend validation and SPI

This stage validates extension boundaries before promising backend stability.

- Build at least two backends with different event models.
- Measure which operations belong in shared SPI and which remain adapters.
- Stabilize only the common, tested versioned prefix.
- Keep direct LLVM, native, and WebAssembly emitters as separate future design
  projects.

The approved
[Stage 6 design](2026-07-16-cr-backend-validation-stage-6-design.md) and
[implementation plan](../plans/2026-07-16-cr-backend-validation-stage-6-implementation.md)
define the concrete IOCP, epoll, kqueue, memory-provider, receive, cancellation,
and quiescence validation sequence.

## Progress tracking

The live plan reports one active stage and updates status as each gate passes.
The approved state at the time of this document is:

```text
Completed: architecture direction and compatibility policy
Completed: CIR and ABI v3 conceptual contract
Completed: formal design, review, and RFC0001
Completed: Stage 0 specification, ABI v2 baseline, and WebAssembly gate
Completed: Stage 1 identities, await planning, SCC analysis, and integration
Completed: Stage 2 core ABI v3 migration and conformance
Completed: Stage 3 typed static await
Completed: Stage 4 coroutine CFG and context optimization
Completed: Stage 5 Waker v1 and reference executors
Completed: Stage 6 design and user approval
Completed: Stage 6 implementation plan and user approval
Completed: Stage 6 Task 1 backend selection and target validation
Completed: Stage 6 Task 2 experimental backend and net records
Completed: Stage 6 Task 3 portable memory provider
Completed: Stage 6 Task 4 reference net-receive awaitable
Completed: Stage 6 Task 5 cancellation and allocation hardening
In progress: none
Next: begin Stage 6 Task 6 Windows IOCP provider
Pending: Stage 6 implementation Tasks 6 through 13
```

Progress reports must identify changed files, completed tests, failed gates,
and the next active item. A partial implementation doesn't count as a completed
stage.

## Next steps

Stage 6 Task 5 is complete. The next action is Task 6 in the approved
[Stage 6 implementation plan](../plans/2026-07-16-cr-backend-validation-stage-6-implementation.md).
Task 6 implements and validates the Windows IOCP completion provider with
real loopback TCP receive, overlapped cancellation, queue interruption, and
quiescence draining.
