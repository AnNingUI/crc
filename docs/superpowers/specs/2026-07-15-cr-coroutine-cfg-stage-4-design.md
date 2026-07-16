# CR coroutine CFG optimization Stage 4 design

This document defines Stage 4 of CR coroutine development. Stage 4 reduces
private task context size and removes redundant coroutine control flow without
changing source semantics, public ABI v3, or the Stage 3 dispatch matrix.

> **Note:** This is a preview feature currently under active development.

The design builds on the approved
[coroutine architecture](2026-07-14-cr-coroutine-architecture-v3-design.md),
the completed
[Stage 3 static await design](2026-07-14-cr-static-await-stage-3-design.md),
and [RFC0001](../../rfcs/0001-core-coroutine-contract.md).

## Decision

Stage 4 uses a dual-layer optimizer. A coroutine CFG canonicalization layer
removes redundant blocks and resume states. A separate physical context layout
layer maps stable logical fields to reusable private storage.

The optimizer preserves logical identities. It never merges or renumbers
`DeclarationId`, `AwaitSlotId`, or `ChildInstanceId` to express storage reuse.
Instead, it produces an explicit layout plan that the C emitter consumes.

Task context size is the primary Stage 4 metric. Ordinary C compilers can
usually simplify emitted goto control flow, but they can't infer that distinct
fields in an emitted task struct have mutually exclusive lifetimes. CR must
perform that ownership-aware storage analysis before C emission.

## Goals

Stage 4 has focused, measurable goals.

- Reduce private task context size through safe field reuse.
- Remove unreachable blocks, trivial jumps, and redundant resume states.
- Preserve exactly-once cleanup and copy-before-finalize behavior.
- Connect the existing `OptimizationLevel` configuration to proven pass sets.
- Keep optimization deterministic across file order, map order, and threads.
- Preserve native-first code quality and portable C11 output.
- Keep generated projects valid for `wasm32-wasi`.
- Produce reusable analysis results for future C, LLVM, and WebAssembly
  backends.

## Non-goals

Stage 4 doesn't broaden the language or runtime contract.

- Full SSA or Memory SSA.
- Constant folding for arbitrary C expressions.
- Loop unrolling, vectorization, or interprocedural inlining.
- Direct LLVM IR, native machine code, or WebAssembly emission.
- Waker, executor, reactor, or backend SPI work.
- Asynchronous drop or cancellation cleanup.
- Declaration-owned task-binding storage reuse.
- A public task layout or ABI v4.
- Target-specific public runtime branches.
- Changes to Stage 3 static or dynamic dispatch decisions.

## Required invariants

Every Stage 4 pass must preserve these invariants.

- Public ABI v3 and generated public headers remain unchanged.
- Cross-unit task layouts remain opaque.
- Static targets remain typed embedded or typed boxed.
- Dynamic targets remain ABI v3 opaque awaitables.
- Suspend and yield don't execute lexical cleanup.
- Parent drop finalizes each active direct child exactly once.
- Binding cleanup remains generation-aware and LIFO.
- Child result and error data are copied before child finalization.
- A field is reused only when all paths prove noninterference.
- An address-taken or escaping field is never reused.
- Optimization never adds runtime checks, branches, clearing, or copies.
- Unknown safety facts disable an optimization candidate.
- Internal IR corruption stops compilation instead of emitting C.

## Compiler pipeline

Stage 4 extends the existing pipeline without replacing the current HIR, CFG,
coroutine, liveness, or static-await plans.

```text
Tree-sitter syntax
  -> identity-based HIR
  -> scoped CFG
  -> scope-exit lowering
  -> coroutine CFG canonicalization
  -> coroutine lowering and resume-state assignment
  -> declaration liveness
  -> static await planning
  -> slot and ownership liveness
  -> physical context layout planning
  -> portable or computed-goto C emission
```

CFG changes occur before coroutine state assignment. Every accepted CFG change
invalidates prior coroutine state and liveness results, so the pipeline
recomputes them from the accepted graph.

Context layout planning occurs after static-await planning because the final
child representation can be embedded, typed boxed, or dynamic opaque. The
layout pass consumes that decision but can't change it.

### Integration with the current pipeline

