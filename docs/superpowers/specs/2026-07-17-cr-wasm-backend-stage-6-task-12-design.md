# CR WebAssembly Backend project validation design

This specification defines Stage 6 Task 12. It proves that a generated
`wasm32-wasi` project can compose the single-thread executor, memory
conformance Provider, and reference receive awaitable without native socket
sources, shared memory, or WebAssembly threads.

> **Note:** The memory Provider and reference receive awaitable remain preview
> components under active development. Backend core v1 and net receive v1 are
> stable through RFC0003.

## Goals

Task 12 validates the complete generated-project boundary rather than isolated
artifact helpers. The test must:

- Generate a real `wasm32-wasi` project.
- Select the single-thread executor and `memory-conformance` Backend.
- Compile an owning generated root that uses the reference receive awaitable.
- Exercise submit, pump, completion, interrupt, cancellation, drop, and
  quiescence.
- Execute the same published source natively with available Clang and GCC
  compilers.
- Compile and link every published C source with the pinned WASI SDK.
- Validate and inspect the final module with the pinned `wasm-tools` release.
- Preserve static await dispatch and aggressive context-layout optimization.
- Reject native Provider sources, native socket dependencies, shared memory,
  WebAssembly threads, and WebAssembly atomic instructions.

Task 12 doesn't add a WASI runtime dependency, execute the final Wasm module,
change pinned tool versions, or modify any stable ABI contract.

## Test boundary

The test lives in `tests/wasm_backend_project.rs`. Keeping this gate separate
from `tests/wasm_generated_project.rs` isolates Backend lifecycle failures from
the existing compiler and executor portability gate.

The test consumes files published into the generated project's `crc/dist`
directory. It doesn't construct the Backend distribution by calling internal
artifact helpers. This boundary verifies configuration, target validation,
artifact planning, deduplication, manifest generation, and publication from
Task 11 before it verifies C behavior.

Existing WASI discovery and command patterns remain the reference behavior.
Task 12 can extract narrowly scoped shared test support only when that removes
meaningful duplication without merging the independent acceptance gates.

## Generated project

The fixture configures these project options:

- `target = "wasm32-wasi"`.
- `executor = "single-thread"`.
- `backends = ["memory-conformance"]`.
- Aggressive context-layout optimization.

The generated distribution must contain the portable runtime ABI, Waker ABI,
single-thread executor, stable Backend and net headers, Backend common source,
memory Provider, and reference receive awaitable. Each shared artifact must
appear exactly once.

The distribution must not contain:

- `cr_backend_iocp.c`.
- `cr_backend_epoll.c`.
- `cr_backend_kqueue.c`.
- WinSock, epoll, kqueue, or other native socket helper sources.
- Native threaded executor sources.
- Native thread or socket build dependencies.

The artifact manifest and generated CMake and Meson metadata must agree with
the published portable source set.

## Owning root and protocol scenario

The generated CR source owns its root task and uses at least one typed static
child before crossing the dynamic boundary into the reference receive
awaitable. This shape proves that Backend integration doesn't replace typed
static child polling with a dynamic vtable call.

The C harness provides caller-owned Backend and operation storage, a borrowed
connected-handle token accepted by the memory Provider, and a borrowed pinned
receive buffer. It drives deterministic scenarios through public generated and
stable Backend interfaces.

### Completion path

The completion path performs this sequence:

1. Create the memory Provider and owning root.
2. Submit the reference receive operation.
3. Poll or run the executor until the operation is pending.
4. Pump the Provider without completion and observe no terminal result.
5. Publish controlled input through the memory Provider.
6. Pump again and allow the registered Waker to reschedule the root.
7. Observe one successful terminal completion and the expected bytes.
8. Drop the owning task and verify that cleanup occurs exactly once.
9. Quiesce and destroy the Provider without a live operation.

### Interrupt path

The interrupt path requests the Provider's thread-safe interrupt while its
owner remains responsible for pumping. The next owner-side pump must report
the interrupt through the stable pump result without completing an unrelated
operation or violating quiescence.

### Cancellation path

The cancellation path leaves one receive pending, requests cancellation
through the owner-side contract, and drives the Provider until it publishes at
most one terminal cancellation. Dropping the owning root must wait for, or
establish, operation quiescence before storage can be reused. Repeated cancel,
drop, pump, or shutdown actions must not duplicate completion or cleanup.

