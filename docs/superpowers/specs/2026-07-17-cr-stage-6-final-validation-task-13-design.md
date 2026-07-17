# CR Stage 6 final validation design

This specification defines the final Stage 6 validation matrix for Task 13.
It consolidates native host validation into one workflow and adds one pinned
WebAssembly job before Stage 6 can be marked complete.

> **Note:** Reference Providers, the reference receive awaitable, and the
> reference executors remain preview components under active development.
> Backend core v1 and net receive v1 are stable through RFC0003.

## Goals

Task 13 produces reproducible evidence that Stage 6 works on every supported
native host and remains portable to `wasm32-wasi`.

The final gate must:

- Run the complete Rust and C integration suite on Windows, Linux, and macOS.
- Run the real IOCP, epoll, and kqueue Provider on its matching host.
- Compare the memory Provider with each native Provider.
- Build generated projects through direct C, CMake, and Meson.
- Check formatting, Clippy, and the Tree-sitter grammar.
- Run a separate required-mode job with pinned WASI SDK 27 and
  `wasm-tools` 1.252.0.
- Preserve all frozen runtime, Waker, coroutine, and Backend ABI gates.
- Keep every job independent so one host failure doesn't hide other results.

Task 13 doesn't add runtime features, change an ABI, execute Wasm, publish
artifacts, or create a release.

## Workflow consolidation

`.github/workflows/backend-differential.yml` becomes the single Stage 6 final
workflow. The workflow keeps push, pull request, and manual triggers.

The existing `.github/workflows/macos-kqueue.yml` is removed after its kqueue,
formatting, lint, test, and grammar coverage moves into the consolidated
matrix. This avoids two independent definitions of the macOS acceptance gate.

The workflow retains:

- Read-only repository permissions.
- Per-ref concurrency cancellation.
- `fail-fast: false` for native platforms.
- Explicit job timeouts.
- Rust build caching.

## Native matrix

The native job contains these reviewed hosts:

- `windows-2022` for memory and IOCP.
- `ubuntu-24.04` for memory and epoll.
- `macos-14` for memory and kqueue.

Every host installs the stable Rust toolchain with `rustfmt` and `clippy`,
Meson, Node.js 24, pnpm, and frozen grammar dependencies. macOS also installs
Zig so the kqueue test can compile both Intel and Apple Silicon artifacts.

Each matrix entry runs the same authoritative commands:

```text
cargo fmt --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
pnpm run grammar:test
```

`cargo test --all-targets` owns Provider-specific selection. Tests that don't
match the current operating system validate artifact boundaries without
claiming native execution. The matching IOCP, epoll, or kqueue test runs its
real loopback conformance fixture.

The complete test command also covers:

- Backend v1 native and WASI prefix fixtures.
- Memory lifecycle, owner, interrupt, allocation, cancellation, and shutdown.
- Reference awaitable manual and executor composition.
- Cross-provider normalized transcripts.
- Empty, memory, native, and combined generated-project selections.
- Direct C, CMake, and Meson generated builds.
- Frozen Stage 4, runtime ABI v3, Waker v1, static await, coroutine CFG, and
  context-layout regression gates.
- The Task 12 generated WebAssembly Backend project when pinned tools are
  available.

## Pinned WebAssembly job

The `wasm32-wasi` job runs on `ubuntu-24.04`. It installs the same stable Rust
toolchain and Rust cache as the native jobs.

The job reads version values from:

- `tools/wasi-sdk.version`.
- `tools/wasm-tools.version`.

It downloads the matching official WASI SDK release, extracts it into the
runner's temporary directory, and exports `WASI_SDK_PATH`. It installs the
matching `wasm-tools` executable and makes it available through `PATH`.

Before testing, the job checks both executable versions. Test code performs a
second version check against the repository pins.

The job sets `CRC_REQUIRE_WASM=1` and runs `cargo test --all-targets`. Required
mode turns missing tools, mismatched versions, skipped compilation, failed
linking, invalid modules, shared memory, WebAssembly atomic instructions,
thread features, and native socket imports into hard failures.

The job validates Wasm but doesn't execute it under a WASI runtime. Native
Clang and GCC execution remains the behavioral oracle for the same portable
source.

## Failure behavior

Every matrix entry runs independently. A failed Windows gate doesn't cancel
Linux or macOS, and a native failure doesn't hide the pinned WebAssembly
result.

The workflow fails when any required command returns a nonzero status. It
doesn't use conditional success, continue-on-error, or optional Provider gates.

The workflow doesn't upload generated binaries. Compiler, test, and validator
output remains in the job log, which is sufficient for the Stage 6 acceptance
decision.

## Completion rule

Local Windows results establish fast feedback but don't complete Stage 6.
Stage 6 can be marked complete only after these four external jobs pass for the
same revision:

- Windows native matrix entry.
- Linux native matrix entry.
- macOS native matrix entry.
- Pinned `wasm32-wasi` entry.

If any job finds a genuine semantic or ABI defect, implementation changes must
return through the relevant focused gate and the complete matrix. CI-only
tool discovery or platform compiler fixes don't change RFC0003 unless they
expose a contract defect.

## Acceptance criteria

Task 13 is complete when all of these statements are true:

- One consolidated workflow defines the Stage 6 final matrix.
- The duplicate macOS workflow no longer exists.
- Windows, Linux, and macOS pass the complete native command set.
- IOCP, epoll, and kqueue pass real host conformance.
- Cross-provider transcripts and generated build systems pass on every host.
- Pinned WASI SDK 27 and `wasm-tools` 1.252.0 pass required mode.
- No Stage 0 through Stage 5 regression fails.
- Production Backend code contains no EventSource, plugin loader, timer, send,
  connect, accept, DNS, TLS, UDP, task pointer, or executor pointer.
- Stable and experimental Stage 6 artifact classes are recorded.
- The Stage 6 plan records the four external results before marking the stage
  complete.

## Next steps

After all four jobs pass, update the Stage 6 implementation plan with final
evidence and mark Stage 6 complete. Any later feature begins in a new approved
stage and must preserve Backend core v1 and net receive v1.