The current compiler lowers scope exits and immediately assigns coroutine
states in `lower_indexed_source`. Stage 4 inserts CFG optimization between
those operations. It then passes the configured optimization level through
the later liveness, static planning, context layout, and emission steps.

The final integration is:

```text
build_cfg
  -> lower_scope_exits
  -> optimize_coroutine_cfg(config.build.optimization)
  -> lower_coroutines
  -> analyze_liveness
  -> build_c_static_await_plan
  -> analyze_slot_liveness
  -> build_context_layout_plan
  -> emit_translation_unit(layout_plan)
```

The compiler must not silently ignore `config.build.optimization`. During
incremental Stage 4 development, selecting an optimization level whose pass
set isn't implemented produces a deterministic compiler diagnostic. After
Stage 4 completes, all four levels are available.

## Module boundaries

Stage 4 introduces focused modules with one responsibility each.

- `coroutine_opt` validates and canonicalizes coroutine CFGs.
- `slot_liveness` computes logical result, temporary, and child ownership
  lifetimes at instruction-level program points.
- `context_layout` builds interference graphs and physical storage plans.
- `c_emitter` consumes access paths and doesn't infer reuse.

The existing `control_flow`, `scope_exit`, `coroutine`, `liveness`,
`await_plan`, and `c_static_plan` modules remain the semantic sources of truth.
Stage 4 can extend their public compiler-private records when an analysis needs
more precise facts, but it doesn't duplicate their decisions.

## CFG optimizer contract

The CFG optimizer applies a fixed, independently testable pass order. It uses
transactional validation so a malformed candidate never reaches emission.

### CFG verification

`VerifyCfg` checks the graph before and after every mutating pass.

It validates:

- Dense, unique `BlockId` values.
- A valid entry block.
- In-range successor edges.
- Consistent source and target scope stacks.
- Valid cleanup, resume, break, continue, and user-goto edge metadata.
- One terminator per block.
- Reachable suspend and yield continuations.
- Stable source spans for retained observable operations.

Verification doesn't change the graph and runs at every optimization level.

### Unreachable block removal

`RemoveUnreachableBlocks` starts at the function entry and follows all semantic
successors, including cleanup, suspend continuation, and yield continuation
edges. It removes blocks outside that closure.

Runtime reachability isn't the only retention requirement. A reachable
`TaskRef` can intentionally refer to a skipped task-binding declaration so the
poll path reports the established inactive-binding error. The declaration
block is therefore a compiler-metadata root even when no runtime edge reaches
it. Stage 4 retains that block and the structural successor closure required to
keep its CFG valid. This retention doesn't create a runtime entry edge.

After removal, the pass rebuilds dense `BlockId` values and rewrites every
edge through an explicit old-to-new map. Source order and original identity
provide deterministic tie breakers.

Block remapping happens before coroutine lowering. It invalidates every record
keyed by `BlockId`, including state maps, liveness maps, predecessor maps, and
program points. These records must be recomputed, not incrementally patched.
The pass preserves `AwaitEdgeId`, `CleanupId`, source spans, and source-backed
diagnostic identities for retained operations.

### Trivial jump threading

`ThreadTrivialJumps` removes an empty block whose only operation is a goto when
the source and target scopes are compatible.

The pass doesn't thread through:

- A suspend or yield boundary.
- Cleanup registration or execution.
- A block that preserves a user-goto diagnostic boundary.
- Different lexical scope stacks.
- A block with source-backed instructions.
- A transition whose edge-kind change would lose semantic information.

### Linear block merging

`MergeLinearBlocks` combines a block and its sole successor when the successor
has exactly one predecessor and both blocks share the same lexical scope
stack.

The merge is rejected when either block contains a suspend, yield, cleanup
boundary, user-goto boundary, or ownership transition that must remain a
separate program point. Instructions retain their original order.

### Resume-state compaction

Coroutine lowering runs again after CFG canonicalization. It assigns states
only to the retained entry, await repoll points, and yield continuations.

State values are dense and deterministic. Stage 4 doesn't move state writes
across child polling and doesn't rely on future waker reentrancy semantics.

### Excluded CFG transforms

Stage 4 keeps C expression semantics opaque.

- It doesn't fold an arbitrary branch or switch condition.
- It doesn't remove evaluation that can have side effects.
- It doesn't reorder calls, volatile access, cleanup, or child finalization.
- It doesn't merge code across a suspension point.
- It doesn't speculate about undefined C behavior.