The harness emits a normalized transcript for observable state transitions.
Native compiler runs must produce identical transcripts.

## Native execution

Task 12 compiles the same generated and runtime source set with each available
host Clang and GCC compiler. Each build uses C11, enables common warnings, and
treats warnings as errors.

The native executable runs every protocol scenario. The test compares its
normalized transcript with the canonical expected transcript and with the
other compiler's transcript when both compilers are available. A missing host
compiler doesn't weaken the required WASI gate when `CRC_REQUIRE_WASM=1`.

Native execution supplies behavioral evidence because Task 12 deliberately
doesn't add a WebAssembly runtime to the pinned toolchain. It doesn't claim to
validate WASI runtime integration.

## Pinned WASI compilation

The test discovers the pinned WASI SDK and `wasm-tools` versions using the same
repository version files and environment contract as existing WASI tests.

Pinned WASI Clang must:

1. Compile every generated translation unit.
2. Compile every published runtime and Backend translation unit.
3. Use the WASI sysroot and C11 mode with warnings denied.
4. Link one final module from the complete object set.

The final module doesn't need an exported application entry point for runtime
execution. It must retain enough code to inspect the owning root, Provider,
and awaitable integration rather than validating an empty link result.

Pinned `wasm-tools` must validate the module and produce a representation that
the test can inspect. The gate rejects:

- Shared WebAssembly memory.
- Thread-related imports or target features.
- `atomic.` or `.atomic` WebAssembly instructions.
- Native Provider and native socket symbols.

The memory Provider can use C11 atomics for its interrupt state. On the
single-thread `wasm32-wasi` target, those source operations must compile without
shared memory or WebAssembly atomic instructions.

## Toolchain and environment handling

`CRC_REQUIRE_WASM=1` makes the complete pinned WASI gate mandatory. Missing
SDK paths, binaries, sysroots, version mismatches, compile failures, link
failures, validation failures, and inspection failures must stop the test with
a stage-specific diagnostic that includes command output.

Without required mode, an unavailable pinned WASI toolchain can skip only the
WASI compilation sub-gate. Artifact validation and available native compiler
runs still execute.

The focused command owns the temporary `CRC_REQUIRE_WASM` override. It records
the previous value and restores that value, or removes the variable when it
was previously absent, after the test command succeeds or fails. Test code
must not mutate the process environment concurrently.

## Optimization invariants

The fixture must retain the existing native-first coroutine architecture:

- Typed same-unit static child awaits use direct poll calls.
- Only the reference receive awaitable uses dynamic dispatch.
- The owning root forwards one poll context through static children.
- Aggressive layout planning reuses legal nonoverlapping storage.
- Generated layout metadata remains valid for the reviewed WASI data model.
- Backend integration doesn't add task or executor pointers to Provider state.

The test inspects generated source and available layout evidence for these
properties. It doesn't freeze private context layout bytes.

## Diagnostics and failure isolation

Every external command records stdout and stderr. Failure messages identify
artifact generation, native compilation, native execution, WASI compilation,
WASI linking, validation, or inspection as the failing stage.

The test validates the published file set before invoking compilers. This
ordering distinguishes an incorrect target package from a C portability
failure. Temporary directories own all generated objects and executables, so a
failed run doesn't modify the workspace distribution.

## Acceptance criteria

Task 12 is complete when all of these statements are true:

- A real generated `wasm32-wasi` project publishes exactly the portable
  single-thread executor, memory Provider, and reference receive awaitable.
- The owning root completes normal, interrupt, cancellation, drop, and
  quiescence scenarios through public boundaries.
- Available native Clang and GCC builds execute with the same canonical
  transcript.
- Pinned WASI Clang compiles and links every published C source.
- Pinned `wasm-tools` validates the final module.
- The module has no shared memory, thread feature, WebAssembly atomic
  instruction, native Provider symbol, or native socket dependency.
- Static await dispatch and aggressive context-layout optimization remain
  intact.
- Backend core v1, net receive v1, runtime ABI v3, and Waker v1 remain
  unchanged.
- Required-mode failures are explicit, and the calling shell restores
  `CRC_REQUIRE_WASM` afterward.

## Next steps

After Task 12 passes locally and in the required WASI gate, Task 13 runs the
complete Stage 6 regression and supported-host matrix before Stage 6 can be
marked complete.
