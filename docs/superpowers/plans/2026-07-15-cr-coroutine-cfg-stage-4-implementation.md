# CR coroutine CFG optimization Stage 4 implementation plan

This plan implements ownership-aware coroutine CFG and context layout
optimization without changing public ABI v3 or the completed Stage 3 dispatch
matrix. It establishes an unoptimized baseline, adds independently verified CFG
passes, models logical field lifetimes, and enables deterministic storage reuse
at the existing optimization levels.

The requirements come from the approved
[Stage 4 design](../specs/2026-07-15-cr-coroutine-cfg-stage-4-design.md), the
[coroutine architecture](../specs/2026-07-14-cr-coroutine-architecture-v3-design.md),
the completed
[Stage 3 design](../specs/2026-07-14-cr-static-await-stage-3-design.md), and
[RFC0001](../../rfcs/0001-core-coroutine-contract.md).

> **Note:** This workspace isn't a Git repository. Each task ends with named
> gates and a live status update instead of a commit checkpoint.

## Outcome

At completion, CR removes redundant coroutine blocks and states before
liveness, then maps noninterfering private logical fields to deterministic
physical context slots. The default `Speed` level reduces hot task contexts
without adding runtime work. `Size` adds proven cross-type union reuse, and
`Aggressive` uses bounded search without producing a larger context.

Stage 4 must satisfy these invariants:

- `None` remains the unoptimized differential baseline.
- CFG changes happen before state assignment and liveness.
- Every CFG mutation passes invariant verification.
- Logical declaration, await, child, cleanup, and source identities remain
  stable.
- Child payload reuse never reads an inactive union member.
- Each child active flag remains an independent field.
- Task bindings, parameters, address-taken values, and cleanup-retained fields
  remain independent.
- `Size` retains the complete `Speed` plan unless exact layout proves a
  non-larger context.
- `Aggressive` retains the complete `Size` plan unless bounded search proves a
  non-larger context.
- Public ABI v3 headers and runtime declarations remain byte-stable.
- Static targets remain typed, and dynamic targets remain on ABI v3 vtables.
- Native remains the primary performance gate.
- Portable C11 and explicit `wasm32-wasi` planning remain supported.
- Stage 5 waker and backend behavior remain out of scope.

## Live status

Stages 0 through 4 are complete. The Stage 4 specification passed user and
independent review, and Tasks 1 through 13 established the verified CFG
optimization pipeline, differential execution evidence, structured slot
liveness, direct-child ownership interference, and validated identity context
layouts with deterministic same-type storage reuse and exact target-layout
knowledge. `Size` now uses that knowledge for proven cross-type and lifted-local
reuse, and `Aggressive` now uses deterministic bounded placement search with the
complete `Size` layout as its fallback. Final native, ABI v3, grammar, and
WebAssembly acceptance gates all pass.

```text
Completed: Stages 0 through 3
Completed: Stage 4 design, independent review, and user approval
Completed: Task 1, optimization configuration and baseline preservation
Completed: Task 2, CFG verification and transactional pass infrastructure
Completed: Task 3, stable block remapping and unreachable removal
Completed: Task 4, trivial jump threading and safe linear block merging
Completed: Task 5, state re-lowering, metrics, and differential execution
Completed: Task 6, structured await results and slot liveness
Completed: Task 7, direct-child ownership interference
Completed: Task 8, identity physical layouts and access-path emission
Completed: Task 9, Speed same-type storage reuse
Completed: Task 10, declarator-safe target layout knowledge
Completed: Task 11, Size cross-type and lifted-local reuse
Completed: Task 12, deterministic bounded Aggressive placement
Completed: Task 13, final native, ABI, grammar, and WebAssembly acceptance
Completed: Stage 4
In progress: none
Pending: none
Blocked: none
```

## Task 1: Wire optimization configuration and preserve the baseline

This task makes optimization selection explicit throughout the pipeline without
changing generated C.

**Files:**

- Modify `src/config/mod.rs`.
- Modify `src/lib.rs`.
- Modify `src/template/templates/crc.toml` only when the serialized default
  must become explicit.
- Modify configuration and compiler pipeline unit tests.
- Modify `tests/wasm_generated_project.rs`.

**Steps:**

1. Add failing configuration tests for `none`, `speed`, `size`, `aggressive`,
   and `wasm32-wasi` round trips.
2. Add an explicit `Wasm32Wasi` target variant with the stable serialized name
   `wasm32-wasi`.
3. Derive or implement the traits needed to pass `OptimizationLevel` by value
   through compiler-private planning APIs.
4. Thread `config.build.optimization` into indexed lowering and C emission.
5. Add compiler-private optimization options without enabling a mutating pass.
6. Make every level call the same no-op baseline pipeline in this task.
7. Change the required WebAssembly fixture to select `wasm32-wasi` before CR
   compilation.
8. Prove the representative Stage 3 generated C remains byte-identical at
   `None` and unchanged at the other levels in this task.
9. Prove no code path reads an optimization level and then discards it.

**Focused gates:**

```powershell
cargo test --lib config
cargo test --lib tests
cargo test --test coroutine_contract representative_abi_v3_output_is_byte_stable
$env:CRC_REQUIRE_WASM='1'
cargo test --test wasm_generated_project
Remove-Item Env:CRC_REQUIRE_WASM
```

**Acceptance evidence:**

- Every optimization level reaches the compiler pipeline.
- The required WebAssembly path plans for `wasm32-wasi`.
- No generated C changes before an optimization pass is approved.
- Public ABI v3 remains unchanged.

### Task 1 completion evidence

Task 1 completed on July 15, 2026, with this evidence:

- All four optimization levels serialize, parse, and reach the compiler
  pipeline.
- `wasm32-wasi` has a stable public configuration name.
- The required WebAssembly fixture selects `wasm32-wasi` before CR
  compilation, then compiles, links, and validates the generated module.
