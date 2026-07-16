# CR static await Stage 3 implementation plan

This plan enables typed static-await emission on top of core ABI v3. It adds a
target-specific C plan, two-phase layout emission, embedded same-unit children,
typed boxed recursion, and cross-translation-unit typed calls without changing
the public runtime or header ABI.

The requirements come from the approved
[Stage 3 design](../specs/2026-07-14-cr-static-await-stage-3-design.md), the
[coroutine architecture](../specs/2026-07-14-cr-coroutine-architecture-v3-design.md),
and [RFC0001](../../rfcs/0001-core-coroutine-contract.md).

> **Note:** This workspace isn't a Git repository. Each task ends with named
> gates and a live status update instead of a commit checkpoint.

## Outcome

At completion, every compiler-known async child uses a concrete typed task API.
Eligible same-unit children live directly in their parent context. Recursive,
policy-boxed, and cross-unit children use typed pointers. Only genuinely
dynamic awaits construct `cr_awaitable` values and use ABI v3 vtables.

Stage 3 must satisfy these invariants:

- Embedded child layouts are finite, complete, and acyclic.
- A layout move never changes a user async body's source environment.
- A static target never falls back to dynamic dispatch.
- Direct and binding ownership preserve RFC0001 cleanup order.
- Cross-unit task layouts remain opaque.
- Public ABI v3 headers remain byte-stable.
- Native remains the primary execution gate.
- Portable C11 continues to compile and validate for `wasm32-wasi`.
- Stage 4 CFG optimization remains disabled.

## Live status

Stage 3 is complete. Every planned static child uses typed dispatch, and the
ABI v3 vtable remains the dynamic boundary.

```text
Completed: Stages 0 through 2
Completed: Stage 3 design, user approval, and independent specification review
Completed: Task 1, structured static-callee metadata
Completed: Task 2, target-specific C static-await plan
Completed: Task 3, declaration environments and layout islands
Completed: Task 4, two-phase task layout and body emission
Completed: Task 5, embedded direct awaits
Completed: Task 6, embedded task bindings
Completed: Task 7, same-unit typed boxed children
Completed: Task 8, cross-unit typed boxed dispatch
Completed: Task 9, final dispatch matrix and dynamic ABI v3 regression
Completed: Task 10, final project, native, and WebAssembly gates
Completed: Stage 3
Pending: none
Blocked: none
```

## Task 1: Add structured static-callee metadata

This task gives later C planning declarator-aware signatures and canonical
typed symbol names without changing generated C.

**Files:**

- Modify `src/symbol_index.rs`.
- Modify `src/semantic.rs` only when HIR async calls need a stable signature
  reference beside `FunctionId`.
- Modify symbol-index and semantic unit tests.

**Steps:**

1. Add failing symbol-index tests for named, unnamed, pointer, array-adjusted,
   function-pointer, and `void` parameter lists.
2. Replace the planning dependence on raw parameter text with structured
   parameter descriptors while retaining canonical text for diagnostics.
3. Record the adjusted C parameter type and a declarator-safe temporary form.
4. Record external and translation-unit-qualified internal typed stems.
5. Preserve conflict detection for equivalent declarations and definitions.
6. Prove stable identities and signatures under input-order changes.
7. Confirm existing generated output remains byte-for-byte unchanged.

**Focused gates:**

```powershell
cargo test --lib symbol_index
cargo test --lib semantic
cargo test --test coroutine_contract
```

**Acceptance evidence:**

- Later passes never need to reparse a comma-separated signature.
- Internal-linkage typed names are deterministic and unit-qualified.
- Stage 2 dynamic emission remains unchanged.

## Task 2: Build the target-specific C static-await plan

This task introduces validated C planning records without enabling static
emission.

**Files:**

- Create `src/c_static_plan.rs`.
- Export the module from `src/lib.rs`.
- Modify `src/lib.rs` to make project liveness available before C emission.
- Modify `src/c_emitter.rs` only to accept a planning input that it initially
  ignores after validation.
- Add unit tests in `src/c_static_plan.rs`.

**Steps:**

1. Write failing tests for the final target and storage matrix.
2. Add `CChildPlan`, `CChildTarget`, `CChildStorage`, `CLayoutReason`, and
   `CFunctionPlan`.
3. Resolve every static `FunctionId` to typed task and API symbols.
4. Map every await edge to its `ChildInstanceId` without inspecting rendered
   C expressions.
5. Preserve requested storage separately from effective C storage.
6. Reject `Static + Opaque`, `Dynamic + Embedded`, and `Dynamic + Boxed`.
7. Require `Embedded` and `Boxed` for both supported static origins.
8. Require `Opaque` only for a direct dynamic origin.
9. Refactor standalone and project compilation so liveness completes before C
   planning and the symbol index remains available to emission.
