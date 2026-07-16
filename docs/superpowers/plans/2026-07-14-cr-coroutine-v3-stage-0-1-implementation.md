# CR coroutine v3 stage 0-1 implementation plan

This plan implements the specification and IR foundations for the approved CR
coroutine architecture. It covers Stage 0 and Stage 1 only. Runtime ABI v3,
static-await code generation, CFG optimization, and waker behavior require
separate plans after this plan passes its final gate.

The requirements come from the approved
[CR coroutine architecture and ABI v3 design](../specs/2026-07-14-cr-coroutine-architecture-v3-design.md).

> **Note:** This plan prepares a preview architecture. Production output must
> continue to use ABI v2 until a later ABI v3 migration plan is approved.

## Outcome

At completion, the compiler has executable behavior baselines, pinned
WebAssembly compile tooling, stable async function identities, child-instance
and await-edge metadata, deterministic SCC analysis, and an await storage plan.
The C emitter continues to produce the same ABI v2 output.

The implementation must satisfy these invariants:

- Existing CR source and generated-project tests remain unchanged in behavior.
- Existing generated C remains byte-for-byte stable unless a test fixture is
  changed only to expose previously nondeterministic output.
- Every static direct call and task binding receives a stable function target.
- Every await receives an `AwaitEdgeId`.
- Every edge-owned or declaration-owned child receives one `ChildInstanceId`.
- Multiple awaits of one task binding reference the same child instance.
- Recursive embedded-layout graphs become acyclic through deterministic boxed
  edge selection.
- No Stage 0 or Stage 1 change activates ABI v3 or static dispatch.

## Working constraints

This workspace isn't a Git repository, so the implementation can't create the
commits normally associated with plan checkpoints. Each task instead ends with
explicit tests and a live status update that lists changed files and evidence.

Use `apply_patch` for source and documentation edits. Use Cargo or pnpm commands
for dependency changes. Don't hand-edit generated Tree-sitter parser output.
Use the `modern-project-standards` skill before selecting or changing pinned
WASI SDK or `wasm-tools` versions.

## Live status

The plan starts with one active task and keeps later work pending until its
dependency gate passes.

```text
Completed: approved architecture design and implementation plan
Completed: Task 1, core contract and ABI v2 behavior baseline
Completed: Task 2, pinned WebAssembly compile toolchain
Completed: Task 3, stable project async symbol identities
Completed: Task 4, identity-based async calls and task references
Completed: Task 5, child-instance and await-edge metadata
Completed: Task 6, deterministic call graph and SCC analysis
Completed: Task 7, embedded, boxed, and opaque storage planning
Completed: Task 8, planning integration with ABI v2 output
Completed: Task 9, Stage 0-1 final gate
In progress: none
Pending: none
Blocked: none
```

The final gate completed with no remaining failures. Stage 0-1 changed these
implementation surfaces:

- Added `src/symbol_index.rs` and `src/await_plan.rs`.
- Updated `src/semantic.rs`, `src/control_flow.rs`, `src/scope_exit.rs`,
  `src/coroutine.rs`, `src/liveness.rs`, `src/c_emitter.rs`, and `src/lib.rs`.
- Added RFC0001, coroutine contract fixtures, the ABI v2 byte baseline, and the
  WebAssembly generated-project gate.
- Added exact WASI SDK and `wasm-tools` version files under `tools/`.

The final evidence includes `cargo test --all-targets` with 79 library tests,
two coroutine contract tests, five generated-project tests, and one WebAssembly
test. Formatting, all-target checks, Clippy with warnings denied, the grammar
corpus, and required-mode WebAssembly validation also pass. Production source
contains no ABI v3 declarations, and `AwaitStorage::Unplanned` appears only in
plan construction, validation, and tests.

After each task, update this block and the active execution plan. A task is
complete only when all named tests pass.

## Task 1: Add the core contract RFC and ABI v2 behavior baseline