- The ABI v3 golden is explicitly pinned to `None`.
- `Speed`, `Size`, and `Aggressive` produce the Stage 3 baseline before their
  passes are enabled.
- `cargo test --all-targets` passed, including 85 library tests and every
  native, project, ABI, static-await, cross-unit, and WebAssembly test.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` passed.

## Task 2: Add CFG verification and transactional pass infrastructure

This task establishes the safety boundary required before any graph mutation.

**Files:**

- Create `src/coroutine_opt.rs`.
- Export the module from `src/lib.rs`.
- Modify `src/control_flow.rs` only for shared successor and edge utilities.
- Add focused unit tests in `src/coroutine_opt.rs`.

**Steps:**

1. Add failing hand-built CFG tests for duplicate, sparse, and out-of-range
   `BlockId` values.
2. Add failing tests for an invalid entry, invalid successor, malformed scope
   metadata, and broken suspend or yield continuation.
3. Define `CfgVerification`, `CfgOptimizationResult`, `CfgPassReport`, and
   stable internal diagnostic records.
4. Centralize semantic successor enumeration so verification, liveness, and
   optimization use one edge definition.
5. Validate dense block identity, entry identity, terminators, edges, scope
   stacks, and retained source metadata.
6. Add a transactional pass wrapper that verifies input and candidate graphs.
7. Stop compilation on an invalid candidate instead of silently returning an
   unverified graph.
8. Run verification only at every optimization level; keep the graph
   byte-for-byte unchanged.
9. Prove valid cleanup, user-goto, suspend, and yield graphs pass.

**Focused gate:**

```powershell
cargo test --lib coroutine_opt
```

**Acceptance evidence:**

- Invalid compiler IR can't reach coroutine lowering or C emission.
- Verification doesn't mutate valid CFGs.
- All later passes have one transactional entry point.

### Task 2 completion evidence

Task 2 completed on July 15, 2026, with this evidence:

- `coroutine_opt` verifies dense block identities, entry identity, closed
  terminators, successor targets, source and target scopes, resume edges,
  cleanup identities, and await-edge identities.
- Invalid input or candidate CFGs produce `CRC7001` through `CRC7009`
  diagnostics and no verified candidate.
- The compiler rejects verifier errors before coroutine lowering.
- Control-flow optimization and liveness share one deterministic successor
  model from `control_flow`.
- All four optimization levels run the same verified identity pass, so Stage 3
  output remains byte-stable.
- Seven focused verifier and transaction tests passed.
- `cargo test --all-targets` passed, including 92 library tests and every
  native, project, ABI, static-await, cross-unit, and WebAssembly test.
- Required-mode `wasm32-wasi` compilation, linking, and validation passed.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` passed.

## Task 3: Implement stable block remapping and unreachable removal

This task adds the first mutating CFG pass and the remapping primitive reused by
later passes.

**Files:**

- Extend `src/coroutine_opt.rs`.
- Modify `src/lib.rs` to insert optimization after scope-exit lowering.
- Extend CFG and integration tests.

**Steps:**

1. Add failing tests for unreachable ordinary, cleanup, label, suspend, and
   yield blocks.
2. Add a deterministic reachability walk from the function entry over every
   semantic successor.
3. Preserve reachable cleanup and continuation edges even when they aren't
   ordinary fallthrough paths.
4. Retain a skipped task-binding declaration as a compiler-metadata root when
   a runtime-reachable `TaskRef` depends on it.
5. Build an explicit old-to-new `BlockId` map in stable original block order.
6. Rewrite the entry and every edge through that map.
7. Preserve `AwaitEdgeId`, `CleanupId`, edge kinds, source spans, and lexical
   scope metadata.
8. Re-run CFG verification after remapping.
9. Enable unreachable removal for `Speed`, `Size`, and `Aggressive`; keep
   `None` unchanged.
10. Prove input block enumeration and hash-map order don't affect output.

**Focused gates:**

```powershell
cargo test --lib coroutine_opt unreachable
cargo test --lib control_flow
cargo test --lib scope_exit
```

**Acceptance evidence:**

- No runtime-reachable block disappears, and an unreachable block remains only
  when required compiler metadata depends on it.
- Retained logical and source identities remain stable.
- Every rewritten graph has dense valid block identities.

### Task 3 completion evidence

Task 3 completed on July 15, 2026, with this evidence:

- `Speed`, `Size`, and `Aggressive` remove runtime-unreachable blocks, while
  `None` retains the verified input graph.
- Stable old-to-new remapping rewrites the function entry and every semantic
  successor in original block order.
- Retained `AwaitEdgeId`, `CleanupId`, edge kinds, scope metadata, and source
  spans remain unchanged.
- Disconnected ordinary, label, cleanup, suspend, and yield blocks are removed
  while their reachable counterparts remain.
- Reachable `TaskRef` dependencies retain skipped task-binding declarations as
  compiler metadata, preserving the inactive-binding error contract.
- The pass is idempotent and independent of translation-unit function order.
- Eleven focused verifier, metadata-root, reachability, and remapping tests
  passed.
- `cargo test --all-targets` passed, including 96 library tests and every
  native, project, ABI, static-await, cross-unit, and WebAssembly test.
