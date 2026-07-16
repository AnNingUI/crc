# CR Waker and reference executor Stage 5 implementation plan

This plan implements the approved Stage 5 Waker contract and validates it with
manual, queued single-thread, and native cross-thread reference execution. It
keeps core ABI v3 compatible, preserves typed static await, and makes the
reference executor an explicit experimental project option.

The requirements come from the approved
[Stage 5 design](../specs/2026-07-15-cr-waker-reference-executor-stage-5-design.md),
the
[coroutine architecture](../specs/2026-07-14-cr-coroutine-architecture-v3-design.md),
the completed
[Stage 4 design](../specs/2026-07-15-cr-coroutine-cfg-stage-4-design.md),
and [RFC0001](../../rfcs/0001-core-coroutine-contract.md).

> **Note:** This workspace isn't a Git repository. Each task ends with named
> gates and a live status update instead of a commit checkpoint.

## Outcome

At completion, CR publishes a stable two-word Waker v1 extension, preserves
manual null-context polling, and provides opt-in experimental single-thread and
native cross-thread reference executors. The single-thread path remains
portable C11 and compiles for `wasm32-wasi`; the threaded path retains one poll
owner and permits wake and shutdown requests from producer threads.

Stage 5 must satisfy these invariants:

- `CR_RUNTIME_ABI_VERSION` remains `3`.
- `cr_runtime.h` and the `cr_poll_context` layout remain Stage 4 compatible.
- Existing generated code receives only the known `CR_POLL_CAP_WAKER` bit.
- `cr_waker` contains exactly state and one shared vtable pointer.
- Valid Waker clone is infallible and doesn't allocate.
- Wake never synchronously enters a task poll function.
- Duplicate, spurious, coalesced, and late wakes remain safe.
- Every effective cross-thread wake publishes readiness to the next poll.
- The task payload drops exactly once and can die before the control block.
- One owner thread performs every poll, cancel, shutdown, and payload drop.
- Manual projects don't compile or link reference executor sources by default.
- The single-thread executor contains no atomics or native thread dependency.
- Native threaded sources aren't emitted for `wasm32-wasi`.
- Static children remain typed and directly polled.
- Executor layout and API remain experimental.
- Reactor, EventSource, timer, socket, and backend SPI behavior remain absent.

## Project selection

Reference executor packaging is explicit. Stage 5 adds a runtime configuration
section with these serialized values:

```toml
[runtime]
executor = "manual"
```

The supported selections are:

- `manual`: Emit `cr_waker.h`, but emit no executor header or source.
- `single-thread`: Emit the public experimental executor header, common
  internals, the portable FIFO implementation, and a threaded-constructor stub.
- `native-threaded`: Emit the single-thread implementation plus the matching
  Windows or POSIX threaded backend.

`manual` is the default and preserves current generated-project linkage.
`native-threaded` is rejected for `wasm32-wasi` and unknown custom targets.

## Live status

Stages 0 through 4 are complete. The Stage 5 design passed incremental user
approval and three rounds of independent specification review.

```text
Completed: Stages 0 through 4
Completed: Stage 5 architecture choices and compatibility correction
Completed: Stage 5 design, independent review, and user approval
Completed: Task 1, RFC0002 Waker semantic contract
Completed: Task 2, explicit executor selection and target validation
Completed: Task 3, Waker v1 layout and stable extension artifact
Completed: Task 4, header-only Waker helpers and protocol tests
Completed: Task 5, deterministic lost-wakeup registration protocol
Completed: Task 6, portable single-thread reference executor
Completed: Task 7, cancellation, ticket, and shutdown lifetime
Completed: Task 8, native cross-thread reference backend
Completed: Task 9, compiler poll-context forwarding and static dispatch
Completed: Task 10, generated-project and build-system integration
Completed: Task 11, portable generated-project WebAssembly validation
Completed: Task 12, final Stage 5 conformance and boundary audit
Completed: Stage 5
In progress: none
Pending: none
Blocked: none
```

## Task 1: Write RFC0002 and freeze the semantic contract

This task turns the approved design into the stable Waker RFC before any public
extension declaration is implemented.

**Files:**

- Create `docs/rfcs/0002-waker-contract.md`.
- Update `docs/rfcs/0001-core-coroutine-contract.md` to link the approved Waker
  extension without changing core lifecycle semantics.
- Update the Stage 5 plan live status.

**Steps:**

1. Define Waker validity, append-only version acceptance, clone, wake, and drop.
2. Define owner-thread and cross-thread callback rules.
3. Define duplicate, spurious, coalesced, and late wake behavior.
4. Define the happens-before rule for every cross-thread wake, including a wake
   that doesn't add a second queue record.
5. Define the awaitable-owned registration and readiness-recheck protocol.
6. Define task-payload, control-block, ticket, queue, and Waker ownership.
7. Define owner-thread cancellation and nonblocking shutdown requests.
8. Classify Waker v1 as stable and reference executor APIs as experimental.
9. State that core ABI v3, synchronous drop, and static dispatch remain intact.
10. State that reactor and backend SPI work remains Stage 6.
11. Search the RFC for placeholders and conflicting lifecycle terms.
12. Record the approved RFC path in this plan.

**Focused gate:**

```powershell
if (rg -n "TODO|TBD|placeholder" docs/rfcs/0002-waker-contract.md) {
    throw 'RFC0002 contains a placeholder'
}
rg -n "Waker|executor|reactor" `
  docs/rfcs/0001-core-coroutine-contract.md `
  docs/rfcs/0002-waker-contract.md
```

The placeholder guard must complete without throwing.

**Acceptance evidence:**

- RFC0002 records observable semantics instead of an internal state machine.
- RFC0001 core behavior remains unchanged and links the new stable extension.
- Executor and backend stability boundaries are explicit.

### Task 1 completion evidence

Task 1 completed on July 15, 2026, with this evidence:

- `docs/rfcs/0002-waker-contract.md` defines the accepted stable Waker v1
  semantic and append-only extension ABI contract.
- Waker validity requires non-null state, a complete v1 prefix, and clone,
  wake, and drop callbacks.
- Valid clone is infallible and allocation-free. Every clone has one matching
  drop, and wake doesn't consume the handle.
- The cross-thread flag covers clone, wake, and drop on distinct handles.
- Every cross-thread wake publishes readiness before coalescing, including a
  wake that doesn't add another queue record.
- Awaitables own readiness, registration, Waker replacement, and the final
  readiness recheck.
- Task payload, scheduling control block, ticket, queue, original Waker, and
  retained clone ownership remain explicitly separate.
- Cancellation and terminal completion keep hard drop synchronous and make
  every late wake a safe no-op.
- Producer threads can request shutdown, but only the poll owner cancels tasks,
  drops payloads, and destroys the reference executor.
- Core ABI v3 retains its existing poll-context layout and uses only
  `CR_POLL_CAP_WAKER`; cross-thread support adds no unknown core capability bit.
- `docs/rfcs/0001-core-coroutine-contract.md` links RFC0002 without changing
  core lifecycle, cleanup, cancellation, or adapter ownership semantics.
- Reference executor API and layout remain experimental, and reactor and
  backend SPI work remains Stage 6.
- Placeholder, 80-column, link, version, lifecycle, lost-wakeup, and Stage 6
  boundary audits pass.
- This task modifies documentation only. No runtime, compiler, generated C, or
  public header implementation changed.

## Task 2: Add explicit executor selection without changing output

This task introduces configuration and pipeline selection before emitting a
Waker header or executor source.

**Files:**

- Modify `src/config/mod.rs`.
- Modify `src/template/templates/crc.toml`.
- Modify configuration and compiler pipeline tests.
- Modify `tests/generated_project.rs` only for configuration assertions.

**Steps:**

