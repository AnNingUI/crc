# CR static await Stage 3 design

This document defines Stage 3 of CR coroutine development. Stage 3 consumes
the existing identity and storage plans, emits typed static children, removes
avoidable allocation and indirect polling, and preserves the core ABI v3
dynamic boundary.

> **Note:** This is a preview feature currently under active development.

The design builds on the approved
[coroutine architecture](2026-07-14-cr-coroutine-architecture-v3-design.md),
the completed
[Stage 2 design](2026-07-14-cr-core-abi-v3-stage-2-design.md), and
[RFC0001](../../rfcs/0001-core-coroutine-contract.md).

## Decision

Stage 3 enables every planned static child origin and storage strategy in one
stage. It covers direct awaits, declaration-owned task bindings, same-unit
embedded storage, recursive boxed storage, and cross-translation-unit boxed
typed dispatch.

The C emitter becomes a two-phase translation-unit emitter. It plans task
layouts and typed declarations first, then emits each async body at its
original source position. It may move compiler-private layouts and prototypes,
but it must not move user function bodies or ordinary source text.

When the compiler can't prove that an embedded task layout is safe at the
required C declaration point, it deterministically changes only that target's
effective C storage to typed boxed storage. The target remains static, so the
generated parent still calls a concrete poll symbol and never falls back to a
vtable.

Cross-translation-unit static children remain boxed behind the existing opaque
public task API. Stage 3 doesn't expose task size or layout, doesn't add public
runtime declarations, and doesn't change ABI v3.

## Goals

Stage 3 must meet these goals:

- Remove allocation, awaitable construction, and indirect polling from every
  eligible same-unit embedded child.
- Remove awaitable construction and indirect polling from every static boxed
  child.
- Apply typed emission consistently to direct children and task bindings.
- Preserve sticky terminal states, transient yield, error propagation,
  cancellation, and exactly-once cleanup.
- Produce finite task layouts for self-recursive and mutually recursive async
  functions.
- Preserve normal C text, macro environments, preprocessing regions, linkage,
  and expression evaluation.
- Keep public task layouts opaque across translation units.
- Keep portable C11 valid for native and `wasm32-wasi` targets.
- Produce deterministic planning and generated output.

## Non-goals

Stage 3 excludes work that belongs to later contracts or optimizers:

- A public task-size or placement-construction ABI.
- Cross-translation-unit embedded task storage.
- Waker operations, executors, reactors, threads, or atomics.
- General C type checking or a complete C preprocessor implementation.
- A new dynamic task-binding syntax; current task bindings require a resolved
  async call and therefore have a static target.
- Await-slot reuse or coroutine CFG optimization from Stage 4.
- Full SSA, Memory SSA, direct LLVM IR, or direct WebAssembly emission.
- Asynchronous drop or asynchronous cancellation cleanup.
- Removing the ABI v3 dynamic adapters exposed by public async tasks.

## Required invariants

The implementation must preserve these invariants:

- `AwaitTarget` and requested `AwaitStorage` remain orthogonal CIR facts.
- A static target never becomes opaque because of a C layout restriction.
- An effective embedded graph is acyclic.
- One `ChildInstanceId` owns one physical child field.
- Multiple task-binding await edges reference that same child field.
- Direct and binding children keep their different finalization points.
- A child error is copied before the child is finalized.
- A null boxed child is never polled or destroyed.
- Every active child generation is finalized at most once.
- Public `.h` output and core ABI v3 declarations remain unchanged.
- The portable backend doesn't require a compiler extension.

## Compiler pipeline

Stage 3 adds target-specific C planning after liveness. The pipeline becomes:

```text
Tree-sitter CST
  -> identity HIR
  -> scoped CFG and scope-exit lowering
  -> coroutine and await planning
  -> project call graph, SCCs, and requested storage
  -> liveness
  -> C static-await and layout planning
  -> two-phase C emission
```

Project compilation performs liveness for all source units before final C
planning. Standalone `compile_source` runs the same process with its local
symbol index. The emitter receives a completed C plan and doesn't re-resolve
targets from rendered expression text.

The C planning pass can box additional requested embedded edges for declaration
visibility or target policy. It doesn't mutate the target identity and doesn't
change an already boxed or opaque plan.

## C codegen plan