10. Validate the plan, then continue to emit the Stage 2 dynamic path.

**Focused gates:**

```powershell
cargo test --lib c_static_plan
cargo test --lib await_plan
cargo test --test coroutine_contract
```

**Acceptance evidence:**

- Every child instance has one validated C-plan record.
- Requested and effective storage are independently inspectable.
- No static code-generation shortcut exists yet.

## Task 3: Index C declaration environments and plan layout islands

This task proves when task layouts and typed prototypes can move without
changing C declaration or preprocessing semantics.

**Files:**

- Create `src/c_declaration_env.rs`.
- Extend `src/c_static_plan.rs`.
- Reuse preprocessing-region facts from `src/preprocessor.rs` and
  `src/syntax.rs` where possible.
- Add focused unit fixtures under `tests/fixtures/static-await/planning/` when
  multiline C cases are clearer outside Rust strings.

**Steps:**

1. Write failing tests for file-scope typedef, tag, declaration, include,
   define, undef, and conditional-region visibility.
2. Index each relevant declaration and preprocessing boundary by source span
   and conditional ancestry.
3. Compute a legal layout anchor for every async task context after liveness.
4. Check complete-layout feasibility independently from typed-prototype
   feasibility.
5. Retain embedded storage only when the complete callee layout is legal
   before the parent layout.
6. Downgrade only an unsafe embedded edge to typed boxed and record a stable
   reason.
7. Emit a deterministic source diagnostic when no valid typed prototype can
   exist before a static call.
8. Reject a local-only type that must enter a file-scope coroutine context.
9. Revalidate the effective embedded graph after every downgrade.
10. Assign islands and topological layout order with stable source and identity
    keys.
11. Prove plans are independent of input enumeration and map order.

**Focused gates:**

```powershell
cargo test --lib c_declaration_env
cargo test --lib c_static_plan
```

**Acceptance evidence:**

- A safe later-defined child can move to an earlier layout island.
- A type, include, macro, or conditional barrier changes only the affected
  edge to boxed.
- Boxing never hides an invalid typed prototype.
- No user body moves.

## Task 4: Split task layout and body emission

This task implements two-phase translation-unit emission while every await
still uses the Stage 2 dynamic behavior.

**Files:**

- Refactor `src/c_emitter.rs`.
- Create `src/c_emitter/layout.rs`.
- Create `src/c_emitter/static_symbols.rs`.
- Modify C-emitter unit tests and the representative golden fixture.

**Steps:**

1. Write failing output tests for opaque forward declarations, typed
   prototypes, complete layouts, and original body positions.
2. Separate task struct emission from async init, poll, drop, create, destroy,
   accessor, and adapter bodies.
3. Emit one forward task identity and one complete definition per function.
4. Emit compiler-private layouts at their planned islands in topological
   order.
5. Emit external typed prototypes with the existing public stem.
6. Emit internal typed prototypes and bodies with unit-qualified `static`
   symbols.
7. Preserve every ordinary source byte and async body replacement position.
8. Preserve preprocessing ancestry at every island.
9. Keep all await terminators on the Stage 2 dynamic path.
10. Regenerate and review the ABI v3 representative golden.

**Focused gates:**

```powershell
cargo test --lib c_emitter
cargo test --test coroutine_contract
```

**Acceptance evidence:**

- Layout order no longer depends on async definition order.
- Macro and conditional fixtures compile with warnings denied.
- Generated behavior is unchanged before typed dispatch is enabled.

## Task 5: Enable embedded direct awaits

This task activates the highest-value allocation-free path for direct static
children.

**Files:**

- Create `src/c_emitter/static_await.rs`.
- Modify context-field and suspend emission in `src/c_emitter.rs`.
- Create `tests/static_await_codegen.rs`.

**Steps:**

1. Write a failing executable same-unit nonrecursive direct-await fixture.
2. Emit one concrete callee task field and active flag for the child instance.
3. Materialize child arguments once in established CR evaluation order.
4. Activate through direct typed `init` without allocation.
5. Poll through the concrete `poll` symbol and forward poll context.
6. Copy same-unit result, yielded value, and error before finalization.
7. Handle pending, yielded, ready, error, canceled, and invalid status.
8. Finalize direct children immediately at terminal status and on parent drop.
9. Keep loop reentry generation-safe without adding a cleanup record.
10. Add site-local output assertions that reject create, allocation,
    awaitable construction, vtable access, and indirect poll.

**Focused gate:**

```powershell
cargo test --test static_await_codegen embedded_direct
```

**Acceptance evidence:**

- The parent await site has no allocation or dynamic dispatch.
- All status and parent-drop paths finalize exactly once.
- Poll context reaches the embedded child unchanged.

## Task 6: Enable embedded task bindings