1. Add a `RuntimeConfig` with an `ExecutorSelection` value.
2. Serialize the values as `manual`, `single-thread`, and `native-threaded`.
3. Make `manual` the serde and programmatic default.
4. Add the explicit default to newly created `crc.toml` projects.
5. Thread the selection into project artifact collection without using it yet.
6. Reject `native-threaded` with `wasm32-wasi` and unknown custom targets.
7. Keep `single-thread` legal for every reviewed native target and WASI.
8. Prove every selection round-trips through TOML.
9. Prove the default artifact set and generated C remain byte-identical in this
   task.
10. Prove `crc check` validates the selection without writing artifacts.

**Focused gates:**

```powershell
cargo test --lib config
cargo test --lib tests
cargo test --test generated_project
```

**Acceptance evidence:**

- Existing projects without `[runtime]` select `manual`.
- No executor artifact is emitted before its implementation task.
- Unsupported threaded target selection fails before publication.

### Task 2 completion evidence

Task 2 completed on July 15, 2026, with this evidence:

- `Config` now contains a serde-defaulted `RuntimeConfig`.
- `ExecutorSelection` serializes as `manual`, `single-thread`, and
  `native-threaded`.
- Missing and empty runtime configuration defaults to `manual`.
- Newly created projects write an explicit `[runtime]` section with
  `executor = "manual"`.
- Manual and single-thread selections validate for every reviewed native
  target, `wasm32-wasi`, and custom portable targets.
- Native-threaded validates for host, Windows MSVC, Windows GNU, Linux GNU,
  Linux musl, and macOS targets.
- Native-threaded rejects `wasm32-wasi` and unknown custom targets before
  artifact collection reads or publishes generated output.
- `Compiler::collect_project_artifacts` performs target validation before any
  artifact planning or publication.
- A successful WASI single-thread `crc check` writes no artifacts and preserves
  the previous published source and manifest.
- A rejected WASI native-threaded `crc check` reports both selections and
  preserves the previous published source and manifest.
- Manual, single-thread, and native-threaded selections produce byte-identical
  single-source generated C because Task 2 doesn't connect the selection to
  emission.
- Default project builds contain no executor header or runtime source.
- Native direct C, CMake, Meson, ABI v3, static-await, CFG optimization,
  context-layout, ownership, and target-layout regressions pass.
- Required-mode WASI compilation, linking, and validation pass with
  `CRC_REQUIRE_WASM` restored to its original unset state.
- `cargo test --all-targets` passes with 136 library tests and every integration
  test.
- `pnpm run grammar:test` passes all four corpus parses.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 3: Publish the Waker v1 layout and extension artifact

This task adds the stable extension header and proves its native and WebAssembly
layout without implementing executor behavior.

**Files:**

- Create `src/waker_abi.rs`.
- Export the module from `src/lib.rs`.
- Modify project artifact collection in `src/lib.rs`.
- Create `tests/waker_v1_layout.rs`.
- Update `tests/generated_project.rs` artifact assertions.
- Update the artifact manifest tests.

**Steps:**

1. Add failing C layout assertions for a two-word `cr_waker`.
2. Define `cr_waker_vtable` with version, size, flags, clone, wake, and drop.
3. Define the v1 minimum-prefix macro through `drop_state`.
4. Define `CR_WAKER_FLAG_CROSS_THREAD` in the extension header only.
5. Define error codes 1110 and 1111 in the extension header.
6. Keep `cr_runtime.h` byte-identical to the completed Stage 4 header.
7. Accept append-only `abi_version >= 1` when the v1 prefix is complete.
8. Emit `include/cr_waker.h` for every project selection.
9. Record the new stable-extension artifact in the publication manifest.
10. Compile the header with native Clang and GCC warnings denied.
11. Compile the same layout assertions with pinned WASI Clang.
12. Prove `cr_poll_context` offsets and size remain unchanged.

**Focused gates:**

```powershell
cargo test --test waker_v1_layout
cargo test --test generated_project `
  successful_build_removes_stale_artifacts_and_records_a_manifest