This task converts approved observable behavior into one concise RFC and tests
the behavior that already exists before any IR change.

**Files:**

- Create `docs/rfcs/0001-core-coroutine-contract.md`.
- Create `tests/coroutine_contract.rs`.
- Create `tests/fixtures/planning/representative.cr`.
- Create `tests/fixtures/planning/abi-v2-baseline.c`.
- Modify `Cargo.toml` only if the new integration test needs an existing test
  utility exposed as a development dependency.

**Steps:**

1. Write failing product-boundary tests that compile generated C and verify
   repeated ready, error, and canceled polls.
2. Add a test that verifies yielded is transient and resumes on the next poll.
3. Add tests for exactly-once child drop and defer execution on error and
   cancellation.
4. Add tests for borrowed and owning ABI v2 adapters without changing their
   current layout.
5. Generate and review one representative ABI v2 C artifact that contains a
   direct call, task binding, and dynamic await, then store it as the Stage 1
   byte-for-byte baseline.
6. Add a test that regenerates the artifact and compares all bytes with the
   checked-in baseline.
7. Run the focused test and confirm that every failure represents a missing
   fixture or a real discrepancy, not an intended ABI v3 change.
8. Write RFC0001 with poll, error, drop, ownership, and nonconcurrent-poll
   contracts from the approved design.
9. Classify each documented surface as stable v3 semantics, temporary ABI v2
   layout, or experimental future extension.
10. Run the focused test again and retain current ABI v2 implementation
    behavior.

**Focused gate:**

```powershell
cargo test --test coroutine_contract
```

**Acceptance evidence:**

- The test executes generated C through the public `Compiler` entry point.
- Terminal-state and cleanup behavior is proven without asserting ABI v3
  declarations.
- RFC0001 contains no task, context, or embedded-slot public layout.

## Task 2: Pin and test the WebAssembly compile toolchain

This task creates a repeatable compile-and-link gate without making WebAssembly
the primary execution target.

**Files:**

- Create `tools/wasi-sdk.version`.
- Create `tools/wasm-tools.version`.
- Create `tests/wasm_generated_project.rs`.
- Modify project test support only if command-discovery code can be shared
  without changing existing test behavior.

**Steps:**

1. Use the dependency-management workflow to choose exact tested WASI SDK and
   `wasm-tools` versions.
2. Write both exact version files and reject empty or nonexact values in the
   test harness.
3. Generate the standard project fixture with portable C11 code generation.
4. Compile and link `crc/dist/*.c` and `src/main.c` with WASI SDK Clang
   targeting `wasm32-wasi`.
5. Validate the linked module with the pinned `wasm-tools` executable.
6. Make the test skip with an explicit reason when the toolchain is absent in a
   normal local run.
7. Make `CRC_REQUIRE_WASM=1` turn an absent or mismatched toolchain into a test
   failure for CI and release gates.
8. Reject computed-goto configuration in the WebAssembly fixture.

**Focused gates:**

```powershell
cargo test --test wasm_generated_project
$env:CRC_REQUIRE_WASM='1'
cargo test --test wasm_generated_project
```

The second command requires the pinned toolchain to be installed. Clear the
environment variable after the gate.

**Acceptance evidence:**

- A linked `.wasm` module passes validation.
- The fixture uses the same generated runtime header as native builds.
- Native tests don't require a WebAssembly installation.

## Task 3: Add stable project async symbol identities

This task introduces cross-translation-unit symbol identity before changing
HIR or generated output.

**Files:**

- Create `src/symbol_index.rs`.
- Modify `src/lib.rs` to export the module and build a project index.
- Modify `src/header_emitter.rs` only if shared declaration extraction is
  needed.
- Add unit tests in `src/symbol_index.rs`.

**Data model:**