The target-specific plan records requested CIR facts separately from effective
C emission decisions. Its conceptual child record is:

```rust
struct CChildPlan {
    instance: ChildInstanceId,
    target: CChildTarget,
    requested_storage: AwaitStorage,
    effective_storage: CChildStorage,
    downgrade_reason: Option<CLayoutReason>,
    origin: ChildOrigin,
    result: ResultLayout,
}

enum CChildTarget {
    Static(StaticCallee),
    Dynamic(ValueId),
}

enum CChildStorage {
    Embedded(ChildSlotId),
    Boxed(TypedSlotId),
    Opaque(AwaitSlotId),
}
```

`StaticCallee` contains the resolved `FunctionId`, task type, linkage, public
or internal symbol stem, result type, parameter types, definition unit, and
whether its full layout is available in the current translation unit.

Parameter types are declarator-aware records, not a comma-separated signature
string. They preserve C parameter adjustment for array and function
parameters, provide the storage type needed for argument temporaries, and let
the planner validate a typed prototype without reparsing emitted text.

Each `CFunctionPlan` contains:

- The caller's task layout and legal layout anchor.
- A child plan keyed by `ChildInstanceId`.
- An await-edge-to-child-instance map.
- Embedded layout dependencies.
- Required opaque forward declarations and typed prototypes.
- The final context field for each child instance.
- Any deterministic embedded-to-boxed downgrade reason.

Downgrade reasons are compiler-private planning evidence. They are available
to structural tests and verbose diagnostics but aren't runtime errors or
public ABI values.

## Effective storage matrix

The emitter supports exactly these target and effective-storage combinations:

| Target | Effective storage | Dispatch | Allocation |
| --- | --- | --- | --- |
| Static, same unit | Embedded | Direct typed calls | None |
| Static, recursive or policy-boxed | Boxed | Direct typed calls | One child task |
| Static, cross unit | Boxed | Public typed calls | One child task |
| Dynamic | Opaque | ABI v3 vtable | Provider-defined |

`Static + Opaque`, `Dynamic + Embedded`, and `Dynamic + Boxed` are invalid C
plans. C-plan validation rejects them before emission.

The parent context allocates one field per child instance:

```text
Embedded -> callee_task child;  bool active;
Boxed    -> callee_task *child; bool active;
Opaque   -> cr_awaitable child; bool active;
```

A declaration-owned child also stores a `uint64_t` generation. Direct children
don't need generation matching because one await edge owns their activation
and finalization.

The current language creates declaration-owned children only from resolved
async calls. Therefore `Embedded` and `Boxed` support both direct and binding
origins, while `Opaque` supports direct dynamic awaits. Stage 3 doesn't relax
the task-binding grammar to create an opaque binding.

Existing result temporaries remain per await edge. Stage 4 can reuse
noninterfering result and child slots after it adds ownership-aware
interference analysis.

## Layout feasibility

Every async function has an original layout anchor at the start of its source
replacement. The planner may assign a callee layout to an earlier island only
when that layout is valid in the earlier C declaration environment.

The planner builds a conservative file-scope index for declarations that can
affect an emitted task field:

- `typedef` names and tag declarations.
- Relevant object and function declarations.
- `#include`, `#define`, and `#undef` positions.
- Preprocessor conditional ancestry.
- The original declaration point of every async definition.

A layout can move to an earlier anchor only when all locally declared types and
macros used by its fields are already visible there. It can't move across an
incompatible preprocessing region. When a type identifier isn't locally
resolved, the planner can treat it as inherited from an earlier include only
when no intervening include or macro directive could change that environment.

If the proof fails, every edge that requires the unavailable complete layout
becomes typed boxed. The compiler doesn't guess, generate an incompatible C
struct, or fall back to the dynamic ABI.

A type declared only inside a function can't become a coroutine context field.
The compiler reports a source diagnostic instead of emitting an invalid
file-scope task layout. This is a general correctness requirement exposed by
Stage 3, not a public ABI restriction.

After feasibility downgrades, the planner validates the remaining embedded
graph again. It assigns layout islands in stable translation-unit, source, and
identity order, then topologically orders complete task layouts with children
before parents.

## Typed prototype feasibility