```

**Acceptance evidence:**

- Waker v1 is exactly two pointer widths on native and `wasm32-wasi`.
- Future append-only versions retain the v1 prefix.
- Core runtime ABI v3 declarations remain byte-stable.

### Task 3 completion evidence

Task 3 completed on July 15, 2026, with this evidence:

- `src/waker_abi.rs` owns the stable portable C11 Waker extension header.
- `cr_waker` contains only state and a shared `const cr_waker_vtable *`.
- The vtable contains version, structure size, provided flags, clone, wake, and
  drop callbacks in the approved v1 order.
- `CR_WAKER_VTABLE_V1_MIN_SIZE` ends exactly after `drop_state`.
- Native and WASI tests correctly permit target-specific trailing structure
  padding; the minimum prefix must fit the complete structure but doesn't need
  to equal `sizeof(cr_waker_vtable)`.
- `CR_WAKER_FLAG_CROSS_THREAD` and error codes 1110 and 1111 exist only in the
  extension header.
- Task 3 deliberately emits no clone, wake, drop helper, executor declaration,
  queue, atomic, or thread API.
- Every project selection now publishes `include/cr_waker.h` after
  `include/cr_runtime.h`.
- The artifact manifest records the Waker header as `runtime-extension`.
- Manual projects still emit no executor header or runtime source.
- Native Clang and GCC compile and execute the same Waker layout fixture with
  C11 warnings denied.
- Pinned WASI Clang compiles the same two-word handle, prefix, future append,
  poll-context, flag, and error-code assertions.
- The required generated WASI project publishes `cr_waker.h` and compiles its
  layout assertions from the actual project include directory.
- A future vtable structure retains the v1 prefix at offset zero and appends
  fields after the complete v1 structure.
- `cr_poll_context` retains the Stage 4 field order and minimum-prefix formula.
- `cr_runtime.h` remains exactly 4,472 bytes with the Stage 4 FNV-1a 64-bit
  digest `0x70dc916f2d8ee4f0`.
- Native direct C, CMake, Meson, ABI v3, static-await, CFG optimization,
  context-layout, ownership, and target-layout regressions pass.
- Required-mode Waker and generated-project WASI gates pass with
  `CRC_REQUIRE_WASM` restored to its original unset state.
- `cargo test --all-targets` passes with 137 library tests and every integration
  test.
- `pnpm run grammar:test` passes all four corpus parses.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 4: Implement header-only Waker helpers and protocol tests

This task implements validation and ownership helpers without adding an
executor or a production awaitable.

**Files:**

- Extend `src/waker_abi.rs`.
- Create `tests/waker_v1_protocol.rs`.
- Add focused C fixtures under `tests/fixtures/waker/` when useful.

**Steps:**

1. Add failing tests for null handles, null state, and null vtables.
2. Test version zero, truncated prefixes, and each missing callback.
3. Implement `cr_waker_is_valid` as portable `static inline` C11.
4. Implement clone with a null output on every failure.
5. Require a non-null state from a valid clone provider.
6. Implement non-consuming wake-by-reference.
7. Implement drop and clear the handle after the callback.
8. Count original, clone, wake, and drop operations in executable C.
9. Accept unknown provided flags without weakening known flag checks.
10. Prove no helper requires a linkable runtime symbol.
11. Prove clone performs no allocation in the reference provider fixture.
12. Keep same-handle concurrent access outside the caller contract while
    permitting distinct cross-thread clones.

**Focused gate:**

```powershell
cargo test --test waker_v1_protocol
```

**Acceptance evidence:**

- Every successful clone owns exactly one reference.
- Every owned handle drops exactly once.
- Invalid and protocol-violating providers fail without invoking wake.

### Task 4 completion evidence

Task 4 completed on July 15, 2026, with this evidence:

- `cr_waker_is_valid` rejects null handles, null state, null vtables, version
  zero, a truncated v1 prefix, and each missing v1 callback.
- Structurally complete future versions and unknown provided flags remain
  valid. Consumers can still test `CR_WAKER_FLAG_CROSS_THREAD` independently.
- `cr_waker_clone` clears its output before every structural or provider
  failure and rejects a clone provider that returns null state.
- The allocation-free reference provider increments an existing state
  reference during clone and performs no allocation.
- Each successful clone owns one reference, and the executable fixture matches
  every owned original or clone with one drop callback.
- `cr_waker_wake` is non-consuming. Duplicate wake calls leave the handle and
  its reference ownership unchanged.
- `cr_waker_drop` calls the provider once for a valid owned handle, clears both
  handle words, and makes a repeated drop a no-op.
- Invalid Wakers don't invoke clone, wake, or drop callbacks. A structurally
  valid provider that returns null from clone records only the failed clone
  call and receives no wake.
- The helpers are portable `static inline` C11 definitions in `cr_waker.h`.
  Clang and GCC compile, link, and execute the protocol fixture with inlining
  disabled and without another runtime object or library.
- Pinned WASI Clang compiles the same helper bodies and complete protocol
  fixture for `wasm32-wasi` with warnings denied.
- `cr_runtime.h` remains exactly 4,472 bytes with the Stage 4 FNV-1a 64-bit
  digest `0x70dc916f2d8ee4f0`.
- Native direct C, CMake, Meson, ABI v3, static-await, CFG optimization,
  context-layout, ownership, and target-layout regressions pass.
- Required-mode Waker layout, Waker protocol, and generated-project WASI gates
  pass with `CRC_REQUIRE_WASM` restored to its original unset state.
- `cargo test --all-targets` passes with 137 library tests and every integration
  test, including the two new Waker protocol tests.
- `pnpm run grammar:test` passes all four corpus parses.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 5: Prove the lost-wakeup registration protocol

This task builds a deterministic controllable event awaitable in the test suite
before implementing a queue or executor.

**Files:**

- Create `tests/waker_registration.rs`.
- Create test-only event sources under `tests/fixtures/waker/`.
- Modify Waker fixtures only when deterministic hooks require shared support.

**Steps:**

1. Implement a test-only event awaitable that requires
   `CR_POLL_CAP_WAKER`.
2. Keep readiness and Waker registration inside the event state.
3. Clone and publish the new Waker before dropping the old registration.
4. Recheck readiness after publication and before returning `Pending`.
5. Test readiness before registration.
6. Test readiness during Waker publication with deterministic hooks.
7. Test readiness after `Pending`.
8. Test replacement and exactly-once drop of the old registered Waker.
9. Test duplicate and spurious wake.
10. Test wake during poll and prove it doesn't reenter the poll callback.
11. Test missing Waker capability, invalid Waker ABI, and clone failure errors.
12. Use barriers and explicit hooks instead of timing sleeps.

**Focused gate:**

```powershell
cargo test --test waker_registration
```

**Acceptance evidence:**

- Every registration boundary has a deterministic executable test.
- Returning `Pending` always retains a valid registration.
- Waker and executor code remain independent of readiness semantics.

### Task 5 completion evidence

Task 5 completed on July 15, 2026, with this evidence:

- `tests/fixtures/waker/registration.cr` implements a test-only event
  awaitable that requires `CR_POLL_CAP_WAKER`.
- The event owns its readiness bit, retained Waker registration, provider
  error, and deterministic publication hooks. No Waker or executor helper
  interprets event readiness.
- Every unready poll clones the incoming Waker, publishes the new clone, drops
  the previous registration, and rechecks readiness before returning
  `CR_POLL_PENDING`.
- Executable cases place readiness before the first poll, after the first
  readiness check but before Waker publication, immediately after publication,
  and after a returned `Pending`.
- A readiness event before publication produces no wake but is observed by the
  required second readiness check.
- A readiness event after publication wakes the retained clone and completes
  in the same poll without synchronously reentering the poll callback.
- Every `Pending` assertion also proves that the event retains a structurally
  valid owned Waker clone.
- The old provider's drop callback observes the replacement Waker already
  published, then records exactly one old-registration drop.
- Duplicate and spurious wake calls don't alter readiness ownership, consume a
  handle, or increase the maximum poll depth above one.
- Missing `CR_POLL_CAP_WAKER` reports
  `CR_ERROR_MISSING_POLL_CAPABILITY` before entering the event poll callback.
- A malformed Waker reports `CR_ERROR_INVALID_WAKER_ABI`, and a valid provider
  that returns null from clone reports `CR_ERROR_WAKER_CLONE_FAILED`.
- Every terminal and error path drops the dynamic event operation once and
  releases any retained registration once.
- The native fixture uses deterministic hooks and callback assertions. It
  contains no timing sleep, executor, queue, reactor, atomic, or thread API.
- Clang and GCC compile, link, and execute the generated C with C11 warnings
  denied and inlining disabled.
- Pinned WASI Clang compiles the same generated registration protocol for
  `wasm32-wasi` with target-aware context layout.
- This task changes only test source and integration coverage. Stable Waker v1,
  core ABI v3, compiler lowering, static await, synchronous drop, and ownership
  behavior remain unchanged.
- `cr_runtime.h` remains exactly 4,472 bytes with the Stage 4 FNV-1a 64-bit
  digest `0x70dc916f2d8ee4f0`.
- Required-mode registration, Waker layout, Waker protocol, and generated WASI
  gates pass with `CRC_REQUIRE_WASM` restored to its original unset state.
- `cargo test --all-targets` passes with 137 library tests and every integration
  test, including the two new registration protocol tests.
- `pnpm run grammar:test` passes all four corpus parses.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 6: Package the experimental executor and single-thread FIFO

This task introduces the first executor behavior and the project artifacts
selected by `runtime.executor = "single-thread"`.

**Files:**

- Create `src/executor_runtime.rs`.
- Export the module from `src/lib.rs`.
- Modify project artifact collection and Meson source manifests in `src/lib.rs`.
- Create `tests/reference_executor.rs`.
- Update `tests/generated_project.rs`.

**Generated artifacts:**

- `include/cr_executor.h`.
- `runtime/cr_executor_internal.h`.
- `runtime/cr_executor_common.c`.
- `runtime/cr_executor_single.c`.
- `runtime/cr_executor_threaded_stub.c`.

**Steps:**

1. Emit no executor artifact for `manual`.
2. Emit the complete portable source set for `single-thread`.
3. Define opaque executor and ticket declarations in the public header.
4. Implement single-thread creation, spawn, run-ready, cancel, shutdown, and
   destroy.
5. Validate the root awaitable before move consumption.
6. Allocate an overflow-checked aligned result buffer for non-void roots.
7. Move and clear the source awaitable only after successful setup.
8. Use an intrusive FIFO record so wake performs no allocation.
9. Coalesce duplicate wake through an ordinary queued flag.
10. Handle `Pending`, `Yielded`, `Ready`, `Error`, and `Canceled` exactly as the
    design specifies.
11. Run observers on the poll owner and document their pointer lifetimes.
12. Implement the portable unsupported threaded-constructor stub.
13. Assert the single-thread sources contain no atomics, pthread, or Win32
    synchronization API.
14. Compile and execute with both Clang and GCC.

**Focused gates:**

```powershell
cargo test --test reference_executor single_thread
cargo test --test generated_project
```

**Acceptance evidence:**

- Manual artifact output doesn't include executor files.
- Single-thread wake is constant-time and allocation-free.
- FIFO order, coalescing, yield requeue, and terminal notification pass.
- CMake and Meson compile the selected runtime sources once.

### Task 6 completion evidence

Task 6 completed on July 15, 2026, with this evidence:

- `src/executor_runtime.rs` owns the complete portable experimental executor
  artifact set.
- The public `cr_executor.h` keeps executor and ticket layouts opaque and
  declares create, spawn, run, wait, shutdown request, cancel, ticket release,
  shutdown, and destroy operations.
- The public header documents that observer value and error pointers remain
  valid only during the matching owner-thread callback.
- Manual projects continue to publish no executor header, internal header, or
  source file.
- Single-thread projects publish `cr_executor.h`, the internal header, common
  runtime source, FIFO implementation, and portable threaded-constructor stub.
- Until Task 8, native-threaded projects receive the same portable base and
  unsupported constructor stub. They don't claim native threaded execution.
- Spawn validates the complete root awaitable ABI, required capabilities,
  callbacks, result size, power-of-two alignment, and allocation overflow
  before consuming the source handle.
- Failed validation leaves the source awaitable byte ownership unchanged and
  clears the output ticket.
- Successful setup allocates the control block and aligned result storage,
  then moves and clears the source awaitable.
- Zero-sized roots use a null result pointer. Over-aligned roots receive a
  correctly aligned observer pointer backed by overflow-checked storage.
- The task control block embeds its FIFO link. Wake checks one ordinary queued
  flag and appends in constant time without allocation or atomic operations.
- Duplicate wake coalesces into one ready record, and wake during poll never
  calls poll recursively.
- FIFO execution handles `Pending`, `Yielded`, `Ready`, `Error`, `Canceled`,
  and invalid status values. Yielded tasks notify and requeue at the tail.
- Terminal notification runs before synchronous payload drop. Each tested
  payload drops once.
- Basic cancel is idempotent, requested shutdown is observed by
  `cr_executor_run_ready`, and destroy performs synchronous shutdown when
  required.
- Control-block, payload, queue-record, ticket, original-Waker, and shared
  executor storage are represented separately, ready for Task 7 lifetime
  conformance.
- The portable threaded constructor returns null with
  `CR_EXECUTOR_ERROR_UNSUPPORTED`, and `cr_executor_wait_one` doesn't block.
- Portable artifact tests reject C atomics, pthread APIs, Win32 thread APIs,
  and native condition-variable dependencies.
- The generated Meson source list names each runtime source exactly once.
  CMake's recursive generated-source collection also compiles each source once.
- A selected single-thread project compiles and runs through direct native C,
  CMake, and Meson builds.
- Clang and GCC compile and execute FIFO, coalescing, yield, aligned result,
  move ownership, terminal status, cancel, shutdown, and stub conformance with
  warnings denied and inlining disabled.
- Pinned WASI Clang links the same executor conformance fixture, and pinned
  `wasm-tools` validates the module.
- The required generated WASI project now selects `single-thread`, publishes
  the real runtime artifacts, links every recursive runtime source, and passes
  module validation.
- Stable Waker v1, core ABI v3, static await, CFG optimization, synchronous
  drop, and compiler-generated task layouts remain unchanged.
- `cr_runtime.h` remains exactly 4,472 bytes with the Stage 4 FNV-1a 64-bit
  digest `0x70dc916f2d8ee4f0`.
- Required-mode executor, registration, Waker layout, Waker protocol, and
  generated-project WASI gates pass with `CRC_REQUIRE_WASM` restored to its
  original unset state.
- `cargo test --all-targets` passes with 139 library tests and every integration
  test.
- `pnpm run grammar:test` passes all four corpus parses.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 7: Complete cancellation, ticket, and shutdown lifetime

This task proves that task payload storage can die before every external Waker
or ticket without creating a use-after-free.

**Files:**

- Extend `src/executor_runtime.rs`.
- Extend `tests/reference_executor.rs`.
- Add allocation-failure and lifecycle fixtures under `tests/fixtures/waker/`.

**Steps:**

1. Track active-task, ticket, queue, original-Waker, and cloned-Waker
   references independently.
2. Let the caller release a ticket before terminal completion without canceling
   the task.
3. Make owner-thread cancel idempotent for a valid retained ticket.
4. Notify `Canceled` exactly once on explicit cancel and shutdown.
5. Drop the awaitable payload exactly once on every terminal path.
6. Release the executor-owned original Waker at terminal, cancel, or shutdown.
7. Retain the control block while any ticket, queue record, or clone remains.
8. Make queued records safe to skip after cancel.
9. Make late wake after cancel, terminal, and destroy a no-op.
10. Make destroy perform owner-thread shutdown when needed.
11. Test failed control-block and result-buffer allocation without consuming
    the source awaitable.
12. Test the documented ephemeral value and error pointer lifetime.
13. Test shutdown with active, queued, pending, yielded, and terminal tasks.
14. Prove every cleanup and adapter drop executes exactly once.

**Focused gate:**

```powershell
cargo test --test reference_executor lifecycle
```

**Acceptance evidence:**

- Payload and control-block lifetimes are structurally separate.
- Ticket release, cancel, terminal, shutdown, and late wake are leak-free.
- No cancellation path suspends or schedules asynchronous cleanup.

### Task 7 completion evidence

Task 7 completed on July 15, 2026, with this evidence:

- Spawn now acquires the active-task, caller-ticket, and executor-owned Waker
  references through three explicit operations instead of one aggregate count.
- Every effective queue insertion and retained Waker clone acquires its own
  control-block reference, and every dequeue, Waker drop, or terminal edge
  releases the matching source.
- The executor's public handle owns shared state independently from each task
  control block. A surviving control block keeps shared state alive after the
  public executor is destroyed.
- Internal allocator macros default to standard `malloc`, `calloc`, and `free`.
  They add no public API or default runtime dependency.
- The lifecycle fixture overrides those macros with a static allocation ledger
  that records every live executor, shared-state, task, and result allocation.
- Deterministic failure of the result-buffer allocation leaves the root
  awaitable unconsumed, clears the output ticket, invokes no drop, and leaks no
  allocation.
- Deterministic failure of the task control-block allocation frees the already
  allocated result buffer while preserving the source awaitable unchanged.
- A caller can release its ticket before terminal completion. Active-task and
  executor-owned Waker references keep polling safe, and the control block
  releases automatically after terminal completion.
- Explicit cancel is idempotent for a retained ticket, reports `Canceled` once,
  and synchronously drops the root payload once.
- A canceled task can leave an intrusive queue record behind. The queue
  reference keeps the control block alive until `run_ready` safely skips and
  releases the inactive record without entering poll.
- Terminal completion and cancel both release the executor-owned Waker while
  retained external clones continue to own only the control block and shared
  state.
- Late duplicate wake after terminal completion, cancel, and public executor
  destruction performs no allocation, queue access, task poll, payload access,
  or resurrection.
- Dropping the final external Waker clone releases the task control block and
  shared state after the payload and public executor have already died.
- Destroy performs owner-thread shutdown when active work remains. It reports
  `Canceled`, drops the payload, drains queued records, and lets a retained
  ticket release the final control block later.
- Requested shutdown covers a previously terminal task, a dormant pending
  task, a yielded task, and an unpolled queued task in one deterministic run.
- The yielded task reports exactly one `Yielded` notification followed by one
  `Canceled` notification. Every other active task reports `Canceled` once.
- Every terminal, explicit-cancel, requested-shutdown, and destroy path invokes
  the root adapter drop exactly once and never polls during cancellation.
- Observer value data is copied before its aligned result allocation is freed.
  The allocation ledger proves the borrowed value pointer is ephemeral.
- Observer error data is copied before payload drop mutates the provider error,
  proving the borrowed error pointer can't be retained as stable state.
- `tests/fixtures/waker/executor_lifecycle.c` contains no sleeps, asynchronous
  cleanup, native synchronization, or thread API.
- Clang and GCC compile and execute the allocation and lifecycle fixture with
  warnings denied and inlining disabled. Every test ends with zero live ledger
  allocations.
- Pinned WASI Clang links the same lifecycle fixture with allocator overrides,
  and pinned `wasm-tools` validates the resulting module.
- Stable Waker v1, public experimental executor API, artifact selection, core
  ABI v3, static await, synchronous drop, and generated task layout remain
  unchanged.
- `cr_runtime.h` remains exactly 4,472 bytes with the Stage 4 FNV-1a 64-bit
  digest `0x70dc916f2d8ee4f0`.
- Required-mode lifecycle, executor, registration, Waker layout, Waker
  protocol, and generated-project WASI gates pass with `CRC_REQUIRE_WASM`
  restored to its original unset state.
- `cargo test --all-targets` passes with 139 library tests and every integration
  test, including four reference executor tests.
- `pnpm run grammar:test` passes all four corpus parses.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 8: Add the native cross-thread reference backend

This task implements optional cross-thread wake while preserving one poll
owner and keeping native synchronization out of the portable source set.

**Files:**

- Extend `src/executor_runtime.rs`.
- Add generated Windows and POSIX backend sources.
- Create `tests/reference_executor_threaded.rs`.
- Update target-selection and artifact-manifest tests.

**Generated artifacts:**

- Windows: `runtime/cr_executor_threaded_windows.c`.
- POSIX: `runtime/cr_executor_threaded_posix.c`.
- Unsupported targets continue to use
  `runtime/cr_executor_threaded_stub.c`.

**Steps:**

1. Establish the creating thread as the only poll owner.
2. Use atomic control-block references only in the threaded backend.
3. Protect ready-queue mutation and coalescing with a mutex.
4. Use a condition variable for `cr_executor_wait_one`.
5. Permit producer threads to call Waker clone, wake, drop, and shutdown
   request on distinct handles.
6. Publish readiness before queue coalescing on every wake.
7. Acquire every publication before the next relevant owner poll.
8. Make shutdown request nonblocking and wake the blocked owner.
9. Make the owner synchronously cancel and drop before wait returns `false`.
10. Prevent run, wait, shutdown, and destroy from running off-owner.
11. Keep the shared queue state alive across public executor destruction while
    retained Waker clones exist.
12. Race registration, duplicate wake, cancel, terminal, and shutdown with
    deterministic barriers.
13. Prove one owner thread performs every poll callback.
14. Compile the Windows implementation on Windows and the POSIX implementation
    on Linux and macOS CI.

**Focused gate:**

```powershell
cargo test --test reference_executor_threaded
```

**Acceptance evidence:**

- Cross-thread wake provides the required visibility without concurrent poll.
- Coalesced wakes don't skip publication.
- A blocked owner exits cleanly after a producer shutdown request.
- Portable single-thread artifacts remain free of native synchronization.

### Task 8 completion evidence

Task 8 completed on July 15, 2026, with this evidence:

- The common executor now dispatches reference ownership, active-list changes,
  ready-queue operations, shutdown state, owner checks, and blocking waits
  through an internal backend operations table.
- Poll result handling, observer ordering, payload drop, cancellation, aligned
  result storage, and public API semantics remain in the shared common layer.
- The single-thread backend implements the same operations with ordinary
  references and fields. It retains its allocation-free intrusive FIFO and
  imports no atomic, thread, mutex, condition-variable, or platform API.
- The Windows backend stores the creating thread ID as poll owner, uses
  `Interlocked` reference operations, protects queue and lifecycle state with a
  critical section, and blocks with a condition variable.
- The POSIX backend stores `pthread_self()` as poll owner, uses compiler atomic
  reference operations, protects queue and lifecycle state with a pthread
  mutex, and blocks with a pthread condition variable.
- Native threaded Wakers advertise `CR_WAKER_FLAG_CROSS_THREAD`. Portable
  single-thread Wakers continue to advertise no cross-thread capability.
- Every threaded wake acquires the queue mutex after producer readiness writes.
  The owner acquires the same mutex before consuming the next ready record.
- Duplicate wake still enters and leaves the mutex before coalescing, so a
  readiness publication isn't lost when no second queue record is added.
- A deterministic test wakes twice while the owner is inside poll. Poll never
  reenters, the first poll returns `Pending`, and the next owner poll observes
  the producer's published value.
- Producer threads clone, wake, and drop distinct Waker handles safely. The
  original owner handle remains separately owned.
- Spawn, run, wait, cancel, ticket release, shutdown, and destroy reject
  off-owner calls without polling, consuming a source awaitable, canceling a
  task, releasing a ticket, or destroying the public executor.
- Poll callbacks, observers, cancellation, payload drop, and public destroy all
  execute on the creating owner thread.
- `cr_executor_request_shutdown` is nonblocking and cross-thread safe. It marks
  shared state closing and broadcasts the native condition variable.
- A default-empty internal wait hook lets tests prove that the owner reached
  the blocking condition wait before the producer requested shutdown. The
  request releases the owner, which synchronously reports `Canceled`, drops the
  payload, completes shutdown, and returns `false` from `wait_one`.
- A producer-held Waker clone keeps the task control block, shared queue state,
  native mutex, and condition variable alive after terminal payload drop,
  ticket release, and public executor destruction.
- Wake and drop from that final producer clone remain safe after public destroy,
  and dropping the clone releases the remaining native backend state.
- Wake-before-cancel leaves one coalesced queue record that the owner safely
  skips after synchronous cancellation.
- Native-threaded project packaging selects exactly one host backend source.
  It emits neither the portable stub nor the other platform implementation.
- Windows and Windows GNU targets select
  `cr_executor_threaded_windows.c`. Linux GNU, Linux musl, and macOS targets
  select `cr_executor_threaded_posix.c`.
- The host-target test selects Windows source on Windows and POSIX source on
  Unix. Unsupported direct artifact queries retain only the portable stub.
- In this Windows workspace, Clang and GCC compile and execute the Windows
  backend and deterministic multithreaded fixture with warnings denied and
  inlining disabled.
- The same integration test selects the POSIX source and adds `-pthread` when
  it runs on Linux or macOS CI.
- `wasm32-wasi` continues to reject the native-threaded executor selection
  before publication. Single-thread WASI projects emit no Windows or POSIX
  source, atomics, mutex, or native condition variable.
- Required-mode portable executor, lifecycle, registration, Waker layout,
  Waker protocol, and generated-project WASI gates pass with
  `CRC_REQUIRE_WASM` restored to its original unset state.
- Stable Waker v1, public experimental executor declarations, core ABI v3,
  static await, CFG optimization, synchronous drop, and generated task layout
  remain unchanged.
- `cr_runtime.h` remains exactly 4,472 bytes with the Stage 4 FNV-1a 64-bit
  digest `0x70dc916f2d8ee4f0`.
- `cargo test --all-targets` passes with 139 library tests and every integration
  test, including the native threaded conformance suite.
- `pnpm run grammar:test` passes all four corpus parses.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.

## Task 9: Preserve compiler forwarding and static dispatch

This task proves the existing compiler pipeline already carries the Stage 5
poll context correctly and adds no dynamic fallback for known children.

**Files:**

- Extend `tests/coroutine_contract.rs`.
- Extend `tests/static_await_codegen.rs`.
- Extend `tests/static_await_project.rs`.
- Create a frozen Stage 4 compatibility fixture under
  `tests/fixtures/waker/`.
- Modify `src/c_emitter.rs` only if a conformance test exposes a real context
  forwarding defect.

**Steps:**

1. Poll a generated root through the single-thread executor using
   `*_into_awaitable`.
2. Forward the same borrowed context through embedded static children.
3. Forward the same context through boxed recursive and cross-unit children.
4. Exercise a genuine dynamic event awaitable that retains a Waker clone.
5. Assert static parent poll bodies contain direct typed child poll calls.
6. Reject `into_awaitable`, `as_awaitable`, and vtable poll at static child
   sites.
7. Compile an unchanged frozen Stage 4 translation unit that includes only
   `cr_runtime.h`.
8. Link that object into a Stage 5 executor application without recompiling it.
9. Pass only `CR_POLL_CAP_WAKER` in the Stage 5 poll context.
10. Prove the old task accepts the context and completes.
11. Prove manual null-context generated behavior remains byte-stable.
12. Re-run Stage 3 recursive, ownership, and cross-unit regressions.

**Focused gates:**

```powershell
cargo test --test coroutine_contract
cargo test --test static_await_codegen
cargo test --test static_await_project
```

**Acceptance evidence:**

- Waker support requires no new source syntax or lowering node.
- Static children remain allocation-aware typed dispatch.
- Old ABI v3 generated code interoperates with the Stage 5 executor context.

### Task 9 completion evidence

Task 9 completed on July 15, 2026, with this evidence:

- A generated `forwarding_root` is converted with
  `cr_forwarding_root_into_awaitable`, moved into the portable single-thread
  executor, and completes with value `42` after three queued polls.
- The generated root first awaits a genuine dynamic event and then an embedded
  static child. Both dynamic event instances receive the exact same borrowed
  `cr_poll_context *` supplied by the executor.
- Each dynamic event requires only `CR_POLL_CAP_WAKER`, clones the executor
  Waker, issues duplicate wakes while returning `Pending`, later returns
  `Ready`, and drops its retained clone exactly once.
- The embedded parent poll body calls
  `cr_forwarding_child_poll(&ctx->cr_child_..., poll_context)` directly and
  contains no child `into_awaitable`, `as_awaitable`, or owning-vtable path.
- A native executable polls an embedded static child and a boxed recursive
  child chain with one explicit non-null context object. Dynamic leaf
  awaitables observe exact pointer identity on both paths.
- The recursive poll body uses typed `cr_recursive_context_create`, direct
  `cr_recursive_context_poll(..., poll_context)`, typed result access, and
  typed destroy. It contains no recursive dynamic adapter path.
- The cross-unit project now runs its public typed child through a generated
  executor root. A dynamic boundary in the parent and a dynamic leaf in the
  child translation unit observe the same non-null context pointer.
- Cross-unit static sites continue to use the public opaque typed task API:
  create, direct poll with `poll_context`, typed result/yield/error access, and
  destroy. No child awaitable adapter or child vtable is referenced.
- `tests/fixtures/waker/stage4_abi_v3_root.c` is a frozen translation unit that
  includes only `cr_runtime.h` from CR, validates that the context advertises
  exactly `CR_POLL_CAP_WAKER`, yields `17`, and then returns `42`.
- The frozen Stage 4 fixture is compiled independently to an object. A separate
  Stage 5 application is then compiled with `cr_waker.h`, `cr_executor.h`, and
  portable executor sources and links the unchanged object without recompiling
  it.
- The Stage 5 executor successfully polls, observes, and owns the old ABI v3
  task, proving that the opaque Waker pointer and known capability bit require
  no core layout or generated-task change.
- `src/c_emitter.rs` required no modification. The conformance tests confirmed
  that embedded, boxed recursive, and cross-unit forwarding were already
  correct.
- The Stage 4 `OptimizationLevel::None` golden remains byte-identical, so
  manual null-context generated behavior and task layout remain unchanged.
- Focused gates pass: 6 coroutine contract tests, 13 static-await codegen
  tests, and 2 cross-unit static-await project tests.
- `cargo test --all-targets` passes with 139 library tests and every integration
  test, including recursive ownership, generated-project, native threaded,
  executor lifecycle, and WebAssembly validation suites.
- Required-mode `wasm32-wasi` executor, generated-project, Waker registration,
  Waker layout, and Waker protocol gates pass with `CRC_REQUIRE_WASM` restored
  afterward.
- `cargo fmt --check`, `cargo check --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `pnpm run grammar:test` all pass.