```rust
pub struct FunctionId(pub u32);

pub enum AsyncLinkage {
    External,
    Internal(PathBuf),
}

pub struct AsyncLinkageKey {
    pub linkage: AsyncLinkage,
    pub name: String,
}

pub struct AsyncSymbolSite {
    pub project_path: PathBuf,
    pub source_start: usize,
    pub kind: AsyncSymbolSiteKind,
    pub result_type: SourceFragment,
}

pub struct AsyncSymbol {
    pub id: FunctionId,
    pub key: AsyncLinkageKey,
    pub sites: Vec<AsyncSymbolSite>,
    pub public_stem: String,
}

pub struct ResolvedAsyncSymbol {
    pub id: FunctionId,
    pub layout_visibility: LayoutVisibility,
}

pub enum LayoutVisibility {
    Visible,
    Opaque,
}
```

**Steps:**

1. Write tests that index two source definitions and one `.hr` declaration in
   different input orders.
2. Resolve C external linkage by generated symbol name, and resolve `static`
   async functions by translation-unit path plus source name.
3. Keep declaration and definition paths as symbol sites rather than part of an
   external linkage key.
4. Assert that IDs remain stable after sorting by linkage kind, normalized
   internal-linkage path, and symbol name.
5. Assert that a local definition is visible and a header-only declaration is
   opaque in another translation unit.
6. Assert that an external `.hr` declaration and `.cr` definition resolve to
   one `FunctionId` with two sites.
7. Add duplicate-definition and conflicting-result-type diagnostics with source
   spans from both declarations.
8. Implement deterministic path normalization without resolving paths outside
   the project root.
9. Compute layout visibility during lookup relative to the current translation
   unit; don't store one global visibility value on a symbol.
10. Expose a complete index builder for later project-pipeline integration.
11. Expose local-index construction for standalone `compile_source`.
12. Prove that the existing compiler and emitter don't read the new index yet;
    Task 8 performs the production-pipeline integration.

**Focused gate:**

```powershell
cargo test --lib symbol_index
```

**Acceptance evidence:**

- Reordering project file discovery doesn't change `FunctionId` assignments.
- Header declarations and definitions share one external linkage identity.
- Equal internal names in different translation units remain distinct.
- Existing project artifacts remain byte-identical.

## Task 4: Resolve async calls and task references by identity

This task removes string-only identity from CR-specific expressions while
preserving source spelling for C emission and diagnostics.

**Files:**

- Modify `src/semantic.rs`.
- Modify `src/lib.rs` to pass the symbol index into semantic construction.
- Modify `src/c_emitter.rs` only to render new identity-bearing HIR variants as
  the same ABI v2 C text.
- Add semantic unit tests in `src/semantic.rs`.

**HIR changes:**

```rust
AsyncCall {
    target: Option<FunctionId>,
    callee: String,
    result_type: SourceFragment,
    arguments: Vec<HirExpr>,
}

TaskRef {
    declaration: DeclarationId,
    name: String,
}
```

**Steps:**

1. Add failing tests for a local async call, a header-declared async call, a
   shadowed task name, and two task bindings with the same source name in
   nested scopes.
2. Add a post-construction CR reference-resolution pass that walks expressions
   with their lexical scope stacks.
3. Resolve only CR async functions and `__async` task bindings; don't attempt a
   complete C type or ordinary identifier resolver in this task.
4. Preserve callee spelling and source spans alongside `FunctionId`.
5. Diagnose a source construct that claims a static async call but has no
   indexed target.
6. Render `TaskRef` with its original C identifier so ABI v2 output doesn't
   change.
7. Run existing semantic, CFG, liveness, and emitter tests.

**Focused gates:**

```powershell
cargo test --lib semantic
cargo test --lib control_flow
cargo test --lib c_emitter
```

**Acceptance evidence:**

- Shadowed task bindings have distinct `DeclarationId` values.
- Cross-unit calls carry stable `FunctionId` values.
- Existing generated C snapshots and executable fixtures remain unchanged.

## Task 5: Build child-instance and await-edge metadata