## Logical context fields

The context layout pass models every private field with a stable logical
identity and an explicit category.

Eligible first-layer fields include:

- Await result slots.
- Expression-lowering await temporaries.
- Direct embedded static child bundles.
- Direct boxed static child bundles.
- Direct dynamic opaque awaitable bundles.

Eligible second-layer fields include non-parameter lifted locals that aren't
address-taken, don't escape, and aren't retained by cleanup ownership.

The following fields remain independent:

- Function parameters.
- Address-taken or escaping local variables.
- Declaration-owned task bindings and their generation counters.
- Fields still referenced by cleanup payloads.
- Task state, cleanup stack, public result, yielded value, and sticky error.
- Fields without a declarator-safe union representation.
- Any field whose lifetime isn't proven.

## Child ownership bundles

A direct child participates in layout planning as one ownership bundle rather
than as an unstructured payload field.

```text
DirectChildBundle
  logical child identity
  payload representation
  independent active flag
  activation point
  pending and yielded states
  terminal finalization points
  parent-drop finalization point
```

The payload representation is an embedded task, a typed task pointer, or a
`cr_awaitable`. Two bundles can share storage only when no path can make both
active at the same program point.

Stage 4 shares only the child payload. Each logical child retains its existing
independent active flag outside the union. Parent drop and error cleanup first
read that independent flag and access the corresponding union member only when
the flag is true. This avoids reading an inactive union member and doesn't add
a runtime tag, branch, or clearing operation beyond the existing lifecycle
checks.

Activation must initialize or assign the selected payload member before it
sets that child's flag. Finalization must access the payload while the flag is
true, complete drop or destroy, and then clear the flag before another member
can become active. The ownership data-flow proof guarantees that two flags for
one physical payload slot can't be true together.

Task bindings don't participate in Stage 4 reuse. Their cleanup records can
retain field and generation references after declaration reexecution. A later
stage can relax this only with a dedicated cleanup-reference proof.

## Program points and liveness

Stage 4 uses instruction-level program points. Block-level liveness alone is
too conservative for useful context reuse and too imprecise for ownership
transitions within one block.

Each block exposes stable points before and after every instruction, before
its terminator, at suspension, and at continuation entry. Cleanup and parent
drop paths also have explicit points.

Logical lifetimes follow these rules:

- An await result becomes live when the successful result is stored.
- An await result dies after its final use on every successor path.
- A direct child becomes live after successful activation.
- A pending or yielded child remains live across task suspension.
- A child dies only after its origin-aware finalization completes.
- Parent drop contributes a live path for every active direct child.
- A lifted local follows recomputed declaration liveness.
- A cleanup capture extends liveness according to the payload representation.
- Loops use a data-flow fixed point instead of source-range assumptions.

Copy-before-finalize creates real interference. A child's payload and its
result slot can't share storage while the result must be copied before drop or
destroy. Stage 4 doesn't rewrite that ordering.

### Cleanup reference lifetimes

Cleanup analysis distinguishes copied values from retained field references.

- A normal `__defer` argument copied into the cleanup stack uses the source
  field at registration. The copied payload owns its later cleanup value.
- An argument that takes or stores a field address marks that field as escaped
  and excludes it from reuse.
- A task-binding cleanup payload retains pointers to the binding payload,
  active flag, and generation counter until the helper completes.
- Binding reexecution can leave stale generation records in the cleanup stack,
  so every referenced binding field remains independent through parent drop.

No cleanup helper can resolve a field through a reusable union access path.
All fields referenced by a generated cleanup payload remain direct fields.

## Interference graph

The layout pass creates an undirected graph whose nodes are eligible logical
fields or child bundles. An edge means the two nodes can't share a physical
slot.

An interference edge exists when:

- Both nodes are live at the same program point.
- Both child bundles can be active on the same path.
- One node can be referenced through an outstanding cleanup payload.
- Alias or escape analysis excludes a safe lifetime end.
- A semantic rule requires independent identity or stable storage.

The analysis is conservative. Missing facts add interference or exclude a
node; they never remove an edge.

## Physical context layout plan

The layout plan separates logical compiler identities from emitted C storage.
Its conceptual records are:

```rust
struct ContextLayoutPlan {
    fields: BTreeMap<LogicalFieldId, FieldPlacement>,
    slots: Vec<PhysicalSlot>,
    decisions: Vec<LayoutDecision>,
}

struct FieldPlacement {
    slot: PhysicalSlotId,
    member: UnionMemberId,
    access_path: CAccessPath,
}

struct PhysicalSlot {
    id: PhysicalSlotId,
    members: Vec<PhysicalMember>,
}
```

These types are compiler-private. They describe semantic placement and access,
not a public memory layout contract.

Every unmerged field receives a direct field or a one-member physical slot.
Every merged group receives a named C union. Direct child payloads can be union
members, while their logical active flags remain independent context fields.

```c
union {
    child_task child_0;
    other_task *child_1;
    int await_result_2;
    long lifted_value_3;
} cr_slot_0;

bool cr_child_0_active;
bool cr_child_1_active;
```

The target C compiler determines union size and alignment. Stage 4 doesn't use
raw byte buffers, target-specific packing, or `_Alignas` layout emulation.

All member declarations must use declarator-aware C field records. Raw type
text isn't sufficient for arrays, function pointers, qualifiers, or nested
declarators. A field without a valid member declarator remains independent.

## C emitter contract

The C emitter receives a validated layout plan. It doesn't perform liveness,
interference, or graph-coloring decisions.

Every logical field reference resolves through a `CAccessPath`. This includes
context declarations, initialization, polling, cleanup, result propagation,
parent drop, and generated helper bodies.

The emitter must diagnose a missing placement as an internal compiler error.
It can't fall back to an old field name or silently allocate a second copy.

For every union member access, the emitter must prove that a dominating write
or typed child initialization selected that member and that no intervening
write selected another member. Parent drop and cleanup code can't probe union
members to discover which logical field is active.

Public task typedefs, public create and poll APIs, accessors, destroy
functions, and ABI v3 adapters remain unchanged. Same-unit task definitions
can change because they are compiler-private layouts.

## Target layout knowledge

Cross-type placement needs stronger evidence than a syntactic size estimate.
Stage 4 represents layout knowledge as `Exact` or `Unknown` for the configured
C target.

Exact knowledge can come from a reviewed target data model and declarator-aware
compiler-owned types. Unknown typedefs, tags, packing environments, custom
targets, and unsupported declarators remain unknown.

Cross-type placement is accepted only when the planner can compute the complete
affected context layout, including physical slot order, padding, alignment, and
all unchanged surrounding fields. A per-field estimate isn't sufficient.

Unknown fields can reuse storage with an identical normalized declarator, but
they don't participate in cross-type placement. The optimizer doesn't estimate
unknown alignment or move a high-alignment union to an earlier field position
based on declaration complexity.

Stage 4 adds an explicit `wasm32-wasi` target configuration and data model. The
required WebAssembly fixture must select that target instead of generating a
host plan and compiling it as WebAssembly later. A custom target without a
reviewed model uses the conservative unknown-layout rules.

## Optimization levels

The existing `OptimizationLevel` configuration selects only proven pass sets.

### None

`None` is the unoptimized semantic baseline.

- Run CFG and layout verification.
- Preserve all blocks and resume states required by the original lowering.
- Keep every logical context field independent.
- Provide the reference output for differential execution tests.

### Speed

`Speed` is the default native-first configuration.

- Run all approved zero-runtime-cost CFG canonicalization passes.
- Reuse compiler-owned await results, temporaries, and direct child bundles.
- Reuse lifted locals only when their normalized C declarators are compatible.
- Use deterministic linear scan and first-fit placement.
- Add no runtime tests, clearing, branches, copies, or allocations.

### Size

`Size` prioritizes minimum private task context and generated data size.

- Include every `Speed` pass.
- Allow different eligible C types with exact target layout knowledge to share
  named union storage.
- Use deterministic DSATUR-style interference coloring.
- Prefer high-cost compiler-owned objects, embedded children, and arrays.
- Use the complete accepted `Speed` layout as the initial incumbent.
- Accept a DSATUR candidate only when the exact target layout proves that the
  complete context is no larger than the `Speed` incumbent.
- Keep unknown cross-type layouts on the `Speed` placement.