This task converts declaration-owned static children without changing their
lexical lifetime.

**Files:**

- Extend `src/c_emitter/static_await.rs`.
- Modify typed binding fields and cleanup helpers in `src/c_emitter.rs`.
- Extend `tests/static_await_codegen.rs`.

**Steps:**

1. Write failing fixtures for repeated await, never-awaited binding,
   reexecution, skipped activation, and defer LIFO.
2. Store one concrete child field, active flag, and generation per binding
   instance.
3. Make every binding await edge poll that same child field directly.
4. Keep ready, error, and canceled binding children alive until lexical
   cleanup.
5. Emit a typed generation-aware cleanup helper that calls embedded drop.
6. Finalize the previous active generation before declaration reexecution.
7. Preserve `CR_ERROR_INACTIVE_TASK_BINDING` for skipped activation.
8. On cleanup-push failure, immediately drop the new generation, clear active,
   run prior cleanups, and report the existing cleanup-allocation error.
9. Inject cleanup allocation failure and prove exactly-once finalization.
10. Add output assertions that the binding site contains no dynamic awaitable
    or vtable path.

**Focused gate:**

```powershell
cargo test --test static_await_codegen embedded_binding
```

**Acceptance evidence:**

- Multiple await edges reuse one typed child generation.
- Binding cleanup remains generation-safe and LIFO with defer.
- Cleanup-registration failure can't leak or double-drop the child.

## Task 7: Enable same-unit typed boxed children

This task activates finite typed-pointer storage for recursion and C-policy
downgrades.

**Files:**

- Extend `src/c_emitter/static_await.rs` and typed cleanup helpers.
- Extend `tests/static_await_codegen.rs`.
- Add recursive fixtures under `tests/fixtures/static-await/recursive/`.

**Steps:**

1. Write failing self-recursive and mutually recursive compilation fixtures.
2. Emit one typed callee pointer and active flag per boxed child instance.
3. Activate through direct typed create with an initialized allocation
   fallback error.
4. Reject a null create result without polling or destroying it.
5. Poll through the concrete same-unit symbol.
6. Copy same-unit terminal fields before typed destroy.
7. Use immediate destroy for direct origins and generation-aware lexical
   destroy for bindings.
8. Clear boxed pointers after every finalization path.
9. Inject create and cleanup-push allocation failures.
10. Prove recursive task contexts are finite and execute terminating base
    cases.
11. Assert the parent site has typed create, poll, and destroy calls but no
    awaitable construction, vtable access, or indirect poll.

**Focused gate:**

```powershell
cargo test --test static_await_codegen boxed_recursive
```

**Acceptance evidence:**

- Every cycle-closing edge uses a finite typed pointer.
- Direct and binding boxed children destroy exactly once.
- Allocation failure is sticky and null-safe.

## Task 8: Enable cross-unit typed boxed dispatch

This task uses the unchanged public opaque task API for known children in
another generated C file.

**Files:**

- Extend cross-unit resolution in `src/c_static_plan.rs`.
- Extend public-symbol emission in `src/c_emitter/static_symbols.rs`.
- Create `tests/static_await_project.rs`.
- Add a multi-file fixture under `tests/fixtures/static-await/cross-unit/`.

**Steps:**

1. Write a failing two-source project fixture with a generated `.hr` API.
2. Keep the cross-unit task type incomplete in the parent translation unit.
3. Reuse or synthesize compatible public typed declarations before first use.
4. Activate through public create and poll through public poll.
5. Copy ready, yielded, and error details through public accessors.
6. Destroy through the public allocating-module destroy function.
7. Cover direct and declaration-owned origins.
8. Add a signature-type visibility failure that produces the planned source
   diagnostic.
9. Prove two internal functions with the same source name in different units
   don't collide or resolve cross-unit.
10. Assert generated public headers are byte-stable.

**Focused gate:**

```powershell
cargo test --test static_await_project
```

**Acceptance evidence:**

- Separate generated C files compile and link through the existing ABI v3
  header surface.
- The parent contains no callee layout or vtable call.
- Allocation and destruction stay in the callee module.

## Task 9: Lock the final dispatch matrix and regress dynamic ABI v3

This task removes every temporary Stage 2 fallback for static targets and
proves that dynamic malformed-object behavior remains intact.

**Files:**

- Remove static-target dynamic fallback branches from `src/c_emitter.rs`.
- Update `tests/abi_v3_protocol.rs` only for non-behavioral fixture routing.
- Update `tests/coroutine_contract.rs` and the representative golden.
- Update generated-project fixtures when static output changes.

**Steps:**

1. Add a negative search for static child sites that construct adapters or
   access a vtable.
2. Require every final static C plan to emit embedded or boxed typed calls.
3. Retain the complete dynamic prefix, layout, capability, status, error, and
   drop validation path.