This task adds the orthogonal target and storage representation without
changing CFG control-flow semantics.

**Files:**

- Create `src/await_plan.rs`.
- Modify `src/lib.rs` to export the module.
- Modify `src/control_flow.rs` to assign stable await-edge identities.
- Modify `src/coroutine.rs` to retain planning metadata without consuming it.
- Add unit tests in `src/await_plan.rs` and `src/control_flow.rs`.

**Data model:**

```rust
pub struct AwaitEdgeId(pub u32);
pub struct ChildInstanceId(pub u32);
pub struct ValueId(pub u32);
pub struct ChildSlotId(pub u32);
pub struct TypedSlotId(pub u32);

pub enum ChildOrigin {
    Direct(AwaitEdgeId),
    Binding(DeclarationId),
}

pub enum AwaitTarget {
    Static(FunctionId),
    Dynamic(ValueId),
}

pub enum AwaitStorage {
    Unplanned,
    Embedded(ChildSlotId),
    Boxed(TypedSlotId),
    Opaque(AwaitSlotId),
}
```

**Steps:**

1. Add failing tests that assign one edge-owned instance to a direct await.
2. Add a test in which two await edges reference one declaration-owned task
   instance.
3. Add tests for a dynamic await expression and an await inside a loop.
4. Assign edge IDs in deterministic block and source-span order.
5. Assign a stable value ID and retain the source-backed operand for each
   dynamic target.
6. Assign direct child instances per edge and binding child instances per
   declaration.
7. Record each child's target, origin, result layout, ownership, source span,
   and unplanned storage.
8. Keep generation and activation flags as future code-generation metadata;
   Stage 1 models their required semantics but doesn't emit them.
9. Attach the plan to `CoroutineFunction` or a neighboring unit without
   changing the existing state and await-slot representation.

**Focused gates:**

```powershell
cargo test --lib await_plan
cargo test --lib control_flow
cargo test --lib coroutine
```

**Acceptance evidence:**

- One binding has one child instance across every referencing await edge.
- A repeated direct edge retains one compile-time instance site.
- Every storage value remains `Unplanned` before the planning pass.

## Task 6: Add deterministic call graph and SCC analysis

This task identifies recursive layout cycles independently of storage policy.

**Files:**

- Extend `src/await_plan.rs`.
- Add pure graph unit tests and source-backed integration tests in that module.

**Steps:**

1. Write pure graph tests for an acyclic chain, a self-loop, one three-function
   cycle, two connected SCCs, and disconnected functions.
2. Build graph edges from static child instances, including instances created
   by task-binding declarations.
3. Implement Tarjan SCC analysis with deterministic node and edge iteration.
4. Sort graph keys by normalized caller path, caller source start, child source
   start, and callee symbol key.
5. Prove that shuffled source discovery and map insertion produce identical
   SCC and edge order.
6. Keep dynamic targets out of the static call graph.

**Focused gate:**

```powershell
cargo test --lib await_plan::tests::scc
```

**Acceptance evidence:**

- Self-recursion is reported as a cyclic SCC.
- Mutual recursion produces one stable SCC regardless of input order.
- Acyclic static calls remain eligible for embedded storage.

## Task 7: Plan embedded, boxed, and opaque storage

This task produces a deterministic maximal acyclic embedded graph but doesn't
activate its code-generation choices.

**Files:**

- Extend `src/await_plan.rs`.
- Add planner tests in `src/await_plan.rs`.

**Steps:**

1. Write failing tests for same-unit acyclic embedded storage.
2. Add tests for opaque cross-unit static storage and dynamic opaque storage.
3. Add self-recursive and mutually recursive tests that box only edges required
   to keep embedded layouts acyclic.
4. Preselect `Opaque` for dynamic targets and `Boxed` for opaque static layouts.
5. Consider remaining candidates in the approved stable key order.
6. Add a candidate to the embedded graph only when it doesn't create a cycle;
   otherwise select `Boxed`.
