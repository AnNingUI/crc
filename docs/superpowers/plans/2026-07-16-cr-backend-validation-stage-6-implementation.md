# CR backend validation and SPI Stage 6 implementation plan

This plan implements the approved Stage 6 backend validation design. It builds
one completion provider, two readiness providers, and one portable conformance
provider before CR freezes any backend core prefix.

The requirements come from the approved
[Stage 6 design](../specs/2026-07-16-cr-backend-validation-stage-6-design.md),
the
[coroutine architecture](../specs/2026-07-14-cr-coroutine-architecture-v3-design.md),
[RFC0001](../../rfcs/0001-core-coroutine-contract.md),
[RFC0002](../../rfcs/0002-waker-contract.md), and the completed
[Stage 5 design](../specs/2026-07-15-cr-waker-reference-executor-stage-5-design.md).

> **Note:** This workspace isn't a Git repository. Each task ends with named
> gates and a live status update instead of a commit checkpoint.

## Outcome

At completion, CR has a validated backend core, a versioned net-receive
extension, and target-selected reference providers for IOCP, epoll, kqueue,
and portable memory conformance. A final RFC freezes only the common semantic
prefix proven by all required providers.

Stage 6 must satisfy these invariants:

- `CR_RUNTIME_ABI_VERSION` remains `3`.
- `cr_runtime.h`, `cr_poll_context`, and Waker v1 remain unchanged.
- Manual null-context polling remains valid.
- Static children remain typed and directly polled.
- Backend code never stores a task or executor pointer.
- Backend callbacks only publish completion and wake.
- Backend control operations remain owner-thread only.
- Backend interrupt remains the only cross-thread core operation.
- Receive uses a borrowed connected socket and borrowed pinned buffer.
- Caller-owned opaque operation storage is queried by size and alignment.
- Submit, cancel, completion, and quiescence don't require allocation.
- One imported socket has at most one active receive in v1.
- Cancellation produces one terminal callback at most once.
- Quiescence completes before terminal poll returns or awaitable drop ends.
- IOCP, epoll, and kqueue keep raw event details private.
- `wasm32-wasi` publishes only the memory provider and portable headers.
- Stage 5's single-thread executor remains free of atomics and native threads.
- The optional memory provider can use C11 atomics only for interrupt state.
- No EventSource, dynamic plugin loader, timer, send, connect, or accept API is
  introduced.
- No backend prefix becomes stable before Task 10.

## Project selection

Backend packaging uses an extensible list in the existing runtime section.

```toml
[runtime]
executor = "manual"
backends = []
```

The first serialized backend selections are:

- `memory-conformance`.
- `native-net`.

Selection follows these rules:

- The default empty list preserves Stage 5 project output.
- `memory-conformance` is valid for native targets and `wasm32-wasi`.
- `native-net` selects IOCP on Windows.
- `native-net` selects epoll on Linux.
- `native-net` selects kqueue on macOS.
- `native-net` is rejected for `wasm32-wasi` and unknown custom targets.
- Duplicate selections are rejected.
- Multiple different selections publish shared core artifacts once.

## Generated artifacts

Stage 6 owns an experimental artifact family until Task 10 freezes the proven
prefix.

Common public artifacts:

- `include/cr_backend.h`.
- `include/cr_net.h`.

Common private and source artifacts:

- `runtime/cr_backend_internal.h`.
- `runtime/cr_backend_common.c`.
- `runtime/cr_net_recv.c`.

Provider artifacts:

- `runtime/cr_backend_memory.c`.
- `runtime/cr_backend_iocp.c`.
- `runtime/cr_backend_epoll.c`.
- `runtime/cr_backend_kqueue.c`.

Only selected provider sources are published. Native sources are absent on
unsupported targets instead of being compiled behind runtime conditionals.

## Rust module boundaries

The implementation keeps generated C artifacts separated by responsibility.

Planned production modules:

- `src/backend_abi.rs` owns public header text, version constants, extension
  identities, and stable error categories.
- `src/backend_runtime/mod.rs` owns artifact descriptions and target selection.
- `src/backend_runtime/common.rs` owns shared backend core C artifacts.
- `src/backend_runtime/net.rs` owns the reference receive awaitable C artifact.
- `src/backend_runtime/memory.rs` owns the portable conformance provider.
- `src/backend_runtime/iocp.rs` owns the Windows completion provider.
- `src/backend_runtime/epoll.rs` owns the Linux readiness provider.
- `src/backend_runtime/kqueue.rs` owns the macOS readiness provider.