- Required-mode `wasm32-wasi` compilation, linking, and validation passed.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` passed.

## Task 4: Thread trivial jumps and merge safe linear blocks

This task completes the approved CFG canonicalization set without interpreting
C expressions.

**Files:**

- Extend `src/coroutine_opt.rs`.
- Add parsed CR fixtures to CFG optimizer tests.

**Steps:**

1. Add failing tests for empty goto chains and single-predecessor linear
   blocks.
2. Add rejection tests for scope changes, cleanup instructions, suspend,
   yield, source-backed operations, and user-goto boundaries.
3. Compute predecessor counts from the shared successor model.
4. Resolve trivial goto chains transitively without creating an edge cycle.
5. Preserve the original semantic edge kind when threading is legal.
6. Merge a linear successor only when it has one predecessor and a compatible
   lexical and ownership boundary.
7. Preserve instruction and source order during every merge.
8. Run stable block remapping and verification after each accepted pass.
9. Prove block counts never increase and repeated optimization is idempotent.

**Focused gate:**

```powershell
cargo test --lib coroutine_opt canonical
```

**Acceptance evidence:**

- Empty routing blocks and safe linear boundaries disappear.
- Cleanup, goto, suspension, and expression semantics remain opaque.
- Running the optimizer twice produces the same CFG.

### Task 4 completion evidence

Task 4 completed on July 15, 2026, with this evidence:

- Optimized levels run verified unreachable removal, trivial jump threading,
  and linear block merging in the approved order.
- Jump threading accepts only empty, non-entry blocks with same-scope
  fallthrough exits.
- User-goto, resume, cleanup, and lexical-scope boundaries prevent threading.
- Linear merging requires a forward same-scope fallthrough and one predecessor.
- Task activation, cleanup, user-goto, suspend, and yield boundaries prevent
  merging.
- Retained source-backed instructions preserve their original order.
- Every mutating pass removes newly unreachable blocks, remaps identities, and
  passes transactional verification before the next pass.
- The complete optimizer is idempotent, and block counts never increase.
- Fifteen focused verifier, reachability, threading, merging, and boundary
  tests passed.
- The `None` ABI v3 golden remained byte-stable, and Stage 3 dispatch tests
  retained their static and dynamic paths.
- `cargo test --all-targets` passed, including 100 library tests and every
  native, project, ABI, static-await, cross-unit, and WebAssembly test.
- Required-mode `wasm32-wasi` compilation, linking, and validation passed.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` passed.

## Task 5: Re-lower states and establish CFG differential execution

This task proves CFG optimization runs at the correct pipeline point and safely
compacts resume states.

**Files:**

- Modify `src/lib.rs`.
- Modify `src/coroutine.rs` only for optimization reports or stronger state
  assertions.
- Create `tests/coroutine_optimization.rs`.
- Modify `tests/coroutine_contract.rs` to pin the ABI golden to `None`.

**Steps:**

1. Add a failing integration fixture whose CFG contains unreachable and
   trivial blocks around await and yield points.
2. Run CFG optimization after scope-exit lowering and before
   `lower_coroutines`.
3. Ensure coroutine state assignment consumes only the optimized graph.
4. Recompute every state map and liveness result instead of patching old
   `BlockId` keys.
5. Add Rust-side reports for input blocks, output blocks, and resume states
   without changing the generated C or public C ABI.
6. Compile and execute the same fixture at `None`, `Speed`, `Size`, and
   `Aggressive`.
7. Assert identical lifecycle logs and results across levels.
8. Assert optimized block and state counts don't exceed `None`.
9. Keep the representative ABI v3 golden byte-stable by compiling it at
   `None`.

**Focused gates:**

```powershell
cargo test --lib coroutine
cargo test --test coroutine_optimization cfg
cargo test --test coroutine_contract
```

**Acceptance evidence:**

- State compaction follows accepted CFG changes automatically.
- No stale `BlockId`-keyed analysis survives optimization.
- `None` remains a stable executable and golden baseline.

### Task 5 completion evidence

Task 5 completed on July 15, 2026, with this evidence:

- `Compiler::compile_source_with_report` returns generated C and verified CFG
  metrics without changing `Compiler::compile_source` behavior.
- The report records the selected optimization level, every verified pass,
  input and output block totals, and final async resume-state totals.
- Metrics come from the same optimized `CfgUnit` that enters coroutine state
  assignment, liveness analysis, storage planning, and C emission.
- Report chains are contiguous, deterministic, and consistent with aggregate
  input and output totals.
- The representative optimization fixture has a strict block-count reduction
  at `Speed`, `Size`, and `Aggressive`; block and state counts never exceed
  `None`.
- Repeated compilation produces byte-identical C and identical reports.
- All four optimization levels preserve the same parent yield, child yield,
  resume, ready, lifecycle event, and final-result sequence under native C11
  execution with `-Wall -Wextra -Werror`.
- The `None` ABI v3 golden remains byte-stable, and all Stage 3 static,
  dynamic, recursive, and cross-unit await tests pass.
- `cargo test --all-targets` passes, including 100 library tests and every
  native, project, ABI, static-await, cross-unit, optimization, and WebAssembly
  test.
- Required-mode `wasm32-wasi` compilation, linking, and validation pass with
  `CRC_REQUIRE_WASM` restored to its prior state after the gate.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 6: Add structured await-result references and program points

This task removes the need to infer await-result identity from rendered source
text and establishes instruction-level analysis points.

**Files:**

- Modify `src/semantic.rs` for a structured await-result expression identity.
- Modify `src/control_flow.rs` to produce the structured identity.
- Modify `src/liveness.rs` and `src/c_emitter.rs` to consume it.
- Create `src/slot_liveness.rs`.
- Export the module from `src/lib.rs`.
- Add focused semantic, CFG, liveness, and slot-liveness tests.

**Steps:**

1. Add failing tests that reject slot analysis based on
   `__cr_await_result_<n>` string scanning.
2. Add a compiler-private `AwaitResultRef(AwaitSlotId)` HIR expression form.
3. Preserve its source span and render it only at the C emitter boundary.
4. Define stable instruction-level `ProgramPoint` values before and after
   instructions, before terminators, at suspension, and at continuation entry.
5. Collect await-result definitions and uses from structured CFG values and
   expressions.
6. Compute backward slot liveness to a fixed point.
7. Cover branch merges, loops, short circuiting, comma expressions, and nested
   awaits.
8. Keep generated C unchanged at every optimization level.
9. Prove program-point and liveness output is deterministic.

**Focused gates:**

```powershell
cargo test --lib semantic
cargo test --lib control_flow
cargo test --lib liveness
cargo test --lib slot_liveness
cargo test --test coroutine_contract representative_abi_v3_output_is_byte_stable
```

**Acceptance evidence:**