## Task 10: Complete generated-project and build-system integration

This task makes selected executor sources first-class compiler-owned artifacts
without changing the default manual project behavior.

**Files:**

- Modify project artifact assembly in `src/lib.rs`.
- Modify `src/template/templates/crc.toml` when the explicit default needs an
  update.
- Modify `src/template/templates/main.c` only if an opt-in example is needed.
- Extend `tests/generated_project.rs`.
- Extend incremental and manifest tests in `src/incremental/mod.rs` and
  `src/lib.rs` as needed.

**Steps:**

1. Add selected executor sources to the generated C source manifest.
2. Keep source ordering deterministic across Windows and POSIX paths.
3. Include internal runtime headers without exposing them as project headers.
4. Record Waker and executor artifacts with stable manifest kinds.
5. Remove stale executor artifacts when selection changes back to `manual`.
6. Preserve the last complete published artifact set after a failed build.
7. Build and execute a manual project with direct native C.
8. Build and execute a single-thread executor project with direct native C.
9. Build both projects through CMake.
10. Build both projects through Meson.
11. Build a native-threaded project on a supported host.
12. Prove default manual projects don't link executor symbols or sources.

**Focused gate:**

```powershell
cargo test --test generated_project
```

**Acceptance evidence:**

- Project publication includes exactly the selected runtime sources.
- Default manual output retains its existing execution and linkage shape.
- Direct C, CMake, and Meson agree on the runtime source set.