No native provider source is embedded in the common or memory module. This
keeps portability audits and target packaging mechanical.

## Live status

Stages 0 through 5 are complete. The Stage 6 design passed incremental user
approval, written specification review, and final user confirmation.

```text
Completed: Stages 0 through 5
Completed: Stage 6 architecture choices
Completed: Stage 6 design and user approval
Completed: Task 1 backend selection and target validation
Completed: Task 2 experimental backend and net records
Completed: Task 3 portable memory provider
Completed: Task 4 reference net-receive awaitable
Completed: Task 5 cancellation and allocation hardening
Completed: Task 6 Windows IOCP provider
Completed: Task 7 Linux epoll provider
In progress: Task 8 macOS kqueue provider
Next: Complete Task 8 on a real macOS Clang runner
Pending: Tasks 8 through 13
External gate: Real macOS execution evidence is not available in this workspace
```

## Task 1: Add backend selection without changing output

This task introduces backend configuration and target validation without
publishing Stage 6 headers or sources.

**Files:**

- Modify `src/config/mod.rs`.
- Modify `src/lib.rs` only to thread validated selection into artifact
  planning without using it.
- Modify `src/template/templates/crc.toml`.
- Extend configuration tests in `src/config/mod.rs`.
- Extend project validation tests in `tests/generated_project.rs`.
- Update this plan's live status.

**Steps:**

1. Add a serialized `BackendSelection` enum with `memory-conformance` and
   `native-net` values.
2. Add `RuntimeConfig.backends` as an empty-by-default ordered list.
3. Reject duplicate selections before artifact collection.
4. Accept `memory-conformance` for every reviewed target.
5. Accept `native-net` for host, Windows, Linux, and macOS targets.
6. Reject `native-net` for `wasm32-wasi` and custom targets.
7. Preserve `runtime.executor` serialization and defaults.
8. Include backend selection in incremental configuration fingerprints.
9. Prove `crc check` validates selection without writing artifacts.
10. Prove default generated C, headers, artifact manifests, and build-system
    files remain byte-identical to the completed Stage 5 baseline.

**Focused gate:**

```powershell
cargo test config::tests
cargo test --test generated_project check_validates
```

**Acceptance evidence:**

- Backend selection is explicit and target validated.
- Default projects publish no Stage 6 artifact.
- Invalid selection can't replace the last complete distribution.
- Core ABI, Waker, executor, and generated task output remain unchanged.

## Task 2: Define experimental backend and net records

This task adds experimental public declarations and C conformance without a
provider implementation or stable ABI promise.

**Files:**

- Create `src/backend_abi.rs`.
- Export the module from `src/lib.rs`.
- Create `tests/backend_abi.rs`.
- Create C fixtures under `tests/fixtures/backend/abi/`.
- Extend WASI layout-model coverage only when the new records require it.

**Steps:**

1. Define experimental backend and net ABI version constants.
2. Define the 128-bit extension identity type.
3. Assign stable candidate identities for backend core and net receive.
4. Define provider and extension descriptor prefixes with version, structure
   size, capability bits, and identity.
5. Define storage size/alignment records.
6. Define native socket handle kinds and `uintptr_t` storage.
7. Define pump reasons and the versioned pump result record.
8. Define receive terminal kinds and the versioned completion record.
9. Define portable backend and network error categories.
10. Declare opaque backend and operation storage boundaries.
11. Declare owner-thread operations and thread-safe interrupt semantics.
12. Keep exact descriptor and callback layout marked experimental.
13. Compile headers as C11 with Clang, GCC, and pinned WASI Clang.
14. Test truncated prefixes, unknown tails, unknown capabilities, and invalid
    identities.

**Focused gate:**

```powershell
cargo test --test backend_abi
```

**Acceptance evidence:**

- Public records are versioned and appendable.
- Backend, socket tracking, and operation storage layouts remain opaque.
- The headers contain no task, executor, reactor, or EventSource pointer.
- Native and WASI compilers agree on every known prefix.

## Task 3: Implement the portable memory provider

