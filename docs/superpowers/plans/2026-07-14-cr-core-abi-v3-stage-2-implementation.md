# CR core ABI v3 Stage 2 implementation plan

This plan migrates CR from Runtime ABI v2 to the approved core ABI v3. It
performs the single authorized public ABI break, adds executable malformed-
object conformance, and preserves Stage 1 planning without enabling static
await dispatch.

The requirements come from the approved
[Stage 2 design](../specs/2026-07-14-cr-core-abi-v3-stage-2-design.md), the
[architecture design](../specs/2026-07-14-cr-coroutine-architecture-v3-design.md),
and [RFC0001](../../rfcs/0001-core-coroutine-contract.md).

> **Note:** This workspace isn't a Git repository. Each task ends with named
> gates and a live status update instead of a commit checkpoint.

## Outcome

At completion, generated tasks expose ABI v3 poll signatures, dynamic
awaitables contain only state and a shared vtable, every dynamic callback path
is validated, and ABI v2 no longer exists in production code. Native remains
the primary execution gate, and portable C11 must continue to compile and link
for the pinned `wasm32-wasi` toolchain.

Stage 2 must satisfy these invariants:

- `cr_awaitable` contains exactly two machine words.
- Public and internal task poll functions accept nullable poll context.
- Vtable layout validates once before slot activation.
- Required capabilities validate before every child poll.
- Edge-owned and declaration-owned children use different cleanup timing.
- A task binding owns one child generation across repeated awaits.
- Every failure path reaches a sticky terminal status and drops exactly once.
- Stage 1 storage planning remains complete but static dispatch stays disabled.
- No ABI v2 compatibility branch remains after the final gate.

## Live status

The plan completed on July 14, 2026. Every implementation task and named gate
passed without enabling Stage 3 static dispatch.

```text
Completed: Stage 0 specification and ABI v2 baseline
Completed: Stage 1 identities, graph analysis, and await storage planning
Completed: Stage 2 design and independent specification review
Completed: Tasks 1 through 8, core ABI v3 implementation and conformance
In progress: none
Pending: none
Blocked: none
```

## Completion evidence

The final implementation migrates the runtime, emitter, generated consumers,
and conformance suite together. The principal changed files are:

- `src/runtime_abi.rs`, `src/header_emitter.rs`, `src/c_emitter.rs`, and
  `src/liveness.rs`.
- `src/template/templates/main.c`.
- `tests/abi_v3_layout.rs`, `tests/abi_v3_protocol.rs`,
  `tests/coroutine_contract.rs`, and `tests/generated_project.rs`.
- `tests/fixtures/planning/abi-v3-baseline.c`.
- RFC0001, the architecture design, the Stage 2 design, and this plan.

The final gate produced this evidence:

- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` passed.
- `cargo test --all-targets` passed all Rust and generated-C tests.
- Native, CMake, and Meson generated-project tests passed.
- Required-mode `wasm32-wasi` compilation, linking, and module validation
  passed.
- `pnpm run grammar:test` passed all Tree-sitter fixtures.
- Production source, tests, templates, and generated fixtures contain no ABI
  v2 path or ownership flag.
- `src/c_emitter.rs` contains no embedded or boxed static-await emission.

## Task 1: Establish the ABI v3 runtime header and layout gate

This task changes public declarations first and proves their target layouts
without changing generated coroutine emission yet.

**Files:**

- Modify `src/runtime_abi.rs`.
- Create `tests/abi_v3_layout.rs`.
- Create target fixtures under `tests/fixtures/abi-v3/` when exact layout
  values require separate native and `wasm32-wasi` sources.

**Steps:**

1. Write failing tests for ABI version 3, fixed-width poll constants,
   `int32_t` error code, poll context, vtable, and two-word awaitable.
2. Assert version constants and `offsetof`-based minimum-prefix macros.
3. Add all stable protocol error codes from 1101 through 1109.
4. Define the known capability mask and advisory yield flag.
5. Keep cleanup-stack helpers available without treating their layout as a
   stable public ABI.
6. Compile layout fixtures with native C11 warnings denied.
7. Compile the same declarations for pinned `wasm32-wasi` and validate the
   linked module.
8. Remove ABI v2 declarations from the runtime header only after the new
   layout test passes.

**Focused gate:**

```powershell
cargo test --test abi_v3_layout
```

**Acceptance evidence:**

- The awaitable size equals two pointer widths on native and WebAssembly.
- Minimum-prefix macros end at `waker` and `value_align`.
- No per-instance callback, layout, or ownership flag remains.

## Task 2: Migrate task poll signatures and public headers

This task threads poll context through generated task entry points before
changing the awaitable representation.

**Files:**

- Modify `src/header_emitter.rs`.
- Modify task poll declaration and definition emission in `src/c_emitter.rs`.
- Modify generated-header tests in `src/header_emitter.rs`.
- Modify direct generated-C callers in `src/c_emitter.rs` tests.

**Steps:**

1. Write failing header tests for the two-argument public poll signature.
2. Add `const cr_poll_context *poll_context` to internal and public task poll.
3. Return sticky terminal status before validating a new context.
4. Treat null context as manual polling with zero capabilities.
5. Validate non-null version, minimum size, and Waker bit/pointer consistency.
6. Route invalid context through error cleanup with code 1101.
7. Update every manual generated-C poll call to pass `NULL`.
8. Retain null-task behavior and accessor preconditions.

**Focused gates:**

```powershell
cargo test --lib header_emitter
cargo test --lib c_emitter
```

**Acceptance evidence:**

- Generated `.h` and `.c` declarations agree exactly.
- Repeated terminal polls ignore a later malformed context.
- Nonterminal malformed contexts become sticky error.

## Task 3: Emit shared vtables and two-word awaitables

This task replaces per-object ABI v2 callbacks with per-function shared ABI v3
tables while retaining dynamic dispatch for every await.

**Files:**

- Modify async adapter emission in `src/c_emitter.rs`.
- Modify emitter unit and executable tests.
- Modify `tests/coroutine_contract.rs` to construct ABI v3 providers.

**Steps:**

1. Add failing output tests for one borrowed and one owning static vtable.
2. Change adapter poll callbacks to accept and forward poll context.
3. Make adapter error callbacks take `const void *`.
4. Generate a borrowed vtable whose drop finalizes without freeing.
5. Generate an owning vtable whose drop calls public destroy.
6. Move result size and alignment into both shared vtables.
7. Return only `{task, &vtable}` from both adapter constructors.
8. Remove `CR_AWAITABLE_OWNS_STATE` and every ABI v2 aggregate initializer.

**Focused gates:**

```powershell
cargo test --lib c_emitter
cargo test --test coroutine_contract
```

**Acceptance evidence:**

- Each adapter object is a state/vtable pair.
- Borrowed and owning adapters share callbacks where safe but use distinct
  drop-bearing vtables.
- Adapter ownership behavior remains executable.

## Task 4: Add opaque-slot validation and capability checks

This task centralizes validation so generated code never calls an unvalidated
vtable field.

**Files:**

- Refactor dynamic await emission in `src/c_emitter.rs`.
- Create `tests/abi_v3_protocol.rs`.
- Add malformed provider fixtures under `tests/fixtures/abi-v3/` if sharing C
  snippets keeps the Rust test readable.

**Steps:**

1. Write failing generated-C cases for null and short vtables.
2. Add helpers that validate only fields covered by the established prefix.
3. Validate mandatory poll and drop callbacks.
4. Validate result size and alignment once before active becomes true.
5. Reject unknown required capability bits with code 1107, including when the
   caller mirrors the unknown bit.
6. Reject missing known capabilities with code 1104 before every child poll.
7. Accept yielded status regardless of the advisory yield flag.
8. Reject unknown poll statuses with code 1106.
9. Copy a child error before cleanup and use code 1108 when details are absent.
10. Keep all fallback error messages in compiler-owned static storage.

**Focused gate:**

```powershell
cargo test --test abi_v3_protocol
```

**Acceptance evidence:**

- Every failure asserts sticky status, stable code, and exact drop count.
- Valid yield cases retain the active child without premature drop.
- No callback field is read outside its validated prefix.

## Task 5: Emit origin-aware task-binding lifetime

This task makes Stage 2 consume `ChildOrigin` for ownership while continuing to
use dynamic ABI v3 dispatch for both child origins.

**Files:**

- Modify planning metadata consumption in `src/c_emitter.rs`.
- Modify context-field and cleanup-helper emission in `src/c_emitter.rs`.
- Modify `src/liveness.rs` only if binding cleanup references need explicit
  persistent-field metadata.
- Modify `src/coroutine.rs` only if an emission lookup must be attached beside
  the existing await plan.
- Add executable task-binding cases to `tests/abi_v3_protocol.rs` and
  `tests/coroutine_contract.rs`.

**Emission model:**

Each declaration-owned child gets a compiler-private context slot containing
the two-word awaitable, active state, and a generation counter. Binding slots
exist independently of ordinary variable liveness because cleanup payloads can
outlive the declaration's immediate block execution.

Each dynamic cleanup payload records the parent context pointer, binding slot,
and captured generation. Its helper drops only when the slot is active and the
captured generation still matches. Reexecuting a declaration finalizes its
active previous generation, increments generation, activates the new child,
and registers a new cleanup payload. Older cleanup records become harmless and
can't drop a later generation.

**Steps:**

1. Add failing tests for repeated awaits of one ready binding.
2. Add a never-awaited binding scope-exit case.
3. Add error, cancellation, return, parent-drop, and nested-defer ordering
   cases.
4. Add declaration reexecution in a loop and prove exactly-once generation
   replacement.
5. Add a control-flow path that skips declaration activation and assert code
   1109 without polling uninitialized storage.
6. Map each await edge to its planned child instance and origin.
7. Keep direct children in edge-owned opaque slots and drop at terminal.
8. Poll binding awaits directly from the declaration-owned slot without
   copying or transferring ownership into an edge slot.
9. Leave bindings active after ready, error, or cancellation until lexical
   cleanup reaches them.
10. Drop an active edge-owned child before running the lexical cleanup stack.

**Focused gates:**

```powershell
cargo test --test abi_v3_protocol task_binding
cargo test --test coroutine_contract
cargo test --lib c_emitter
```

**Acceptance evidence:**

- Multiple awaits reference one binding generation.
- Binding cleanup stays in LIFO order with `__defer`.
- Stale cleanup records can't drop a newer generation.
- Direct and binding children each drop exactly once.

## Task 6: Replace baselines, templates, and generated callers

This task migrates repository-owned consumers and records the completed ABI v3
surface.

**Files:**

- Replace `tests/fixtures/planning/abi-v2-baseline.c` with an ABI v3 fixture and
  rename it accordingly.
- Modify `tests/coroutine_contract.rs` baseline lookup.
- Modify embedded templates under `templates/` or their actual source
  directory discovered by `src/template.rs`.
- Modify generated-project test drivers and examples.
- Modify manifest expectations only when the runtime ABI version is recorded.

**Steps:**

1. Regenerate and review the representative ABI v3 artifact.
2. Assert the new artifact byte-for-byte.
3. Pass `NULL` from every manual polling caller.
4. Update custom dynamic providers to vtable form.
5. Update runtime ABI version records in generated manifests.
6. Compile and run native, CMake, and Meson project fixtures.

**Focused gates:**

```powershell
cargo test --test coroutine_contract
cargo test --test generated_project
```

**Acceptance evidence:**

- Repository templates create ABI v3 projects without manual edits.
- The representative artifact contains no ABI v2 fields or flags.
- Native generated projects preserve observable output.

## Task 7: Run native and required WebAssembly conformance

This task proves that the ABI v3 core remains native-first and portable C11.

**Files:**

- Modify `tests/wasm_generated_project.rs` only when new layout fixtures need
  inclusion.
- Modify ABI v3 fixtures to fix target-specific failures without weakening
  the shared contract.

**Steps:**

1. Run native executable protocol and lifecycle tests with warnings denied.
2. Run CMake and Meson project gates.
3. Compile and link portable generated C for pinned `wasm32-wasi`.
4. Validate the linked module with pinned `wasm-tools`.
5. Run required mode so absent or mismatched tools fail.
6. Confirm computed goto remains rejected by the WebAssembly fixture.

**Focused gates:**

```powershell
cargo test --test generated_project
$env:CRC_REQUIRE_WASM='1'
cargo test --test wasm_generated_project
Remove-Item Env:CRC_REQUIRE_WASM
```

**Acceptance evidence:**

- Native execution remains the primary behavioral proof.
- Required-mode WebAssembly compiles, links, and validates ABI v3 code.
- Core v3 introduces no thread, atomic, TLS, or operating-system dependency.

## Task 8: Delete ABI v2 and run the final gate

This task removes temporary migration debris and proves that Stage 3 behavior
didn't leak into Stage 2.

**Files:**

- Delete the ABI v2 golden fixture.
- Update Stage 2 status in this plan and the architecture design.
- Modify implementation files only to fix failures found by the gate.

**Steps:**

1. Search source, tests, templates, and generated fixtures for ABI v2 version,
   per-object callback fields, ownership flags, and one-argument polls.
2. Delete dead v2 helpers and compatibility branches.
3. Confirm the C emitter still ignores embedded and boxed static dispatch.
4. Run formatting, checks, all targets, Clippy, and grammar tests.
5. Run native, CMake, Meson, and required-mode WebAssembly gates.
6. Record the final changed-file list and evidence.
7. Mark Stage 2 complete only when every named gate passes.

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
rg -n "ABI_VERSION 2|OWNS_STATE|\.poll\(|\.drop\(" src tests templates
rg -n "AwaitStorage::Embedded|AwaitStorage::Boxed" src/c_emitter.rs
```

The first search must find no ABI v2 production path. Callback invocations may
remain only behind validated vtable access. The second search may find explicit
Stage 2 ignoring or validation, but no static child code generation.

## Stop conditions

Stop and amend the design instead of broadening implementation when any of
these conditions occurs:

- A readable-prefix validation rule can't avoid an out-of-prefix field read.
- Origin-aware binding cleanup requires changing RFC0001 ordering.
- A public layout can't be expressed consistently for native and WebAssembly
  target data models.
- Preserving sticky terminal behavior conflicts with poll-context validation.
- A static dispatch shortcut is required to make ABI v3 pass.

## Next steps

Stage 2 is complete. Write and approve the Stage 3 static-await implementation
plan before enabling embedded, boxed, or cross-unit typed dispatch.