7. Add a validation pass that rejects any remaining unplanned storage or cycle
   in the embedded graph.
8. Snapshot the plan, not generated C, so Stage 1 doesn't alter output.

**Focused gate:**

```powershell
cargo test --lib await_plan::tests::storage
```

**Acceptance evidence:**

- A simple recursive edge is boxed and remains statically targeted.
- Cross-unit static calls are boxed rather than dynamic.
- Every planner result is deterministic and contains no embedded cycle.

## Task 8: Integrate planning while preserving ABI v2 output

This task makes planning part of every compiler path while keeping the current
emitter authoritative.

**Files:**

- Modify `src/lib.rs`.
- Modify `src/coroutine.rs`.
- Modify `src/c_emitter.rs` only for identity rendering and explicit legacy
  plan ignoring.
- Modify `tests/generated_project.rs`.
- Add integration coverage to `tests/coroutine_contract.rs`.

**Steps:**

1. Insert symbol-index construction before HIR in project and standalone paths.
2. Insert await planning after scoped CFG and before coroutine lowering.
3. Propagate planner diagnostics through the existing diagnostic channel.
4. Keep ABI v2 await slots and emission selected for every storage plan.
5. Add a test-only plan inspection API instead of exposing CIR metadata through
   the public C ABI.
6. Compile a same-unit direct call, self-recursive call, task binding, and
   dynamic await through the complete pipeline.
7. Compare generated C with pre-integration golden strings or hashes.
8. Run generated native, CMake, and Meson projects.

**Focused gates:**

```powershell
cargo test --test coroutine_contract
cargo test --test generated_project
cargo test --lib
```

**Acceptance evidence:**

- Every compile path produces a complete await plan.
- ABI v2 runtime headers and public generated signatures don't change.
- Existing `crc-demo` behavior and generated-project behavior remain stable.

## Task 9: Run the Stage 0-1 final gate

This task proves that planning infrastructure is complete and that no future
runtime behavior leaked into production output.

**Files:**

- Update this plan's live status.
- Update the approved design's progress block.
- Modify implementation files only to fix failures found by the gate.

**Steps:**

1. Run formatting, checks, all tests, and Clippy.
2. Run the grammar corpus even though Stage 0-1 doesn't change grammar.
3. Run native generated-project, CMake, and Meson gates.
4. Run the required WebAssembly gate with `CRC_REQUIRE_WASM=1`.
5. Search generated headers and source for accidental ABI v3 declarations.
6. Search production code for unplanned storage reaching coroutine lowering.
7. Record the final changed-file list and gate outputs.
8. Mark Stage 0 and Stage 1 complete only after every required gate passes.

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
rg -n "CR_RUNTIME_ABI_VERSION 3|cr_poll_context" src tests
rg -n "AwaitStorage::Unplanned" src
```

The first search must find only design, plan, test, or explicitly inactive
future declarations. The second search must find planner construction and
validation but no unvalidated path into coroutine lowering.

## Stop conditions

Stop the plan and report the evidence instead of broadening scope when any of
these conditions occurs:

- Stable function identity requires full C name or type resolution beyond CR
  async declarations.
- Header declarations can't be associated with definitions without active
  preprocessor correlation that Stage 1 doesn't provide.
- Preserving ABI v2 output conflicts with the new identity representation.
- A required WebAssembly toolchain can't be pinned or installed in CI.
- Task-binding identity requires changing language semantics beyond the
  approved contract.

A stop condition triggers a focused design amendment. It doesn't authorize an
ABI v3 shortcut or a text-based identity fallback.

## Next steps

After this plan passes, write and approve the Stage 2 ABI v3 implementation
plan. That plan must define exact runtime declarations, public header migration,
malformed-object conformance tests, template changes, and the single authorized
ABI break. Static-await code generation remains Stage 3 and starts only after
ABI v3 passes its full gate.