Boxing removes the need for a complete task layout at the caller, but it
doesn't remove the need for a valid typed C declaration. Before emitting any
static call, the planner separately proves that the callee task forward type,
result type, and adjusted parameter types are visible in the caller's
declaration environment.

The planner can reuse a compatible generated-header declaration or synthesize
a typed prototype at a legal island. A synthesized prototype follows the same
file-scope type, macro, include, and preprocessing-region rules as a task
layout, except that the task itself can remain incomplete.

If no legal prototype point exists before the static call, boxed storage can't
repair the source. The compiler emits a deterministic source diagnostic that
requires a visible compatible async declaration. It doesn't generate an
implicit C declaration, move the user body, erase the typed call, or fall back
to a vtable. Normal C code can resolve the diagnostic by including the
generated header or placing a compatible async declaration before use.

## Two-phase translation-unit emission

The first phase prepares compiler-owned declarations. The second phase emits
function bodies without changing their source environment.

At each layout island, the emitter can produce:

```text
opaque task forward declarations
typed internal or public prototypes
topologically ordered complete child task layouts
the task layout anchored at this source position
```

An incomplete task forward declaration is enough for boxed pointers and typed
function prototypes only when every non-task signature type is visible.
Embedded fields additionally require the complete callee layout.

Complete task definitions use a forward typedef plus a later `struct`
definition so the same task identity is shared by boxed and embedded paths.
Each complete definition is emitted exactly once.

Async function bodies remain in their original replacement ranges. Ordinary C
bytes between replacements remain unchanged. Generated declarations don't
cross incompatible `#if`, `#elif`, or `#else` regions.

External async functions retain their existing public symbol stem and public
task API. Internal-linkage async functions use a deterministic
translation-unit-qualified private stem and `static` typed declarations. An
internal symbol is never referenced from another translation unit.

The public header emitter remains byte-stable. It continues to expose only the
opaque task typedef and the existing create, poll, destroy, result, yielded,
error, and dynamic-adapter functions.

## Static activation

The compiler materializes every argument once in established CR evaluation
order before it activates a child. Compiler-private temporaries enforce this
order where a generated C call would otherwise leave argument ordering
unspecified.

An embedded child activates with direct internal calls:

```c
callee_init(&parent->child, evaluated_arguments);
parent->child_active = true;
```

A boxed child activates through typed allocation:

```c
parent->child = callee_create(
    evaluated_arguments,
    &parent->error
);
if (parent->child == NULL) {
    /* sticky parent error cleanup */
}
parent->child_active = true;
```

Before a boxed create, the parent initializes its error slot with the existing
allocation fallback. If create returns null without useful error details, the
fallback remains observable. The parent doesn't poll or destroy the null
pointer.

Same-unit boxed and embedded children use compiler-known internal entry points.
Cross-unit boxed children call the existing public typed task API. Cross-unit
storage never depends on `sizeof(callee_task)`.

## Static polling

Both static storage strategies call the concrete poll function and forward the
current poll context:

```c
status = callee_poll(child_pointer, poll_context);
```

The parent doesn't perform vtable capability checks for a static child. The
generated child poll validates its own nonterminal poll context. Repeated polls
of a terminal child retain the Stage 2 sticky-terminal behavior.

The parent handles each status as follows:

- `CR_POLL_PENDING` retains the active child and returns pending.
- `CR_POLL_YIELDED` copies the yielded value when the established propagation
  types are compatible, retains the child, and returns yielded.
- `CR_POLL_READY` copies the result before origin-aware finalization.
- `CR_POLL_ERROR` copies error details before origin-aware finalization.
- `CR_POLL_CANCELED` performs origin-aware cancellation cleanup.
- Any other value reports `CR_ERROR_INVALID_POLL_STATUS`.

For a same-unit child, the emitter can copy compiler-private result, yielded,
and error fields directly because the layout is visible. For a cross-unit
boxed child, it uses the public typed accessors. A missing cross-unit error
detail uses `CR_ERROR_MISSING_CHILD_ERROR` before destroy.

Void children don't expose result or yielded accessors. Stage 3 preserves the
existing yielded-type compatibility behavior and doesn't introduce a new
generator contract.

## Origin-aware lifetime