This task implements the common backend core and a deterministic provider
without native sockets, operating-system event APIs, or background threads.

**Files:**

- Create `src/backend_runtime/mod.rs`.
- Create `src/backend_runtime/common.rs`.
- Create `src/backend_runtime/memory.rs`.
- Create `tests/backend_memory.rs`.
- Create fixtures under `tests/fixtures/backend/memory/`.
- Extend `src/lib.rs` to export artifact query helpers without publishing them
  in generated projects yet.

**Steps:**

1. Implement opaque backend creation from a static provider descriptor.
2. Record and enforce owner identity with a portable C11 `_Thread_local`
   token, without importing pthread or Win32 thread APIs.
3. Implement 128-bit extension query and capability miss behavior.
4. Implement operation storage size and alignment queries.
5. Implement deterministic memory socket handles and one active receive rule.
6. Implement receive initialization, submit, controlled completion, cancel,
   quiesce, reinitialization, and destruction.
7. Implement relative timeout and `max_events` validation.
8. Implement `Progress`, `Timeout`, `Interrupted`, and `Error` pump results.
9. Implement thread-safe interrupt with portable C11 atomic state.
10. Keep the provider free of native socket and event headers.
11. Make repeated cancel and interrupt requests idempotent.
12. Make backend shutdown cancel and quiesce every active operation.
13. Add deterministic hooks for every registration and terminal boundary.
14. Compile and execute the fixture with available native C compilers.
15. Compile, link, and validate it with pinned WASI tools.
16. Inspect the Wasm module and reject shared memory and thread features.

**Focused gate:**

```powershell
cargo test --test backend_memory
```

**Acceptance evidence:**

- The common state machine works without a native event model.
- The memory provider validates interrupt without creating a thread.
- Every successful submit produces one terminal callback.
- Rejected submit produces none.
- WASI uses no shared memory or WebAssembly thread feature.

## Task 4: Add the reference net-receive awaitable

This task composes the net extension with RFC0002 Waker semantics while
preserving manual polling.

**Files:**

- Create `src/backend_runtime/net.rs`.
- Extend `src/backend_abi.rs` with experimental awaitable declarations.
- Create `tests/backend_net_awaitable.rs`.
- Create fixtures under `tests/fixtures/backend/net/`.
- Extend `tests/coroutine_contract.rs` only for generated-root composition.

**Steps:**

1. Define caller-owned awaitable state without embedding backend-private
   operation bytes.
2. Accept an opaque backend, imported socket handle, pinned buffer, and
   caller-owned operation storage.
3. Use Waker registration only when a valid Waker is available.
4. Preserve manual null-context submit, pump, and repoll.
5. Clone and replace Wakers according to RFC0002.
6. Submit the operation once per awaitable generation.
7. Copy the borrowed terminal completion record in the callback.
8. Wake without directly polling or resuming a task.
9. Quiesce before returning `Ready`, `Error`, or `Canceled`.
10. Interpret `Ready(0)` as EOF.
11. Expose bytes transferred as the awaitable result.
12. Translate portable and native error fields into stable awaitable error
    storage.
13. Run the same awaitable through manual polling and the single-thread
    executor.
14. Run a generated owning root through `*_into_awaitable`.
15. Assert static generated children remain direct typed polls.

**Focused gate:**

```powershell
cargo test --test backend_net_awaitable
cargo test --test coroutine_contract
```

**Acceptance evidence:**

- The awaitable owns readiness and Waker registration.
- The provider owns no Waker.
- Manual polling doesn't require `CR_POLL_CAP_WAKER`.
- Terminal poll crosses quiescence before exposing the buffer to user code.
- Compiler lowering and static dispatch remain unchanged.

## Task 5: Prove cancellation, quiescence, and allocation safety

This task hardens synchronous destruction before native providers introduce
operating-system completion races.

**Files:**

- Extend `src/backend_runtime/common.rs`.
- Extend `src/backend_runtime/memory.rs`.
- Extend `src/backend_runtime/net.rs`.
- Extend `tests/backend_memory.rs`.
- Extend `tests/backend_net_awaitable.rs`.
- Create allocator and lifecycle hooks under
  `tests/fixtures/backend/lifecycle/`.

**Steps:**