- Await-result use-def analysis is identity-based.
- Instruction-level liveness handles loops and branches conservatively.
- No physical field reuse is enabled yet.

### Task 6 completion evidence

Task 6 completed on July 15, 2026, with this evidence:

- `HirExprKind::AwaitResultRef(AwaitSlotId)` preserves result identity and the
  original source span without encoding identity in rendered source text.
- Composite expressions retain structured CR extensions until C emission.
- The emitter renders await-result access paths only at the final C boundary;
  the placeholder-rewrite scanner has been removed.
- Placeholder-shaped user declarations remain ordinary declarations and don't
  create false await-result uses or lifted fields.
- `ProgramPoint` identifies block entries, instruction boundaries,
  terminators, suspension observations, and continuation entries.
- `slot_liveness` computes backward slot liveness to a deterministic fixed
  point from structured expression, instruction, value, and terminator uses.
- A later await suspension keeps an earlier nested result live without marking
  the not-yet-produced result as live at the suspension point.
- Focused tests cover nested awaits, branch merges, loops, short circuiting,
  comma expressions, placeholder collisions, and repeated deterministic
  analysis.
- No physical field or child-payload reuse is enabled in this task.
- The representative ABI v3 golden remains byte-stable, and all optimization
  levels retain the Stage 3 generated output for the representative fixture.
- `cargo test --all-targets` passes, including 106 library tests and every
  native, project, ABI, static-await, cross-unit, optimization, and WebAssembly
  test.
- Required-mode `wasm32-wasi` compilation, linking, and validation pass with
  `CRC_REQUIRE_WASM` restored to its prior state after the gate.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 7: Model direct-child ownership interference

This task proves when direct child payloads can share storage across suspension
and parent-drop paths.

**Files:**

- Extend `src/slot_liveness.rs`.
- Extend `src/c_static_plan.rs` only when a stable child-to-edge query is
  missing.
- Extend `tests/coroutine_optimization.rs`.
- Extend `tests/static_await_codegen.rs` for lifecycle regression.

**Steps:**

1. Add failing tests for sequential, mutually exclusive, loop-carried, and
   simultaneously relevant direct children.
2. Model activation, pending retention, yielded retention, terminal
   finalization, invalid status, and parent drop.
3. Treat every direct child payload and its independent active flag as one
   logical ownership bundle for interference analysis.
4. Keep active flags outside the reusable payload graph.
5. Add a synthetic parent-drop observation that keeps the current active
   payload live.
6. Add interference between a child payload and any result copied before its
   finalization.
7. Exclude every declaration-owned binding and cleanup-referenced child.
8. Prove no physical slot can have two logical active flags true on one path.
9. Keep C emission unchanged.

**Focused gates:**

```powershell
cargo test --lib slot_liveness child
cargo test --test coroutine_optimization ownership
cargo test --test static_await_codegen
```

**Acceptance evidence:**

- Direct-child ownership is modeled across every terminal and suspension path.
- Bindings and cleanup-retained fields are conservatively excluded.
- Copy-before-finalize creates the required interference edge.

### Task 7 completion evidence

Task 7 completed on July 15, 2026, with this evidence:

- Each direct child has one `DirectChildBundle` keyed by `ChildInstanceId`, its
  owning await edge, and its result slot.
- Each bundle records activation, pending, yielded, ready-result-copy, error,
  canceled, invalid-status, and parent-drop observations.
- Every bundle keeps its active flag explicitly independent from reusable
  payload storage.
- Ownership observations contain at most one active direct child, proving the
  current lowering finalizes one child before another direct child activates.
- Sequential and mutually exclusive direct children don't interfere with each
  other.
- A later child interferes with every earlier await result that remains live
  across its suspension.
- A non-void direct child interferes with its result slot after result copy and
  before finalization, including when source code discards the result.
- Loop-carried direct children retain every active lifecycle observation and
  reach a deterministic fixed point.
- Declaration-owned task bindings are excluded as both binding-owned and
  cleanup-retained storage.
- Optimized-CFG integration produces deterministic ownership and interference
  results, and no ownership point can expose two direct active flags.
- Native lifecycle regression proves the first sequential child finalizes
  before the second activates and parent drop finalizes only the active second
  child, exactly once.
- No physical layout reuse or C emission change is enabled in this task.
- The representative ABI v3 golden remains byte-stable, and all Stage 3
  static, dynamic, recursive, and cross-unit await tests pass.
- `cargo test --all-targets` passes, including 110 library tests and every
  native, project, ABI, static-await, cross-unit, optimization, ownership, and
  WebAssembly test.
- Required-mode `wasm32-wasi` compilation, linking, and validation pass with
  `CRC_REQUIRE_WASM` restored to its prior state after the gate.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 8: Add identity physical layouts and access-path emission

This task introduces the physical layout abstraction with one independent slot
per logical field. Generated C must remain unchanged at `None`.

**Files:**

- Create `src/context_layout.rs`.
- Export the module from `src/lib.rs`.
- Modify `src/lib.rs` to build and pass a layout plan.
- Refactor `src/c_emitter.rs` to consume `CAccessPath` records.
- Add unit tests in `src/context_layout.rs`.
- Extend `tests/coroutine_optimization.rs`.

**Steps:**

1. Add failing tests for complete placement of lifted fields, await results,
   dynamic slots, and static direct child payloads.
2. Define stable logical field, physical slot, union member, placement,
   access-path, and decision records.
3. Build an identity layout that maps every logical field to an independent
   direct field with its existing name.
4. Keep task state, cleanup stack, terminal result, yielded value, sticky
   error, parameters, bindings, generations, and active flags independent.
5. Route context declarations and every field read or write through the
   layout plan.
6. Route parent drop, cleanup helpers, result propagation, and static and
   dynamic await code through validated access paths.
7. Diagnose a missing placement instead of emitting an implicit fallback
   field.
8. Prove `None` output remains byte-identical to the Stage 3 baseline.
9. Prove all executable Stage 3 lifecycle tests remain unchanged.