A direct child is owned by its await edge. The edge creates one generation on
entry, retains it across pending and yielded polls, then finalizes it at ready,
error, cancellation, invalid status, or parent drop. Reentering the edge starts
a new generation.

A task binding is owned by its declaration scope. Declaration execution:

1. Finalizes an active previous generation.
2. Increments the generation counter.
3. Initializes or creates the new typed child.
4. Marks the child active after successful activation.
5. Pushes a lexical cleanup with the captured generation.

If cleanup registration fails after activation, the generated failure path
immediately invokes the typed finalizer for the captured generation, clears
the active flag and boxed pointer, runs all previously registered cleanups,
records the existing cleanup-allocation error, and becomes sticky error. The
failed cleanup record was never installed, so no later path can finalize that
generation again.

Every await of that binding polls the same field. Ready, error, and canceled
child states remain stored until lexical cleanup so repeated awaits observe the
same sticky terminal state. A control-flow path that skipped activation still
reports `CR_ERROR_INACTIVE_TASK_BINDING`.

Typed binding cleanup payloads contain the child field, active flag, generation
pointer, and captured generation. The helper calls embedded `drop` or boxed
`destroy` according to the effective storage. Stale records can't finalize a
newer generation.

Parent drop first finalizes any active direct child. It then destroys the
cleanup stack, which runs task bindings and `__defer` callbacks in reverse
dynamic-activation order. Boxed destroy invalidates the typed pointer and the
compiler clears it after finalization.

## Dynamic boundary

A genuinely dynamic target keeps the complete Stage 2 path:

- A two-word `cr_awaitable` slot.
- Prefix-safe vtable validation.
- Result layout validation.
- Required capability checks before every poll.
- Vtable polling and origin-aware vtable drop.

Stage 3 must not weaken malformed-object conformance. Public generated
`*_as_awaitable` and `*_into_awaitable` adapters remain available even when all
calls inside one project resolve statically.

## Error and cancellation contract

Stage 3 adds no public runtime error codes. It reuses these established paths:

- Allocation fallback for a boxed create failure.
- `CR_ERROR_MISSING_CHILD_ERROR` for missing error details.
- `CR_ERROR_INVALID_POLL_STATUS` for an unknown static child status.
- `CR_ERROR_INACTIVE_TASK_BINDING` for skipped binding activation.
- The child's copied error for a valid static child error.

Failure cleanup remains origin-aware. A direct child finalizes immediately
before parent lexical cleanup. A binding remains active until its registered
cleanup position, preserving LIFO order with surrounding defers.

Task drop remains synchronous and can't poll or suspend.

## Determinism

The same normalized project input must produce the same effective plan and C
output regardless of file enumeration, hash-map order, or worker scheduling.

Stable ordering uses:

```text
normalized translation-unit path
caller source start
child-instance source start
callee linkage key
child-instance identity
```

Layout island assignment, feasibility downgrade, prototype order, task layout
order, context field order, and generated private names all use stable keys.

## Implementation boundaries

The new planning logic belongs outside the text emitter. A focused
`c_static_plan` module consumes liveness, symbol-index, source-position, and
preprocessor-region facts and produces validated C plans.

The existing `c_emitter` remains the translation-unit orchestrator. New static
layout and static-await emission can live in focused submodules, while the
Stage 2 dynamic-await implementation remains isolated and reusable. Stage 3
must not duplicate lifecycle rules separately for every storage strategy;
shared origin-aware emission helpers select typed or opaque operations from the
plan.

This refactoring is limited to boundaries required by typed child emission. It
doesn't introduce a general C AST or rewrite unrelated synchronous emission.

## Conformance strategy

Stage 3 requires structural planning tests and executable generated-C tests.
Internal Rust tests alone don't prove the product boundary.

### Planning conformance

Planning tests must cover:

- A same-unit nonrecursive child that remains embedded.
- A child defined later in source that moves to a safe earlier layout island.
- A local type, macro, include, or preprocessing boundary that deterministically
  changes only the affected edge to boxed.
- A self-recursive function with a finite typed pointer field.
- A mutual SCC with deterministic cycle-closing boxed edges.
- A cross-unit known child that remains typed boxed.
- A boxed target whose task type is incomplete but whose typed prototype is
  valid.
- A static target whose signature types aren't visible before use and produce
  a deterministic source diagnostic instead of dynamic fallback.