### Task 10 completion evidence

Task 10 completed on July 16, 2026, with this evidence:

- Project artifact collection publishes executor files only for the selected
  `runtime.executor` value. `manual` publishes no executor file,
  `single-thread` publishes the portable common, single-thread, and threaded
  stub sources, and `native-threaded` replaces the stub with exactly one
  target-selected Windows or POSIX backend.
- Generated source ordering is deterministic. Project translation units appear
  first, followed by `cr_executor_common.c`, `cr_executor_single.c`, and the
  selected threaded implementation.
- The artifact manifest records the public header as `executor-header`, the
  private runtime header as `executor-internal`, and every compiled runtime
  translation unit as `executor-source` in deterministic order.
- `cr_executor_internal.h` remains under `runtime/`. It is never copied into
  the public generated include directory.
- The generated Meson fragment now declares both
  `cr_generated_sources` and `cr_generated_dependencies`. Manual,
  single-thread, Windows, and stub selections use an empty dependency list.
- A POSIX native-threaded selection adds only Meson's `threads` dependency.
  It doesn't add that dependency to the portable single-thread or
  `wasm32-wasi` paths.
- The root Meson template passes the compiler-generated dependency list to the
  executable target.
- The CMake template detects only the selected POSIX threaded source, resolves
  `Threads::Threads`, and links it privately. Manual, single-thread, and
  Windows projects retain their prior linkage shape.