**Focused gates:**

```powershell
cargo test --lib context_layout
cargo test --lib c_emitter
cargo test --test coroutine_contract representative_abi_v3_output_is_byte_stable
cargo test --test static_await_codegen
cargo test --test static_await_project
```

**Acceptance evidence:**

- The emitter no longer decides storage reuse.
- Every private logical field has one validated access path.
- The identity plan preserves Stage 3 C exactly at `None`.

### Task 8 completion evidence

Task 8 completed on July 15, 2026, with this evidence:

- `context_layout` defines stable logical-field, physical-slot, union-member,
  placement, access-path, and layout-decision records.
- The identity planner preserves the Stage 3 declaration order and gives each
  logical field its own direct physical slot.
- State, status, sticky error, cleanup stack, lifted values, binding metadata,
  direct-child payloads, active flags, dynamic awaitables, await results,
  terminal results, and yielded values all have validated placements.
- Context declarations and all private context reads and writes now consume
  `CAccessPath` records; storage selection no longer occurs in the emitter.
- Layout verification runs before emission. Removing the state placement
  produces `CRC8004`, emits no source, and never synthesizes a fallback field.
- The `CAccessPath::UnionMember` form is reserved for later reuse tasks, but
  Task 8 emits only direct fields and enables no union or physical reuse.
- All four optimization levels preserve independent child payload, active-flag,
  and await-result fields in the integration fixture.
- The representative `None` ABI v3 golden remains byte-identical to the
  completed Stage 3 baseline.
- All Stage 3 static, dynamic, recursive, cross-unit, cleanup, error,
  cancellation, and lifecycle regressions remain unchanged.
- `cargo test --all-targets` passes, including 113 library tests and every
  native, project, ABI, static-await, cross-unit, optimization, ownership, and
  WebAssembly test.
- Required-mode `wasm32-wasi` compilation, linking, and validation pass with
  `CRC_REQUIRE_WASM` restored to its prior state after the gate.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 9: Enable `Speed` storage reuse

This task enables the default zero-runtime-cost context reduction for
compiler-owned fields with compatible declarators.

**Files:**

- Extend `src/context_layout.rs`.
- Extend union declaration and access emission in `src/c_emitter.rs`.
- Create `tests/context_layout_codegen.rs`.
- Extend generated-project regression tests.

**Steps:**

1. Add failing fixtures with sequential same-type await results and reusable
   direct dynamic child payloads.
2. Build an interference graph from slot and ownership liveness.
3. Use stable linear scan and first-fit placement for identical normalized C
   declarators.
4. Emit named unions for reused payloads and result fields.
5. Keep each direct child's active flag outside the union.
6. Require a dominating initialization or assignment before every union member
   read.
7. Make parent drop test the independent active flag before accessing a child
   payload member.
8. Enable reuse at `Speed`, `Size`, and `Aggressive`; keep `None` independent.
9. Compile and execute all fixtures across every optimization level.
10. Measure generated `sizeof(task)` and prove a representative strict
    reduction at `Speed`.
11. Assert no allocation, dynamic dispatch, clearing, tag, or new branch is
    added at static sites.

**Focused gates:**

```powershell
cargo test --lib context_layout speed
cargo test --test context_layout_codegen speed
cargo test --test coroutine_optimization
cargo test --test static_await_codegen
```

**Acceptance evidence:**

- Default `Speed` reduces a representative task context.
- No inactive union member is probed.
- Runtime instruction shape doesn't gain optimization bookkeeping.

### Task 9 completion evidence

Task 9 completed on July 15, 2026, with this evidence:

- `Speed` uses deterministic source-order linear scan and first-fit placement
  for fields with identical normalized C types.
- The eligible set contains compiler-owned await results, direct static child
  payloads, and direct dynamic `cr_awaitable` payloads.
- Simultaneously live await results and every ownership-interfering child or
  result pair remain in different physical slots.
- Named C unions preserve each member's declarator and route all reads and
  writes through validated `CAccessPath::UnionMember` records.
- Direct-child and dynamic-await active flags remain independent direct fields.
  Activation writes the selected member before setting its flag, and drop
  checks the flag before accessing the member.
- Layout verification rejects malformed slot shapes, invalid C access paths,
  and interfering logical fields placed in one physical slot.
- Sequential static children finalize exactly once when parent drop observes
  the second active flag through reused payload storage.
- Sequential dynamic awaitables reuse one payload slot only after the previous
  awaitable's drop callback completes; both callbacks run exactly once.
- Native generated-C measurement proves the representative `Speed` task is
  strictly smaller than its `None` task.
- Static polling gains no allocation, vtable dispatch, clearing, runtime tag,
  or optimization-only branch.
- `Size` and `Aggressive` retain the complete deterministic `Speed` layout in
  this task. Cross-type reuse remains disabled until Task 10 and Task 11.
- The representative `None` ABI v3 golden remains byte-identical to the
  completed Stage 3 baseline.
- `cargo test --all-targets` passes, including 115 library tests and every
  native, project, ABI, context-layout, static-await, cross-unit, optimization,
  ownership, and WebAssembly test.
- Required-mode `wasm32-wasi` compilation, linking, and validation pass with
  `CRC_REQUIRE_WASM` restored to its prior state after the gate.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 10: Add declarator-safe target layout knowledge

This task provides the exact size and alignment evidence required before
cross-type placement.

**Files:**

- Create `src/target_layout.rs`.
- Extend `src/context_layout.rs`.
- Extend `src/c_declaration_env.rs` for conservative packing barriers.
- Reuse structured declarator support from `src/symbol_index.rs` where
  possible.
- Add target-layout and declaration-environment unit tests.

**Steps:**

1. Add failing tests for fixed-width scalars, supported built-in types,
   pointers, arrays, generated task layouts, unknown typedefs, function
   pointers, and packing directives.
2. Define reviewed data models for host, Windows MSVC, Windows GNU, Linux GNU,
   Linux musl, macOS, and `wasm32-wasi` where the ABI fact is stable.