1. Test cancel before completion publication.
2. Test cancel after completion publication but before pump.
3. Test cancel while pump dispatches the target operation.
4. Test success and error winning a cancel race.
5. Test repeated cancel before terminal completion.
6. Test awaitable drop before first submit.
7. Test awaitable drop with one active receive.
8. Test terminal callback during drop-driven quiescence.
9. Test backend shutdown with multiple active operations.
10. Deliver callbacks for unrelated operations encountered during quiescence.
11. Test that no callback occurs after quiescence.
12. Protect receive buffers with guard bytes.
13. Reject buffer, socket, or storage reuse before quiescence when detectable.
14. Add allocator hooks for backend, tracking, awaitable, and operation paths.
15. Prove submit, cancel, completion, and quiescence make no allocation.
16. Prove every allocation and handle is released after shutdown.

**Focused gate:**

```powershell
cargo test --test backend_memory lifecycle
cargo test --test backend_net_awaitable lifecycle
```

**Acceptance evidence:**

- Synchronous drop remains sufficient.
- Borrowed buffers and caller storage can't be accessed after quiescence.
- Cancellation delivers at most one terminal callback.
- Hot operation paths don't require allocation.

## Task 6: Implement the Windows IOCP provider

This task validates the completion model with real loopback TCP receive and
overlapped cancellation.

**Files:**

- Create `src/backend_runtime/iocp.rs`.
- Extend target artifact selection in `src/backend_runtime/mod.rs`.
- Create `tests/backend_iocp.rs`.
- Create shared native fixtures under `tests/fixtures/backend/native/`.
- Add Windows-specific helper headers only under the fixture directory.

**Steps:**

1. Create and own one IOCP queue per backend.
2. Associate imported WinSock handles without closing them.
3. Place `OVERLAPPED` and private generation state in caller-owned operation
   storage.
4. Submit one overlapped receive without hot-path allocation.
5. Pump completions with the configured timeout and event budget.
6. Map bytes, EOF, WinSock errors, and Win32 errors into the common record.
7. Implement `CancelIoEx` cancellation requests.
8. Drain the matching completion before quiescence releases storage.
9. Dispatch unrelated completion packets encountered during quiescence.
10. Use a private `PostQueuedCompletionStatus` packet for interrupt.
11. Reject private control packets as receive completions.
12. Enforce one active receive per socket.
13. Test immediate and deferred completion.
14. Test cancel before and after packet queueing.
15. Test interrupt, timeout, `max_events`, shutdown, and handle cleanup.
16. Compile and execute with Clang and GCC on Windows.
17. Keep the IOCP source absent from Linux, macOS, and WASI artifact sets.

**Focused gate:**

```powershell
cargo test --test backend_iocp
```

**Acceptance evidence:**

- The completion packet, not socket readiness, defines progress.
- IOCP never references operation storage after quiescence.
- Cancel races produce one terminal outcome.
- Pump interrupt is distinct from receive completion.

**Completion evidence (2026-07-16):**

- One provider-owned completion port serves each backend; imported WinSock
  handles remain borrowed and are never closed by the provider.
- Caller-owned operation storage contains `OVERLAPPED`, generation, callback,
  and intrusive lifecycle state without submit, cancel, completion, or
  quiescence allocation.
- Real loopback TCP fixtures cover inline and deferred completion, EOF,
  cancellation before completion and after packet queueing, unrelated packet
  dispatch during quiescence, one-active-receive enforcement, event budgets,
  timeout, interrupt coalescing, shutdown drain, and socket preservation.
- Provider and queue failures retain distinct WinSock and Win32 native error
  domains; private interrupt packets never become receive callbacks.
- Windows Clang and GCC both compile and execute `tests/backend_iocp.rs` with
  warnings denied.
- `cargo test --all-targets` passes 151 library tests and every integration
  gate, including native generated projects and pinned `wasm32-wasi`
  validation. Formatting, `cargo check --all-targets`, Clippy with warnings
  denied, and grammar tests pass.
- IOCP artifacts are returned only for Windows native-net target queries and
  remain absent from memory, Linux, macOS, custom, and `wasm32-wasi` sets.

## Task 7: Implement the Linux epoll provider

This task validates readiness with nonblocking receive, rearming, and stale
event retirement.

**Files:**