- A single-thread project compiles and executes through direct native C,
  CMake, and Meson. Each runtime source appears exactly once in the generated
  source manifest.
- A native-threaded project compiles and executes through direct native C,
  CMake, and Meson. It publishes, compiles, and records exactly the host
  backend and excludes the stub and the other native platform source.
- A manual project compiles and executes through direct native C, CMake, and
  Meson without publishing an executor header, runtime directory, executor
  manifest kind, executor source, or runtime dependency.
- Switching a published single-thread project to `manual` while the source is
  invalid leaves the complete previous executor artifact set and manifest
  unchanged.
- After the source is repaired, the successful manual build atomically replaces
  the distribution and removes the stale executor header, private header,
  runtime sources, source-manifest entries, and executor artifact records.
- The incremental compiler fingerprints runtime configuration changes. A
  single-thread-to-manual change returns `Rebuilt`, removes stale executor
  artifacts, and the following content-identical build returns `Unchanged`.
- `crc check` continues to validate runtime and target selection without
  publishing files or changing the last complete artifact set.
- Single-source compiler output remains independent of executor selection.
  Core ABI v3, generated task layout, Stage 3 static dispatch, Stage 4 context
  optimization, and synchronous drop remain unchanged.
- The focused generated-project gate passes 9 tests, including direct C,
  CMake, Meson, native backend selection, failed-publication preservation,
  successful stale cleanup, and target validation.