3. Represent every queried layout as `Exact` or `Unknown`.
4. Keep custom targets, unmodeled types, unsupported declarators, and packing
   environments unknown.
5. Produce declarator-safe union member declarations for supported fields.
6. Compute complete candidate context layout, including slot order, alignment,
   padding, and unchanged surrounding fields.
7. Refuse cross-type placement when any required layout fact is unknown.
8. Compare predicted sizes with native C `sizeof` for every modeled fixture.
9. Compare the `wasm32-wasi` model with a pinned WASI compilation fixture.
10. Keep physical layout behavior identical to `Speed` in this task.

**Focused gates:**

```powershell
cargo test --lib target_layout
cargo test --lib c_declaration_env
cargo test --lib context_layout layout_knowledge
$env:CRC_REQUIRE_WASM='1'
cargo test --test wasm_generated_project
Remove-Item Env:CRC_REQUIRE_WASM
```

**Acceptance evidence:**

- Cross-type layout never depends on a syntactic size guess.
- Unknown ABI facts retain the lower-level plan.
- Native and WASI `sizeof` agree with modeled exact layouts.

### Task 10 completion evidence

Task 10 completed on July 15, 2026, with this evidence:

- `target_layout` represents every ABI query as `Exact` or `Unknown` with a
  stable conservative reason.
- Reviewed models cover the host, Windows MSVC, Windows GNU, Linux GNU, Linux
  musl, macOS, and `wasm32-wasi` data models used by the current target names.
- The model resolves fixed-width integers, supported built-in scalars,
  pointers, constant-size arrays, function pointers, runtime ABI types, and
  already-computed generated task layouts.
- Unknown typedefs, tags, atomic types, `long double`, variable or flexible
  arrays, zero-length arrays, unsupported declarators, and custom targets
  remain unknown.
- The declaration environment detects `#pragma pack`, `_Pragma("pack")`,
  packed attributes, and alignment declarations as translation-unit packing
  barriers.
- Complete context layout computation accounts for physical slot order,
  member size and alignment, struct padding, union size, field offsets, and
  unchanged surrounding fields.
- Generated embedded task dependencies resolve through a deterministic fixed
  point; recursive typed edges remain pointers and don't require an infinite
  concrete layout.
- Native C `sizeof`, `_Alignof`, and `offsetof` values match the host model for
  scalar, pointer, array, function-pointer, runtime ABI, struct, and union
  fixtures.
- A generated coroutine task's modeled exact size matches its actual native C
  `sizeof` value.
- The pinned WASI SDK accepts compile-time assertions for the modeled
  `wasm32-wasi` size, alignment, and offsets before validating the generated
  WebAssembly module.
- Task 10 records layout knowledge only. `Speed`, `Size`, and `Aggressive`
  retain the complete Task 9 placement, and cross-type reuse remains disabled.
- The representative `None` ABI v3 golden remains byte-identical to the
  completed Stage 3 baseline.
- `cargo test --all-targets` passes, including 123 library tests and every
  native, project, ABI, context-layout, target-layout, static-await,
  cross-unit, optimization, ownership, and WebAssembly test.
- Required-mode `wasm32-wasi` compilation, model assertions, linking, and
  validation pass with `CRC_REQUIRE_WASM` restored after the gate.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 11: Enable `Size` cross-type and lifted-local reuse

This task enables exact-layout DSATUR placement while preserving `Speed` as the
complete incumbent.

**Files:**

- Extend `src/context_layout.rs`.
- Extend `src/liveness.rs` with the eligibility facts missing from lifted
  fields.
- Extend `src/c_emitter.rs` for cross-type union members.
- Extend `tests/context_layout_codegen.rs`.

**Steps:**

1. Add failing fixtures for cross-type result fields, arrays, embedded child
   payloads, and eligible lifted locals.
2. Add exclusion tests for parameters, address-taken values, volatile or
   atomic values, cleanup-retained values, and task bindings.
3. Build deterministic DSATUR ordering from stable logical identities and
   exact layout costs.
4. Start every function with its complete accepted `Speed` placement.
5. Evaluate the complete context layout for every cross-type candidate.
6. Accept a candidate only when exact target layout proves it is no larger
   than the `Speed` incumbent.
7. Retain `Speed` placement for unknown layouts and record the reason.
8. Enable this pass only for `Size` and `Aggressive`.
9. Compile and run differential lifecycle tests at all levels.
10. Prove `Size <= Speed` with generated C `sizeof` on every supported target
    fixture and prove a strict reduction on a cross-type fixture.
11. Prove union access preserves qualifiers and member declarators.

**Focused gates:**

```powershell
cargo test --lib context_layout size
cargo test --lib liveness
cargo test --test context_layout_codegen size
cargo test --test coroutine_optimization
```

**Acceptance evidence:**

- `Size` never replaces `Speed` with a larger exact-layout context.
- Eligible lifted locals reuse storage without alias or cleanup violations.
- Unknown and excluded fields remain independent.

### Task 11 completion evidence

Task 11 completed on July 15, 2026, with this evidence:

- Lifted declarations now have instruction-level liveness at block entry,
  before and after each instruction, before terminators, at suspension, and at
  continuation entry.
- Lifted-field facts record parameter ownership, task binding, address escape,
  cleanup-retained references, and volatile or atomic qualification.
- Declaration lowering preserves `const`, `volatile`, `restrict`, and
  `_Atomic` qualifiers in declarator-aware private field types.
- `Size` starts from the complete accepted `Speed` layout and treats each
  existing physical slot as one incumbent candidate node.
- Deterministic DSATUR ordering uses saturation, exact storage cost,
  interference degree, and stable original slot order.
- The interference graph combines lifted declaration liveness, await-result
  liveness, direct-child ownership, copy-before-finalize, and parent-drop
  observations.
- Candidate unions can contain different await-result types, embedded child
  task types, dynamic awaitables, arrays, function pointers, and eligible
  lifted locals.