Generated-C conformance tests compare actual `sizeof(task)` values against the
planner's exact cost. A mismatch is an optimizer correctness failure, and a
candidate larger than the `Speed` incumbent can't become the `Size` plan.

### Aggressive

`Aggressive` uses more compile time without relaxing semantic rules.

- Include every `Size` pass.
- Run bounded branch-and-bound placement for small interference graphs.
- Optimize context cost, then slot count, then access-path complexity.
- Use the accepted `Size` plan as the initial incumbent.
- Use a deterministic node budget instead of a wall-clock timeout.
- Fall back to the `Size` result when the node budget is exhausted.

For every supported target layout, `Aggressive` must produce a context no
larger than `Size`, and `Size` must produce a context no larger than `Speed`.
Cross-type fixtures verify this contract against emitted C. Unknown layouts
retain the lower-level plan instead of relying on a monotonicity guess.

## Determinism

Optimization output must be independent of incidental enumeration order.

Stable ordering uses:

```text
normalized translation-unit path
function identity
field category
source position
logical field identity
normalized C declarator
```

CFG remapping, state numbering, interference edges, physical slot numbering,
union member naming, and fallback decisions use these keys.

The aggressive search budget counts deterministic search nodes. Machine speed,
thread scheduling, and elapsed time can't affect its result.

## Optimization decisions and diagnostics

The optimizer records compiler-private decisions for structural tests and
future explain tooling.

A decision can report:

- Reused in a physical slot.
- Interferes with another logical field.
- Address taken or escaped.
- Retained by cleanup ownership.
- Parameter or public terminal field.
- Binding lifetime isn't supported.
- Declarator isn't union-safe.
- No profitable placement was found.
- Aggressive search used the deterministic fallback.

An unavailable optimization isn't a source error. The compiler retains the
original field.

An invalid CFG, missing placement, broken ownership transition, or failed
post-pass invariant is an internal optimization diagnostic. Compilation stops
instead of emitting potentially incorrect C. The compiler doesn't silently
hide an invariant failure by using an unverified candidate.

## Conformance strategy

Stage 4 requires IR, generated-code, executable, layout, and project-level
tests. Rust unit tests alone don't prove the source-to-source boundary.

### CFG conformance

CFG tests cover each pass independently and in the approved sequence.

- Reachable and unreachable ordinary blocks.
- Empty goto chains.
- Linear blocks with one and multiple predecessors.
- Cleanup and user-goto boundaries.
- Await repoll blocks and yield continuations.
- Scope-stack mismatches that prevent merging.
- Dense deterministic block remapping.
- Resume-state compaction after accepted CFG changes.
- Input-order-independent output.

Small hand-built graphs verify rejection paths. Parsed CR fixtures verify that
the optimizer preserves real lowering metadata.

### Liveness and ownership conformance

Analysis tests cover instruction-level lifetime boundaries.

- Sequential await results that can share storage.
- Simultaneously live expression results that must interfere.
- Mutually exclusive direct children.
- Loop-carried active children.
- Pending and yielded child retention.
- Ready, error, canceled, and invalid-status finalization.
- Parent drop while a child is active.
- Copy-before-finalize interference.
- Address-taken locals.
- Cleanup captures and task bindings.
- Conservative fixed points in loops and branches.

### Layout conformance

Layout tests inspect the validated plan and emitted C.

- Reused fields share one physical slot.
- Interfering fields occupy different slots.
- Ownership analysis treats each child payload and active flag as one logical
  bundle.
- Child active flags remain outside reusable payload unions.
- Binding and generation fields remain independent.
- Different eligible types use named C unions at `Size` and `Aggressive`.
- Array and function-pointer members use valid declarators.
- Access paths are stable and complete.
- Repeated planning produces byte-identical C.
- Every union read has a dominating member-selecting write or initialization.
- Parent drop never reads a union member whose logical active flag is false.

Generated C measures actual task size. Every optimized fixture must satisfy
`sizeof(optimized_task) <= sizeof(unoptimized_task)`, and representative
fixtures must prove a strict reduction.

The release conformance matrix compiles union fixtures with the supported
native C toolchains and the pinned WASI SDK. Required CI covers Clang or GCC,
MSVC, and `wasm32-wasi`; a local environment can report an unavailable optional
native compiler, but the release gate can't omit it.