- Internal-linkage identities in separate units.
- Input-order-independent plans and private names.
- Rejection of every invalid target and storage combination.

### Lifecycle conformance

The executable matrix covers `Embedded` and `Boxed` storage with both `Direct`
and `Binding` origins. It also covers `Opaque` storage with the supported
`Direct` dynamic origin. It verifies:

- Pending, yielded, ready, error, canceled, and invalid statuses.
- Copy-before-finalize result and error behavior.
- Repeated awaits of one sticky binding.
- Never-awaited bindings and skipped activation.
- Binding declaration reexecution and stale cleanup records.
- Parent drop while each child strategy is active.
- Interleaved binding cleanup and defer LIFO order.
- Exactly-once embedded drop, boxed destroy, and opaque drop.
- Boxed create failure without polling or destroying null.
- Cleanup-registration failure after embedded and boxed binding activation,
  including immediate exactly-once finalization.

Recursive fixtures must compile to finite contexts and execute a terminating
base case. Cross-unit fixtures must compile separate generated C files and link
through generated public headers.

### Performance-shape conformance

Generated-code assertions inspect the parent await site rather than the whole
translation unit, because public dynamic adapters remain present.

An embedded site must contain no child create, allocation, `cr_awaitable`,
vtable access, or indirect poll. A boxed static site can call typed create and
destroy but must contain no dynamic awaitable construction, vtable access, or
indirect poll. An opaque site must retain all ABI v3 validation.

These structural assertions are the Stage 3 performance contract. Native C
compiler optimization and later benchmarks can measure secondary effects, but
they don't replace the shape gate.

### Project and target gates

The final gate includes:

- Native generated-C execution with warnings treated as errors.
- Separate-unit native compilation and linking.
- CMake and Meson generated-project execution.
- Portable and computed-goto backend coverage.
- Required-mode `wasm32-wasi` compilation, linking, and module validation.
- ABI v3 layout and malformed dynamic protocol regression tests.
- Rust formatting, all-target checks and tests, Clippy with warnings denied,
  and Tree-sitter grammar tests.
- Byte-stable public header output.

## Enablement sequence

Implementation enables one independently gated layer at a time:

1. Add C plan data, layout feasibility, and validation without changing C.
2. Add two-phase task forward declarations, prototypes, and layout islands.
3. Enable same-unit embedded direct awaits.
4. Enable embedded declaration-owned task bindings.
5. Enable same-unit recursive and policy-boxed typed children.
6. Enable cross-unit boxed direct awaits and bindings.
7. Regenerate the representative golden and run every native and WebAssembly
   gate.

Intermediate layers can keep later strategies on the Stage 2 dynamic path only
while their focused implementation task is incomplete. The completed Stage 3
output must obey the final effective-storage matrix with no static opaque path.

## Completion criteria

Stage 3 completes only when all of these conditions hold:

- Every static `ChildInstanceId` emits through embedded or boxed typed calls.
- Every dynamic target still emits through ABI v3 vtable dispatch.
- Direct and binding origins pass the complete lifecycle matrix.
- Same-unit eligible embedded sites allocate nothing and poll directly.
- Recursive and cross-unit contexts remain finite and poll directly.
- Unsafe layout hoists downgrade deterministically without changing semantics.
- Async bodies remain in their original macro and preprocessing environment.
- Public headers and core ABI v3 remain unchanged.
- Native, generated-project, and required WebAssembly gates pass.
- No Stage 4 CFG optimization is enabled.

## Stop conditions

Stop and amend this design instead of broadening implementation when any of
these conditions occurs:

- Full embedded emission requires moving a user async body.
- A layout move can't preserve its preprocessing condition or C type identity.
- Cross-unit typed dispatch requires exposing task layout or changing ABI v3.
- A static path can't preserve RFC0001 ownership and cleanup ordering.
- A recursive plan produces an embedded layout cycle.
- WebAssembly support requires a target-specific public runtime branch.
- Correct static dispatch requires enabling an unreviewed Stage 4 optimizer.

## Next steps

After independent review and user approval, create a file-by-file Stage 3
implementation plan with red-green gates. Begin with C-plan data and layout
feasibility; don't enable typed emission until that plan passes its structural
tests.