- Parameters, address-taken values, cleanup-retained references, task bindings,
  volatile values, atomic values, active flags, generations, cleanup stacks,
  public results, yielded values, state, status, and errors remain independent.
- Every candidate is priced as a complete Context with target-specific size,
  alignment, padding, union layout, field order, and unchanged surrounding
  fields.
- `Size` accepts a candidate only when exact layout proves
  `candidate_size <= speed_size`; unknown target facts and packing barriers
  retain the complete `Speed` placement.
- Layout verification rejects interfering or ineligible fields in reusable
  physical storage.
- Native generated-C execution proves identical lifecycle and result behavior
  across `None`, `Speed`, `Size`, and `Aggressive`.
- Native `sizeof` proves `Size <= Speed` and a strict reduction for the
  cross-type lifted-array and function-pointer fixture.
- Generated unions preserve array dimensions and function-pointer member
  declarators, and all accesses use validated member paths.
- Modeled `Size <= Speed` holds for Windows MSVC, Windows GNU, Linux GNU,
  Linux musl, macOS, and `wasm32-wasi` fixtures.
- The required WASI project selects `optimization = "size"`, emits a real
  cross-type lifted-local union, compiles with pinned WASI Clang, links, and
  validates successfully.
- `Aggressive` retains the complete deterministic `Size` layout until Task 12.
- The representative `None` ABI v3 golden remains byte-identical to the
  completed Stage 3 baseline.
- `cargo test --all-targets` passes, including 128 library tests and every
  native, project, ABI, context-layout, target-layout, static-await,
  cross-unit, optimization, ownership, and WebAssembly test.
- Required-mode `wasm32-wasi` compilation, linking, and validation pass with
  `CRC_REQUIRE_WASM` restored after the gate.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 12: Enable deterministic bounded `Aggressive` placement

This task adds a more expensive placement search without introducing
time-dependent output.

**Files:**

- Extend `src/context_layout.rs`.
- Extend context-layout unit and integration tests.

**Steps:**

1. Add small interference graphs with a known optimal weighted placement.
2. Implement deterministic branch-and-bound search with the accepted `Size`
   layout as the initial incumbent.
3. Order search nodes and candidate slots by stable logical and layout keys.
4. Bound work by a configured compiler-private node count, not wall-clock
   time.
5. Retain `Size` when no strictly better or equal simpler candidate is proven.
6. Fall back to `Size` when the node budget is exhausted.
7. Record explored nodes and fallback decisions for structural tests.
8. Compare small-graph results with exhaustive enumeration.
9. Prove repeated, reordered, and parallel planning produces identical output.
10. Prove generated C satisfies `Aggressive <= Size` on every exact-layout
    fixture.

**Focused gate:**

```powershell
cargo test --lib context_layout aggressive
cargo test --test context_layout_codegen aggressive
```

**Acceptance evidence:**

- Aggressive output is deterministic and node-budgeted.
- The accepted `Size` plan is always a valid fallback.
- `Aggressive` never produces a larger supported exact-layout context.

### Task 12 completion evidence

Task 12 completed on July 15, 2026, with this evidence:

- `Aggressive` uses the same stable candidate nodes and ownership-aware
  interference graph as `Size`.
- The accepted complete `Size` layout is the initial incumbent. When `Size`
  retains `Speed`, the equivalent independent candidate coloring becomes the
  incumbent.
- The search objective is lexicographic: complete context size, physical slot
  count, and union access-path complexity. Stable canonical color assignment
  resolves otherwise equivalent search results without publishing an
  equal-cost layout as an optimization.
- Search nodes use deterministic DSATUR ordering. Candidate slots use stable
  incremental storage cost, existing-slot preference, resulting weight, and
  slot identity.
- Branch-and-bound uses conservative storage, slot-count, and access-complexity
  lower bounds. It never estimates a context as larger than the exact target
  layout can prove.
- The compiler-private budget is exactly 100,000 deterministic search nodes.
  The implementation doesn't read elapsed time or machine-dependent timing.
- Budget exhaustion discards every partial or provisional improvement and
  restores the exact `Size` incumbent.
- `AggressiveLayoutDecision` records accepted placement, retained `Size`,
  budget exhaustion, unknown layout facts, complete context sizes, and explored
  node counts for structural tests and future explain tooling.
- A known weighted graph improves from the DSATUR cost of 8 bytes to the
  exhaustive optimum of 6 bytes. An independent enumerator produces the same
  placement and cost.
- A second graph proves the tie-break order by reducing access complexity from
  5 to 4 while retaining the same context size and slot count.
- A one-node budget test proves exact incumbent retention and records exactly
  one explored node.
- Repeated, reordered, and parallel searches produce identical colorings,
  costs, explored-node counts, and fallback decisions.
- Exact target-model tests prove `Aggressive <= Size <= Speed` for Windows
  MSVC, Windows GNU, Linux GNU, Linux musl, macOS, and `wasm32-wasi`.
- Repeated generated-C compilation is byte-identical, and native `sizeof`
  execution proves `Aggressive <= Size` on the cross-type fixture.
- The required WASI generated project now selects
  `optimization = "aggressive"`, emits cross-type union storage, compiles and
  links with pinned WASI Clang, and passes `wasm-tools validate`.
- The representative `None` ABI v3 golden remains byte-identical, and static,
  dynamic, embedded, boxed, recursive, cross-unit, ownership, cancellation,
  CMake, and Meson regressions remain green.
- `cargo test --all-targets` passes with 132 library tests and every integration
  test. Required-mode WASI validation also passes with `CRC_REQUIRE_WASM`
  restored to its original unset state.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 13: Run final native, ABI, and WebAssembly gates

This task proves Stage 4 is complete without starting Stage 5.

**Files:**

- Modify implementation files only to fix failures found by final gates.
- Update this plan's live status after every gate passes.
- Update the Stage 4 design only when implementation exposes a genuine
  specification defect.