### Differential execution

The same CR source compiles and runs under `None`, `Speed`, `Size`, and
`Aggressive`. Observable results and lifecycle logs must match.

The matrix covers:

- Sequential and nested await expressions.
- If, switch, loops, break, continue, and goto.
- Pending, yielded, ready, error, canceled, and invalid statuses.
- Embedded, boxed, cross-unit, and dynamic children.
- Parent drop and exactly-once finalization.
- Defer and task cleanup LIFO order.
- Loop reentry and mutually exclusive branches.
- Multiple simultaneously live results.
- Address-taken and cleanup-captured locals.

### Dispatch and ABI regression

Stage 3 and ABI v3 tests remain mandatory.

- Static sites contain no awaitable adapter or vtable fallback.
- Dynamic sites retain prefix, layout, capability, status, error, and drop
  validation.
- Public headers remain byte-stable.
- Cross-unit callers retain opaque typed task pointers.
- The core runtime header remains portable C11.

### Project and target gates

The final Stage 4 gate includes:

- Rust formatting, all-target checks and tests, and Clippy with warnings
  denied.
- Tree-sitter grammar tests.
- Portable switch and native computed-goto backends.
- Native generated-C execution with warnings denied.
- Separate-unit native compilation and linking.
- CMake and Meson generated projects.
- ABI v3 layout and malformed dynamic protocol regression.
- Required-mode `wasm32-wasi` compilation, linking, and module validation.
- Explicit `wasm32-wasi` layout planning rather than host-target planning.
- `Speed` as the default generated-project configuration.
- `None` as the differential baseline.
- Representative `Size` and `Aggressive` layout and determinism gates.

## Enablement sequence

Stage 4 enables one independently gated layer at a time.

1. Connect `OptimizationLevel` and establish `None` metrics.
2. Add CFG verification and stable block remapping.
3. Add unreachable removal, jump threading, block merging, and state
   compaction.
4. Add instruction-level slot and direct-child ownership liveness.
5. Add compiler-owned physical layout planning and enable `Speed`.
6. Add eligible lifted-local and cross-type union reuse and enable `Size`.
7. Add bounded placement search and enable `Aggressive`.
8. Run every native, cross-unit, ABI v3, and required WebAssembly gate.

Each layer must pass differential execution before the next layer becomes
active. An incomplete later layer can't change the behavior of an earlier
optimization level.

## Completion criteria

Stage 4 completes only when all approved behavior is implemented and gated.

- Default `Speed` strictly reduces task context size, block count, or state
  count in representative fixtures.
- `Size` produces contexts no larger than `Speed` for supported target layouts
  and conservatively retains `Speed` placement for unknown layouts.
- `Aggressive` produces contexts no larger than `Size` for supported target
  layouts and uses `Size` as its incumbent.
- All optimization levels match `None` observable behavior.
- Exactly-once drop, cleanup LIFO, and copy-before-finalize remain unchanged.
- Static and dynamic dispatch retain the Stage 3 matrix.
- Public ABI v3 and cross-unit opaque APIs remain unchanged.
- Native remains the primary performance path.
- Portable C11 and required `wasm32-wasi` gates pass.
- Output remains deterministic under input and enumeration changes.
- No Stage 5 waker or backend SPI behavior is introduced.

## Stop conditions

Stop and amend this design instead of broadening implementation when any of
these conditions occurs:

- Safe reuse requires changing public ABI v3.
- A static child must fall back to dynamic dispatch.
- Correctness requires asynchronous cleanup or a waker contract.
- A pass needs to evaluate arbitrary C expressions.
- A layout requires raw byte storage or target-specific packing.
- A cross-type layout has unknown size or alignment and can't conservatively
  retain the lower-level plan.
- A cleanup or binding lifetime can't be represented conservatively.
- WebAssembly requires a target-specific public runtime branch.
- An optimization level can't retain deterministic output.
- A size optimization increases a supported exact-layout context relative to
  its lower-level incumbent.
- Differential execution exposes an unexplained semantic change.

## Next steps

After independent specification review and user approval, create a file-by-file
Stage 4 implementation plan with red-green gates. Begin with configuration
wiring, baseline metrics, and CFG verification. Don't enable field reuse until
instruction-level ownership interference tests pass.