- Create `src/backend_runtime/epoll.rs`.
- Extend target artifact selection in `src/backend_runtime/mod.rs`.
- Create `tests/backend_epoll.rs`.
- Reuse shared native fixtures under `tests/fixtures/backend/native/`.
- Add Linux-specific deterministic hooks under the fixture directory.

**Steps:**

1. Create and own one epoll descriptor per backend.
2. Import but never close nonblocking connected socket descriptors.
3. Register one active receive per socket.
4. Use one-shot or explicit rearming semantics.
5. Perform nonblocking `recv` only after readable readiness.
6. Keep the operation pending after readiness followed by `EAGAIN`.
7. Map bytes, EOF, errno, and cancellation into the common record.
8. Remove or disarm interest during cancellation.
9. Use generation identity to reject stale readiness records.
10. Use `eventfd` or a private pipe for thread-safe interrupt.
11. Enforce timeout and `max_events` semantics.
12. Test data before registration and at every rearm boundary.
13. Test duplicate readiness and stale records after reuse.
14. Test cancel, quiesce, shutdown, and descriptor cleanup.
15. Compile and execute with Clang and GCC on Linux.
16. Keep epoll and Linux headers absent from Windows, macOS, and WASI sets.

**Focused gate:**

```powershell
cargo test --test backend_epoll
```

**Acceptance evidence:**

- Readiness remains private to the epoll provider.
- `EAGAIN` never becomes a false terminal completion.
- Rearming doesn't lose receive progress.
- Stale readiness can't access reused operation storage.

**Completion evidence (2026-07-16):**

- One provider-owned epoll descriptor uses `EPOLLONESHOT` for imported,
  connected, nonblocking TCP sockets. The provider never closes imported
  descriptors.
- Separate private `eventfd` descriptors carry thread-safe interrupt records
  and owner-thread cancellation completions. Duplicate interrupt writes
  coalesce when the owner drains the counter.
- Every submitted generation receives a monotonically increasing 64-bit event
  token. epoll records contain only the token, and the provider resolves it
  through the active operation set before accessing caller-owned storage.
- Deterministic hooks force readiness followed by `EAGAIN`, stale-token
  delivery after storage reuse, queued-readiness cancellation, and rearm
  boundaries. These cases produce no false completion or stale storage access.
- Real loopback TCP fixtures cover data before registration, data after rearm,
  duplicate readiness, EOF, errno completion, one-active-receive enforcement,
  `max_events`, timeout, cross-thread interrupt, cancellation, unrelated
  completion dispatch during quiescence, shutdown, and descriptor ownership.
- Linux GCC and Clang compile and execute the fixture with warnings denied.
  Clang AddressSanitizer and UndefinedBehaviorSanitizer pass, and GCC static
  analysis reports no diagnostics.
- `cargo test --all-targets` passes 152 library tests and every integration
  gate, including Windows IOCP, native generated projects, and pinned
  `wasm32-wasi` validation. Formatting, `cargo check --all-targets`, Clippy
  with warnings denied, and grammar tests pass.
- epoll artifacts are returned only for Linux native-net target queries and
  remain absent from Windows, macOS, custom, memory, and `wasm32-wasi` sets.

## Task 8: Implement the macOS kqueue provider

This task validates the second readiness implementation before common-prefix
stabilization.

**Files:**

- Create `src/backend_runtime/kqueue.rs`.
- Extend target artifact selection in `src/backend_runtime/mod.rs`.
- Create `tests/backend_kqueue.rs`.
- Reuse shared native fixtures under `tests/fixtures/backend/native/`.
- Add macOS-specific deterministic hooks under the fixture directory.

**Steps:**

1. Create and own one kqueue descriptor per backend.
2. Import but never close nonblocking connected socket descriptors.
3. Register one active `EVFILT_READ` operation per socket.
4. Use one-shot or explicit filter rearming.
5. Perform nonblocking `recv` after readable readiness.
6. Keep the operation pending after `EAGAIN`.
7. Map bytes, EOF flags, errno, and cancellation into the common record.
8. Delete or disable the filter during cancellation.
9. Use generation identity to reject stale events.
10. Use `EVFILT_USER` or a private pipe for interrupt.
11. Enforce timeout and `max_events` semantics.
12. Test data before registration and at every rearm boundary.
13. Test EOF flags independently from epoll assumptions.
14. Test cancel, quiesce, shutdown, and descriptor cleanup.
15. Compile and execute with Clang on macOS.
16. Keep kqueue headers absent from Windows, Linux, and WASI sets.