4. Re-run malformed dynamic protocol conformance without weakening assertions.
5. Re-run borrowed and owning public adapter tests.
6. Regenerate and manually review the representative golden.
7. Confirm public generated headers and `cr_runtime.h` are byte-stable.
8. Confirm Stage 4 slot reuse and CFG optimization remain absent.

**Focused gates:**

```powershell
cargo test --test abi_v3_layout
cargo test --test abi_v3_protocol
cargo test --test coroutine_contract
cargo test --test static_await_codegen
cargo test --test static_await_project
```

**Acceptance evidence:**

- Static sites contain no ABI v3 dynamic dispatch.
- Dynamic sites retain every Stage 2 safety check.
- The public ABI hasn't changed.

## Task 10: Run project, native, and WebAssembly final gates

This task proves Stage 3 is complete without starting Stage 4.

**Files:**

- Modify implementation files only to fix failures found by final gates.
- Update Stage 3 status in this plan and the architecture design.

**Steps:**

1. Run native executable lifecycle and performance-shape tests with warnings
   denied.
2. Run separate-unit, CMake, and Meson generated-project tests.
3. Run portable and computed-goto backend tests.
4. Run required-mode `wasm32-wasi` compilation, linking, and module validation.
5. Run formatting, all-target checks and tests, Clippy, and grammar tests.
6. Search generated output for illegal static vtable paths.
7. Search public headers and runtime declarations for ABI changes.
8. Record the final changed-file list and gate evidence.
9. Mark Stage 3 complete only when every named gate passes.

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
rg -n "AwaitTarget::Static" src/c_emitter.rs src/c_emitter
rg -n "vtable|into_awaitable|as_awaitable" `
  tests/fixtures/static-await tests/fixtures/planning/abi-v3-baseline.c
```

The first search must show plan-driven static branches, not a generic dynamic
fallback. The second search can find public adapter definitions and genuine
dynamic fixtures, but no embedded or boxed parent await site can use them.

**Acceptance evidence:**

- Native behavior and performance-shape gates pass.
- Required WebAssembly validation passes without target-specific core ABI.
- Public ABI v3 remains unchanged.
- No Stage 4 optimization appears in emission or planning.

### Completion evidence

The final Stage 3 gate completed on July 15, 2026, with this evidence:

- `cargo fmt --check` passed.
- `cargo check --all-targets` passed.
- `cargo test --all-targets` passed, including 82 library tests, 11 static
  await executable tests, 2 cross-unit static await tests, native generated
  projects, ABI v3 conformance, and WebAssembly validation.
- `cargo clippy --all-targets -- -D warnings` passed.
- `pnpm run grammar:test` passed all 4 Tree-sitter corpus parses.
- Required-mode `cargo test --test wasm_generated_project` passed with
  `CRC_REQUIRE_WASM=1`.
- Static-site searches found no adapter construction, vtable access, or
  indirect poll in embedded or boxed parent await sites.
- The representative ABI v3 golden remained byte-stable, and the public ABI
  layout and protocol tests passed.
- Source and documentation searches confirmed that Stage 4 slot reuse and CFG
  optimization remain disabled.

The Stage 3 implementation and conformance changes are contained in:

- `src/await_plan.rs`
- `src/c_declaration_env.rs`
- `src/c_emitter.rs`
- `src/c_static_plan.rs`
- `src/lib.rs`
- `src/semantic.rs`
- `src/symbol_index.rs`
- `tests/static_await_codegen.rs`
- `tests/static_await_project.rs`
- `tests/coroutine_contract.rs`
- `tests/fixtures/planning/abi-v3-baseline.c`
- `docs/superpowers/specs/2026-07-14-cr-static-await-stage-3-design.md`
- `docs/superpowers/plans/2026-07-14-cr-static-await-stage-3-implementation.md`

## Stop conditions

Stop and amend the design instead of broadening implementation when any of
these conditions occurs:

- A complete layout requires moving a user async body.
- A prototype or layout can't preserve C type or preprocessing identity.
- A static target requires dynamic fallback to compile.
- Cross-unit dispatch requires exposing task size or changing ABI v3.
- Typed lifetime handling conflicts with RFC0001 cleanup ordering.
- A recursive task context remains infinitely sized.
- Portable C11 or required WebAssembly needs a target-specific public branch.
- Static dispatch depends on enabling Stage 4 CFG optimization.

## Next steps

Treat Stage 4 as an optional, separately reviewed phase. Its highest-value
scope is ownership-aware coroutine CFG cleanup and noninterfering child/result
slot reuse. Stage 4 must preserve the completed Stage 3 dispatch matrix, ABI
v3, native-first performance, portable C11, and required WebAssembly support.