**Steps:**

1. Run differential execution across all four optimization levels.
2. Run context-size, block-count, state-count, determinism, and inactive-member
   structural gates.
3. Run embedded, boxed, cross-unit, and dynamic Stage 3 regressions.
4. Run native generated projects, CMake, and Meson.
5. Run portable and computed-goto backend tests.
6. Run ABI v3 layout, malformed protocol, header, and golden tests.
7. Run required `wasm32-wasi` planning, compilation, linking, and validation.
8. Run the supported native compiler union-layout matrix.
9. Search generated output for static vtable fallback, reusable binding fields,
   inactive union probing, and optimization runtime tags.
10. Confirm Stage 5 waker, executor, and backend SPI behavior remains absent.
11. Record the final changed-file list and gate evidence.
12. Mark Stage 4 complete only when every named gate passes.

**Final commands:**

```powershell
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
pnpm run grammar:test
$env:CRC_REQUIRE_WASM='1'
cargo test --test wasm_generated_project
Remove-Item Env:CRC_REQUIRE_WASM
cargo test --test coroutine_optimization
cargo test --test context_layout_codegen
cargo test --test static_await_codegen
cargo test --test static_await_project
cargo test --test abi_v3_layout
cargo test --test abi_v3_protocol
cargo test --test coroutine_contract
rg -n "vtable|into_awaitable|as_awaitable" tests src/c_emitter.rs
rg -n "generation|cleanup_payload|active" tests/context_layout_codegen.rs
rg -n "waker|executor|reactor" src tests
```

The vtable search can find public adapters and genuine dynamic await paths, but
no static parent await site can use them. The cleanup search must prove binding
and cleanup-retained fields remain independent. The Stage 5 search can find
documentation or reserved ABI terms, but no new runtime behavior.

**Acceptance evidence:**

- Default `Speed` reduces representative task context or CFG state shape.
- `Size` and `Aggressive` satisfy their incumbent monotonicity contracts.
- Every optimization level matches `None` observable behavior.
- Required native and WebAssembly gates pass.
- Public ABI v3 and Stage 3 dispatch remain unchanged.
- No Stage 5 behavior appears in implementation.

### Task 13 completion evidence

Task 13 completed on July 15, 2026, with this evidence:

- Differential native execution produces identical observable behavior for
  `None`, `Speed`, `Size`, and `Aggressive`.
- Verified block and state metrics remain deterministic. Child active flags,
  task binding generations, cleanup ownership, and inactive-member rules remain
  independent of physical storage reuse.
- Native generated-C execution proves `Speed < None` on the representative
  reuse fixture and `Aggressive <= Size <= Speed` on the cross-type fixture.
- The native union-layout matrix compiles and executes the same generated
  `Speed`, `Size`, and `Aggressive` C with both Clang and GCC. Both compilers
  satisfy the context-size monotonicity contract.
- Embedded, boxed, directly recursive, mutually recursive, cleanup-failure,
  static binding, dynamic await, and cross-translation-unit Stage 3 regressions
  pass without dynamic fallback at static parent await sites.
- Generated projects compile and execute through the direct native compiler,
  CMake, and Meson workflows. Failed and incremental builds preserve their
  publication contracts.
- Portable C11 runtime and generated headers compile with warnings treated as
  errors. The GNU computed-goto backend also passes its focused gate.
- ABI v3 layout, malformed object protocol, public coroutine contract, and the
  representative byte-stable `None` golden all pass unchanged.
- Static dispatch auditing finds vtables only in public adapters and genuine
  dynamic await paths. Static parent poll tests explicitly reject
  `into_awaitable`, `as_awaitable`, and vtable polling.
- Ownership auditing finds no runtime storage tag or optimization-level runtime
  branch. Active flags and task binding generations remain independent fields,
  and cleanup helpers retain direct ownership paths.
- Stage 5 auditing finds only the existing ABI v3 `cr_waker` opaque pointer and
  capability validation. No executor, reactor, backend SPI, scheduling, or wake
  behavior has entered the implementation.
- The required generated project selects `optimization = "aggressive"` and
  `target = "wasm32-wasi"`. Pinned WASI Clang compiles and links the generated
  portable C, and pinned `wasm-tools` validates the final module.
- Required-mode WASI validation restores `CRC_REQUIRE_WASM` to its original
  unset state after the gate.
- `cargo test --all-targets` passes with 132 library tests and every ABI,
  context-layout, optimization, generated-project, static-await, target-layout,
  and WebAssembly integration test.
- `pnpm run grammar:test` passes all four corpus parses with no failures.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.
- Task 13 changes only
  `tests/context_layout_codegen.rs` and this implementation plan. The test
  change adds the Clang and GCC union-layout matrix; no production or public ABI
  file required a final-gate fix.
- No stop condition triggered, so the approved Stage 4 design remains
  unchanged.

## Stop conditions

Stop and amend the design instead of broadening implementation when any of
these conditions occurs:

- A CFG transform requires evaluating an arbitrary C expression.
- A block remap can't preserve logical edge, cleanup, or source identity.
- A reuse candidate needs a runtime tag, new branch, clearing, or copy.
- Parent drop must inspect an inactive union member.
- A cleanup helper must resolve a reusable union access path.
- A task binding must be reused to reach the target context reduction.
- Cross-type reuse depends on an unknown size, alignment, or packing rule.
- `Size` can't retain the complete `Speed` plan as its incumbent.
- `Aggressive` can't retain the complete `Size` plan as its incumbent.
- A static target requires dynamic fallback.
- Public ABI v3 or cross-unit opacity must change.
- Correctness depends on a waker, asynchronous drop, or Stage 5 behavior.
- Portable C11 or `wasm32-wasi` requires a target-specific public runtime
  branch.
- Differential execution exposes an unexplained semantic change.

## Next steps

Stage 4 is complete. Don't begin Stage 5 waker, executor, or backend work until
its language and runtime contracts have an approved design and implementation
plan.