**Focused gate:**

```powershell
cargo test --test backend_kqueue
```

**Acceptance evidence:**

- kqueue validates readiness without reusing epoll implementation details.
- EOF, rearm, delete, and stale-event semantics match the common contract.

**Implementation evidence pending the macOS execution gate (2026-07-16):**

- One provider-owned kqueue descriptor uses `EV_DISPATCH` read filters and
  explicit `EV_ENABLE` rearming after `EAGAIN`.
- Separate `EVFILT_USER` identifiers carry thread-safe interrupt records and
  owner-thread cancellation completions without provider threads.
- Every submitted generation receives a monotonic 64-bit event token. kqueue
  records contain the token instead of an operation pointer, so retired
  records can't access reused caller storage.
- The fixture independently records `EV_EOF`, `fflags`, and `EV_ERROR`, and it
  uses deterministic hooks for `EAGAIN`, errno completion, stale tokens,
  rearming, and control-record ordering.
- Zig's macOS Clang frontend compiles and links the complete fixture for both
  `x86_64-macos` and `aarch64-macos` with warnings denied.
- A Linux libkqueue behavioral surrogate executes the full loopback fixture,
  including cancellation, rearming, stale records, timeout, interrupt,
  shutdown, and ownership. AddressSanitizer and UndefinedBehaviorSanitizer
  pass on that surrogate.
- `cargo test --all-targets` passes 153 library tests and every available
  native, ABI, generated-project, and `wasm32-wasi` gate. Formatting,
  `cargo check --all-targets`, Clippy with warnings denied, and grammar tests
  pass.
- Task 8 remains incomplete until the same fixture compiles and executes with
  Clang on an Apple macOS kernel. Cross-compilation and libkqueue execution do
  not replace that acceptance requirement.
- Native operation storage is safe through quiescence.

## Task 9: Run cross-provider differential analysis

This task compares semantic output and removes operations that aren't honestly
shared before any prefix becomes stable.

**Files:**

- Create `tests/backend_differential.rs`.
- Create a shared scenario fixture under
  `tests/fixtures/backend/differential/`.
- Modify experimental declarations and providers only when differential
  evidence proves a mismatch.
- Update the Stage 6 design when an approved semantic correction is necessary.

**Steps:**

1. Define one abstract log for submit, pump, callback, wake, cancel, quiesce,
   reuse, and destroy observations.
2. Run identical loopback receive scenarios on each supported native provider.
3. Run the same state scenarios on the memory provider.
4. Compare terminal kind, bytes, portable category, callback count, and
   lifecycle order.
5. Permit native error domain and code differences.
6. Compare owner-thread and interrupt behavior.
7. Compare `max_events` fairness without requiring equal native record counts.
8. Compare allocation counts on every hot path.
9. Confirm one active receive per socket on every provider.
10. Confirm quiescence prevents every later access.
11. Remove or move provider-specific fields out of the common prefix.
12. Repeat the differential gate after every interface change.

**Focused gate:**

```powershell
cargo test --test backend_differential
```

**Acceptance evidence:**

- Completion and readiness produce one common receive lifecycle.
- Raw IOCP, epoll, and kqueue records remain private.
- The surviving common prefix has evidence from every required provider.
- No interface is stable yet.

## Task 10: Freeze the proven semantic prefix

This task converts validated common behavior into a stable RFC and rejects
unproven fields from the v1 prefix.

**Files:**

- Create `docs/rfcs/0003-backend-core-and-net-receive-contract.md`.
- Update `docs/rfcs/0001-core-coroutine-contract.md`.
- Update the Stage 6 design with final validated names and stability classes.
- Modify `src/backend_abi.rs` to mark stable v1 constants and prefixes.
- Create frozen ABI fixtures under `tests/fixtures/backend/stable/`.
- Extend `tests/backend_abi.rs` with byte and prefix stability tests.
- Update this plan with review evidence.

**Steps:**