- `cargo test --all-targets` passes with 141 library tests and every integration
  test. Native threaded, Waker, frozen Stage 4 object, recursive static-await,
  ownership, and generated-project suites remain green.
- Required-mode executor, lifecycle, Waker registration, Waker layout, Waker
  protocol, and generated-project `wasm32-wasi` gates pass with
  `CRC_REQUIRE_WASM` restored afterward.
- `cargo fmt --check`, `cargo check --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `pnpm run grammar:test` all pass.

## Task 11: Compile and validate the single-thread path as WebAssembly

This task proves Waker and queued polling don't require threads, atomics, or a
target-specific core runtime branch.

**Files:**

- Extend `tests/wasm_generated_project.rs`.
- Modify runtime source generation only to fix failures found by the required
  WASI gate.
- Keep pinned versions under `tools/` unchanged unless an approved toolchain
  update is necessary.

**Steps:**

1. Select `target = "wasm32-wasi"` and
   `runtime.executor = "single-thread"` before CR compilation.
2. Add a Waker-aware controllable event to the generated project.
3. Compile an owning generated root awaitable into the FIFO executor.
4. Use the same source as a native fixture that executes `Pending`, wake,
   re-poll, and `Ready` without native threads.
5. Assert the emitted runtime set contains the single-thread source and stub.
6. Assert it contains no Windows, POSIX, or atomic threaded source.
7. Compile every generated source with pinned WASI Clang and C11 warnings
   denied.
8. Link the final module.
9. Validate it with pinned `wasm-tools`.
10. Preserve Stage 4 Aggressive layout planning in the same fixture.
11. Run the gate in required mode.
12. Restore `CRC_REQUIRE_WASM` after success or failure.

**Focused gate:**

```powershell
$previous = $env:CRC_REQUIRE_WASM
try {
    $env:CRC_REQUIRE_WASM = '1'
    cargo test --test wasm_generated_project
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
finally {
    if ($null -eq $previous) {
        Remove-Item Env:CRC_REQUIRE_WASM -ErrorAction SilentlyContinue
    }
    else {
        $env:CRC_REQUIRE_WASM = $previous
    }
}
```

**Acceptance evidence:**

- The Waker and FIFO executor compile and link as portable `wasm32-wasi` C11.
- The threaded backend is absent, not conditionally executed at runtime.
- The core ABI, static dispatch, and context optimizer remain intact.

### Task 11 completion evidence

Task 11 completed on July 16, 2026, with this evidence:

- The generated project selects `target = "wasm32-wasi"`,
  `optimization = "aggressive"`, and
  `runtime.executor = "single-thread"` before CR compilation.
- The project C integration layer defines a Waker-aware dynamic event. The
  event requires exactly `CR_POLL_CAP_WAKER`, clones the borrowed Waker,
  issues duplicate wakes, returns `Pending`, later returns `Ready`, and drops
  each retained clone exactly once.
- The generated CR root awaits one dynamic event directly and a second event
  through an embedded static child. The static edge remains a direct typed
  child poll with the borrowed `poll_context` and uses no child awaitable
  adapter.
- The generated root task is heap-created, converted with
  `cr_executor_root_into_awaitable`, moved into the single-thread executor, and
  observed through the public executor callback.
- The same generated project compiles and executes with every available native
  Clang and GCC compiler before the WASI link. It performs exactly three task
  polls, four event polls, two event drops, one terminal observer callback, and
  returns the value `42`.
- The generated C source set is exactly `main.c`,
  `cr_executor_common.c`, `cr_executor_single.c`, and
  `cr_executor_threaded_stub.c`, in deterministic order.
- The project publishes no Windows or POSIX threaded backend. Its artifact
  manifest contains exactly three `executor-source` records and its generated
  Meson dependency list is empty.
- The complete portable runtime source set contains no C atomics, pthread API,
  Windows header, Interlocked operation, critical section, condition variable,
  or thread creation API.
- The same CR translation unit retains the Stage 4 Aggressive layout fixture.
  Generated output contains optimized union storage, the lifted array types,
  and the static child stored through the optimized context slot.
- The gate verifies the exact `wasm32-wasi` target layout model with C static
  assertions for Waker size, aggregate size and alignment, and every modeled
  field offset.
- Pinned WASI SDK `27.0` compiles every generated translation unit and the
  project C application as C11 with `-Wall -Wextra -Werror`.
- The final module links successfully and pinned `wasm-tools 1.252.0` validates
  it.
- `CRC_REQUIRE_WASM=1` makes missing or mismatched tools a hard failure and is
  restored to its previous value after the focused gate.
- No runtime, compiler lowering, core ABI, Waker ABI, executor implementation,
  or toolchain pin required modification. Task 11 only strengthens the
  generated-project conformance fixture.
- `cargo test --all-targets` passes with 141 library tests and every integration
  test. Native executor, native threaded, generated-project, frozen Stage 4
  object, static-await, ownership, and Waker suites remain green.
- `cargo fmt --check`, `cargo check --all-targets`,
  `cargo clippy --all-targets -- -D warnings`, and
  `pnpm run grammar:test` all pass.

## Task 12: Run final Stage 5 native, ABI, race, and WASI gates

This task proves Stage 5 is complete without starting Stage 6 backend work.

**Files:**

- Modify implementation files only to fix failures found by final gates.
- Update RFC0002 and the design only when a genuine contract defect appears.
- Update this plan with final changed-file and gate evidence.

**Steps:**

1. Run Waker v1 native and WASI layout gates.
2. Run invalid Waker protocol and exact clone/drop accounting.
3. Run every deterministic lost-wakeup interleaving.
4. Run single-thread FIFO behavior, lifecycle, and allocation-failure tests.
5. Run Windows and POSIX cross-thread gates on their supported hosts.
6. Run cancellation, terminal, shutdown, and retained-clone races.
7. Run manual, single-thread, and native-threaded generated projects.
8. Run CMake and Meson project gates.
9. Run ABI v3, Stage 3 static await, and Stage 4 optimization regressions.
10. Run the frozen Stage 4 object compatibility link.
11. Run required `wasm32-wasi` compilation, linking, and validation.
12. Search production source for reactor, EventSource, backend SPI, socket, and
    timer behavior.
13. Search static child output for dynamic fallback.
14. Confirm manual and single-thread sources contain no atomic or thread API.
15. Record final artifact and changed-file lists.
16. Mark Stage 5 complete only after every named gate passes.

**Final commands:**

```powershell
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
pnpm run grammar:test
cargo test --test waker_v1_layout
cargo test --test waker_v1_protocol
cargo test --test waker_registration
cargo test --test reference_executor
cargo test --test reference_executor_threaded
cargo test --test coroutine_contract
cargo test --test static_await_codegen
cargo test --test static_await_project
cargo test --test coroutine_optimization
cargo test --test context_layout_codegen
cargo test --test generated_project
rg -n -i "reactor|eventsource|backend spi|socket|timer" src tests
rg -n "into_awaitable|as_awaitable|vtable->poll" `
  tests/static_await_codegen.rs tests/static_await_project.rs
rg -n "stdatomic|pthread|windows.h|CreateThread|CRITICAL_SECTION" src tests
```

Run the required WASI command from Task 11 with environment restoration after
the native gates.

**Acceptance evidence:**

- Waker v1 satisfies every RFC0002 ownership and wake guarantee.
- Manual polling remains structurally and behaviorally unchanged.
- Single-thread execution uses no atomics and passes native and WASI gates.
- Cross-thread wake preserves visibility and one poll owner.
- Payload, ticket, queue, executor, and Waker lifetimes are exactly owned.
- Core ABI v3, Stage 3 dispatch, and Stage 4 optimization remain unchanged.
- Reference executor APIs remain experimental.
- No Stage 6 backend behavior appears in production.

### Task 12 completion evidence

Task 12 completed on July 16, 2026, with this evidence:

- Task 12 required no implementation correction. Every named final gate passed
  against the Task 11 implementation state.
- Waker v1 native and WASI layout tests confirm the stable two-word handle,
  append-only vtable prefix, helper declarations, and exact clone and drop
  ownership rules.
- Invalid Waker ABI, null clone state, duplicate wake, spurious wake, late wake,
  and exact reference accounting tests pass.
- Every deterministic registration interleaving passes without sleeps. The
  awaitable registers a clone, rechecks readiness, and can't lose a wake at any
  publication boundary.
- The single-thread executor passes FIFO ordering, duplicate-wake coalescing,
  yielded requeue, terminal status, invalid status, aligned result, void
  result, cancellation, shutdown, ticket, and allocation-lifetime tests.
- The native threaded gate passes cross-thread clone, wake, drop, visibility,
  owner-thread enforcement, blocked wait, shutdown request, cancellation,
  terminal completion, and retained-clone-after-destroy races.
- The platform-specific threaded test compiles and executes the Windows backend
  on Windows and selects the POSIX backend with `-pthread` on Linux and macOS.
  It never compiles a native backend for `wasm32-wasi`.
- ABI v3 layout and malformed-object protocol tests pass with warnings denied.
- The frozen Stage 4 translation unit compiles independently and links unchanged
  into the Stage 5 executor application.
- `cr_runtime.h` remains exactly 4,472 bytes with the Stage 4 FNV-1a 64-bit
  digest `0x70dc916f2d8ee4f0`.
- `OptimizationLevel::None` generated output remains byte-identical to the
  Stage 4 golden fixture.
- Embedded, boxed recursive, and cross-unit static children continue to call
  concrete typed poll functions and forward the same borrowed
  `cr_poll_context *`.
- Static child poll bodies contain no `into_awaitable`, `as_awaitable`, or
  vtable poll fallback. Dynamic awaitables remain the only vtable boundary.
- Every Stage 4 optimization level passes verified CFG, ownership, lifetime,
  result, and generated native behavior tests.
- Manual, single-thread, and native-threaded generated projects compile and run
  through direct native C. Manual and single-thread projects also pass CMake
  and Meson, and the native-threaded host project passes both build systems.
- Failed project builds preserve the previous complete artifact set. Successful
  executor-selection changes atomically replace the distribution and remove
  stale runtime files and manifest records.
- Required-mode WASI tests pass for Waker layout, Waker helpers, registration,
  portable executor lifecycle, and the generated executor project.
- Pinned WASI SDK `27.0` compiles and links all portable sources with C11
  warnings denied. Pinned `wasm-tools 1.252.0` validates the final module.
- The required-mode gate restores `CRC_REQUIRE_WASM` after success or failure.
- A production-only source scan before each `#[cfg(test)]` section finds no
  reactor, EventSource, backend SPI, socket, or timer behavior.
- The portable artifact conformance test confirms that the single-thread source
  set contains no C atomics, pthread API, Windows synchronization API,
  condition variable, or thread creation call.

The final generated artifact matrix is:

- Every project publishes `include/cr_runtime.h`, `include/cr_waker.h`,
  generated C sources and public headers, `meson.build`, and
  `crc-artifacts.json`.
- `manual` publishes no executor artifact.
- `single-thread` adds `include/cr_executor.h`,
  `runtime/cr_executor_internal.h`, `runtime/cr_executor_common.c`,
  `runtime/cr_executor_single.c`, and
  `runtime/cr_executor_threaded_stub.c`.
- Windows `native-threaded` replaces the stub with
  `runtime/cr_executor_threaded_windows.c`.
- Linux and macOS `native-threaded` replace the stub with
  `runtime/cr_executor_threaded_posix.c` and add the private thread dependency.
- `wasm32-wasi` accepts only `manual` and `single-thread` and never publishes a
  native threaded source.

The final Stage 5 implementation and conformance file set is:

- Stable contracts and planning:
  `docs/rfcs/0001-core-coroutine-contract.md`,
  `docs/rfcs/0002-waker-contract.md`, the Stage 5 design, and this plan.
- Production runtime and project integration: `src/config/mod.rs`,
  `src/waker_abi.rs`, `src/executor_runtime.rs`, `src/lib.rs`,
  `src/incremental/mod.rs`, `src/template/mod.rs`, and the `crc.toml`, CMake,
  and Meson templates.
- Waker and executor conformance: `tests/waker_v1_layout.rs`,
  `tests/waker_v1_protocol.rs`, `tests/waker_registration.rs`,
  `tests/reference_executor.rs`, and `tests/reference_executor_threaded.rs`.
- Compiler compatibility and project integration: `tests/coroutine_contract.rs`,
  `tests/static_await_codegen.rs`, `tests/static_await_project.rs`,
  `tests/generated_project.rs`, and `tests/wasm_generated_project.rs`.
- Deterministic fixtures: every file under `tests/fixtures/waker/`, including
  registration, lifecycle, threaded barriers, allocator hooks, and the frozen
  Stage 4 ABI v3 object.
- Reproducible toolchain pins: `tools/wasi-sdk.version` and
  `tools/wasm-tools.version`.

The final gate results are:

- `cargo test --all-targets` passes with 141 library tests and every integration
  test.
- All named ABI, optimization, static-await, Waker, executor, threaded race,
  generated-project, CMake, Meson, and WASI focused gates pass.
- `cargo fmt --check`, `cargo check --all-targets`, and
  `cargo clippy --all-targets -- -D warnings` pass.
- `pnpm run grammar:test` passes all four corpus parses.
- Core ABI v3, synchronous drop, cleanup ownership, native-first typed static
  dispatch, and portable `wasm32-wasi` support remain intact.
- Waker v1 is stable. Reference executor API and layout remain experimental.
- Stage 5 introduces no reactor or backend SPI contract.

## Stop conditions

Stop and amend the approved design instead of broadening implementation when
any of these conditions occurs:

- Waker support requires changing the `cr_poll_context` layout.
- A new core capability bit is required for cross-thread wake.
- Valid Waker clone requires allocation or recoverable resource failure.
- Wake correctness requires synchronous or concurrent task poll.
- A late wake can read task payload after terminal, cancel, or shutdown.
- Cancellation requires asynchronous drop or awaited cleanup.
- The single-thread path requires atomics or native synchronization.
- `wasm32-wasi` requires threaded sources or WebAssembly atomics.
- Static children require dynamic adapter construction or indirect poll.
- Correctness requires reactor, EventSource, timer, socket, or backend SPI.
- Race correctness depends on sleeps or machine timing.
- Old ABI v3 generated code rejects the Stage 5 executor context.
- The experimental executor layout must be stabilized to complete the stage.

## Next steps

Stage 5 is complete. Continue with the approved
[Stage 6 design](../specs/2026-07-16-cr-backend-validation-stage-6-design.md)
and
[implementation plan](2026-07-16-cr-backend-validation-stage-6-implementation.md).
Stage 6 validates IOCP, epoll, and kqueue before stabilizing any backend prefix.