1. Record observable backend core semantics instead of provider state machines.
2. Record extension identity and version negotiation.
3. Record owner-thread and interrupt lifetime rules.
4. Record pump timeout, event budget, reason, count, and error behavior.
5. Record borrowed socket, buffer, and operation storage ownership.
6. Record submit, cancel, terminal callback, quiesce, reuse, and destroy.
7. Record manual polling and Waker-aware polling behavior.
8. Classify provider implementations and reference awaitable layout as
   experimental.
9. Freeze only fields used by memory, IOCP, epoll, and kqueue providers.
10. Remove placeholders, provider-specific names, and raw native event fields.
11. Compile the frozen v1 headers on every supported target.
12. Compare the stable header prefix against a checked-in golden fixture.
13. Record an explicit deferral instead of freezing when no honest common
    prefix survives.
14. Obtain user approval of RFC0003 before project packaging treats the prefix
    as stable.

**Focused gate:**

```powershell
if (rg -n "TODO|TBD|placeholder" `
  docs/rfcs/0003-backend-core-and-net-receive-contract.md) {
    throw 'RFC0003 contains a placeholder'
}
cargo test --test backend_abi
```

**Acceptance evidence:**

- The stable prefix is semantic, versioned, minimal, and provider neutral.
- Every frozen field has four-provider conformance evidence.
- Provider, reference awaitable, and operation layouts remain opaque.
- Core ABI v3 and Waker v1 don't change.

## Task 11: Complete generated-project integration

This task publishes selected providers and build dependencies without changing
the default empty-backend project.

**Files:**

- Modify artifact collection in `src/lib.rs`.
- Extend `src/backend_runtime/mod.rs` artifact selection.
- Modify `src/template/templates/CMakeLists.txt`.
- Modify `src/template/templates/meson.build`.
- Extend `src/template/mod.rs` tests.
- Extend `src/incremental/mod.rs` tests.
- Extend `tests/generated_project.rs` or create
  `tests/backend_generated_project.rs` when isolation is clearer.

**Steps:**

1. Publish shared backend and net artifacts once for any selected provider.
2. Publish exactly the selected provider sources.
3. Keep private headers under `runtime/`.
4. Record stable header, internal header, common source, awaitable source, and
   provider source manifest kinds.
5. Keep artifact and generated-source ordering deterministic.
6. Link WinSock only for the IOCP provider.
7. Add no provider thread dependency for epoll, kqueue, or memory.
8. Reject `native-net` before publication on unsupported targets.
9. Remove stale backend artifacts after a successful selection change.
10. Preserve the previous complete artifact set after a failed build.
11. Rebuild incrementally when backend selection changes.
12. Build and run empty, memory, and native-net projects with direct C.
13. Build and run each supported selection through CMake and Meson.
14. Prove empty-backend projects retain Stage 5 output and linkage shape.

**Focused gate:**

```powershell
cargo test --test generated_project
cargo test --test backend_generated_project
```

Run the second command only when the test is split into its own file.

**Acceptance evidence:**

- Project publication contains exactly the selected provider family.
- Shared artifacts appear once.
- Build-system dependencies match the selected target provider.
- Default projects pay no Stage 6 runtime or linkage cost.

## Task 12: Validate the memory provider as WebAssembly

This task proves the stabilized headers and conformance provider remain
portable without native sockets, shared memory, or WebAssembly threads.

**Files:**

- Create `tests/wasm_backend_project.rs`.
- Extend `tests/backend_memory.rs` when shared helpers are needed.
- Modify provider source only to fix failures found by required WASI gates.
- Keep pinned tool versions unchanged unless a separate update is approved.

**Steps:**

1. Create a generated project with `target = "wasm32-wasi"`.
2. Select `memory-conformance` and a single-thread executor.
3. Compile a generated owning root through the reference receive awaitable.
4. Execute the same source with native Clang and GCC.
5. Exercise submit, pump, controlled completion, interrupt, cancel, and
   quiesce.
6. Assert that no IOCP, epoll, kqueue, or native socket source is emitted.
7. Assert that Stage 5's executor sources remain atomics-free.
8. Compile every generated and runtime source with pinned WASI Clang.
9. Link the final module with C11 warnings denied.
10. Validate it with pinned `wasm-tools`.
11. Inspect the module and reject shared memory and thread feature use.
12. Preserve Aggressive context-layout planning in the same fixture.
13. Run in required mode and restore `CRC_REQUIRE_WASM` afterward.

**Focused gate:**

```powershell
$previous = $env:CRC_REQUIRE_WASM
try {
    $env:CRC_REQUIRE_WASM = '1'
    cargo test --test wasm_backend_project
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

- Stable headers and memory provider compile and link as portable C11.
- The Wasm module requires no shared memory or thread feature.
- Native provider artifacts are absent.
- Generated tasks, static dispatch, and context optimization remain intact.

## Task 13: Run final Stage 6 gates

This task proves backend stabilization didn't weaken any Stage 0 through Stage
5 contract or introduce out-of-scope runtime behavior.

**Files:**

- Modify implementation files only to fix failures found by final gates.
- Update RFC0003 and the Stage 6 design only for genuine contract defects.
- Update this plan with final artifact, file, and gate evidence.

**Steps:**

1. Run backend ABI native and WASI prefix gates.
2. Run memory provider state, interrupt, and allocation gates.
3. Run reference awaitable manual and executor composition gates.
4. Run every cancel, quiesce, drop, and shutdown race.
5. Run IOCP on Windows, epoll on Linux, and kqueue on macOS.
6. Run cross-provider differential logs on every supported host.
7. Run empty, memory, and native-net generated projects.
8. Run direct C, CMake, and Meson project gates.
9. Run frozen Stage 4 and stable backend v1 compatibility objects.
10. Run ABI v3, Waker v1, static await, and CFG optimization regressions.
11. Run required `wasm32-wasi` compilation, linking, inspection, and
    validation.
12. Search production code for EventSource, dynamic plugin loading, timer,
    send, connect, accept, DNS, TLS, UDP, and backend-to-task coupling.
13. Confirm default manual and Stage 5 single-thread artifacts remain unchanged.
14. Record final stable and experimental artifact classes.
15. Mark Stage 6 complete only after every supported-host gate passes.

**Final commands:**

```powershell
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
pnpm run grammar:test
cargo test --test backend_abi
cargo test --test backend_memory
cargo test --test backend_net_awaitable
cargo test --test backend_iocp
cargo test --test backend_epoll
cargo test --test backend_kqueue
cargo test --test backend_differential
cargo test --test generated_project
cargo test --test backend_generated_project
cargo test --test wasm_backend_project
cargo test --test coroutine_contract
cargo test --test static_await_codegen
cargo test --test static_await_project
cargo test --test coroutine_optimization
cargo test --test context_layout_codegen
cargo test --test reference_executor
cargo test --test reference_executor_threaded
```

Run only test targets that exist after the approved task split. Run the
required WASI command from Task 12 with environment restoration.

**Acceptance evidence:**

- Backend core v1 and net receive semantics match RFC0003.
- IOCP, epoll, and kqueue preserve one common receive lifecycle.
- Readiness and completion remain provider-private.
- Waker registration and synchronous drop remain intact.
- Hot operation paths require no allocation.
- Default projects retain Stage 5 output and runtime cost.
- WASI retains portable C11 without shared memory or threads.
- No EventSource or out-of-scope network API appears in production.

## Stop conditions

Stop and amend the approved design instead of broadening implementation when
any of these conditions occurs:

- Correctness requires changing core ABI v3 or Waker v1.
- A backend needs a task or executor pointer in the shared ABI.
- A backend must directly poll or resume a coroutine.
- Safe cancellation requires async drop.
- Quiescence can't retire native references before task payload destruction.
- Borrowed receive requires mandatory copying.
- A fixed public inline operation capacity becomes necessary.
- IOCP, epoll, and kqueue can't share an honest terminal record.
- A provider-specific raw event field becomes necessary in the common prefix.
- Submit, cancel, completion, or quiescence requires allocation.
- Portable headers require native socket, thread, or event declarations.
- WASI requires a native provider, shared memory, or threads.
- Correctness tests require timing sleeps.
- One active receive per socket is insufficient for basic validation.
- Connect, accept, send, DNS, TLS, UDP, timer, file I/O, GPU, or browser
  behavior becomes necessary.
- Dynamic plugin loading becomes necessary.

## Next steps

After user approval, begin Task 1 by adding backend selection and target
validation without changing generated artifacts. Don't publish experimental
headers or provider sources until Task 2 and Task 3 establish their executable
conformance gates.
