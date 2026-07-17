# CR Backend generated-project integration design

This specification defines Stage 6 Task 11. It publishes the stable Backend v1
headers, selected reference Provider sources, and experimental reference
receive awaitable into generated projects without changing default Stage 5
output.

> **Note:** Reference Providers and the receive awaitable remain preview
> components under active development. Backend core v1 and net receive v1 are
> stable through RFC0003.

## Goals

Task 11 turns validated Backend selection into deterministic generated-project
artifacts and build dependencies.

The integration must:

- Keep `backends = []` byte-identical to Stage 5 output.
- Publish exactly the selected Provider family.
- Publish shared Backend files and the reference awaitable once.
- Link WinSock only for IOCP.
- Preserve existing POSIX thread dependency behavior for the threaded
  executor.
- Build through direct C, CMake, and Meson.
- Keep publication atomic and remove stale Provider files after a successful
  selection change.
- Preserve the previous complete distribution after validation or publication
  failure.
- Keep `native-net` unavailable for `wasm32-wasi` and unsupported targets.

Task 11 doesn't stabilize reference Provider symbols, add dynamic loading, add
new Backend capabilities, or change RFC0003.

## Planning model

Project artifact collection produces one deterministic plan before writing any
file. The plan contains artifacts and build dependencies.

### Artifacts

Every artifact records:

- Its relative output path.
- Its complete contents.
- Its manifest kind.
- Whether it participates in generated C compilation.

The collector inserts artifacts by logical layer:

1. Add ordinary generated translation units and runtime ABI files.
2. Add selected executor artifacts.
3. Add shared Backend core artifacts when any Backend is selected.
4. Add the experimental reference receive awaitable.
5. Add each selected Provider source in configuration order.
6. Deduplicate by normalized output path.
7. Reject one output path with different contents or metadata.
8. Generate build manifests and the artifact manifest from the final plan.

Deduplication preserves first insertion order. This keeps output deterministic
without sorting away the declared configuration order.

### Build dependencies

The compiler uses a private `BuildDependency` model. It doesn't expose this
type through Backend v1.

Task 11 defines two dependency capabilities:

- `PosixThreads` for the native threaded executor on POSIX hosts.
- `WinSock` for the Windows IOCP Provider.

Dependencies deduplicate in stable enum order. Artifact filenames don't infer
dependencies. The same selection plan drives CMake, Meson, and the artifact
manifest.

## Publication matrix

Backend selection determines the exact additional file family.

### Empty selection

`backends = []` publishes no Backend header, internal header, source, Provider,
or reference awaitable. It adds no Backend dependency and preserves Stage 5
output byte for byte.

### Memory conformance

`memory-conformance` publishes:

- `include/cr_backend.h`.
- `include/cr_net.h`.
- `runtime/cr_backend_internal.h`.
- `runtime/cr_backend_common.c`.
- `runtime/cr_backend_memory.c`.
- `runtime/cr_net_recv.c`.

The selection adds no native socket or Provider thread dependency.

### Native net

`native-net` publishes the shared headers, internal header, common source, and
reference awaitable. It publishes exactly one target Provider:

- IOCP for Windows MSVC and GNU targets.
- epoll for Linux GNU and musl targets.
- kqueue for macOS.

IOCP adds `WinSock`. epoll and kqueue add no Provider thread dependency.

### Combined selection

Selecting `memory-conformance` and `native-net` publishes both Provider sources
but only one copy of every shared artifact and the reference awaitable.

The existing configuration validator rejects duplicate selection of the same
Backend and rejects `native-net` for `wasm32-wasi` and custom unsupported
targets before publication.

## Manifest classes

The artifact manifest describes Backend outputs with explicit classes:

- `backend-header` for stable public headers.
- `backend-internal` for private implementation headers.
- `backend-source` for common and Provider sources.
- `backend-awaitable-source` for the experimental reference adapter.
- `build-manifest` for generated CMake and Meson dependency data.

The manifest records build dependencies using stable private serialization
names:

- `posix-threads`.
- `winsock`.

These names describe generated build requirements. They aren't public Backend
ABI constants.

## CMake integration

The generated CMake project continues to compile all C sources under
`crc/dist`. It optionally includes
`crc/dist/crc-generated-dependencies.cmake`, which is rendered from the same
dependency plan as Meson.

The generated fragment appends only planned link dependencies:

- `Threads::Threads` for the POSIX native-threaded executor.
- `ws2_32` for IOCP.

The root template links `CR_GENERATED_DEPENDENCIES` when the optional fragment
defines it. It doesn't inspect Provider filenames or link WinSock for empty or
memory-only projects. An empty dependency plan emits no CMake fragment, so the
default distribution gains no Stage 6 artifact.

## Meson integration

`crc/dist/meson.build` lists every generated source exactly once and constructs
`cr_generated_dependencies` from the dependency plan.

Meson maps:

- `PosixThreads` to `dependency('threads')`.
- `WinSock` to the C compiler's `find_library('ws2_32')` result.

An empty dependency plan emits `cr_generated_dependencies = []`.

## Atomic publication

Task 11 keeps the existing staging-directory transaction.

The compiler completes validation, artifact planning, dependency planning,
manifest generation, and staging writes before replacing `crc/dist`.

On success, whole-directory replacement removes stale Provider files. On
failure, the previous complete directory remains unchanged. A failed check
never writes publication artifacts.

Path conflicts, differing duplicate contents, invalid target selection, and
write failures abort the transaction.

## Incremental behavior

Backend selection already participates in the project fingerprint. Task 11
extends regression coverage to published contents.

Changing selection must:

- Trigger a rebuild.
- Publish the new exact Provider family.
- Remove files and dependencies from the old selection.
- Preserve unrelated generated translation units.
- Skip an identical subsequent build.

## Validation

The focused integration target validates generated projects, not only artifact
helper functions.

### Artifact validation

Tests assert exact file sets, manifest kinds, dependency names, source order,
shared-file deduplication, and absence of unselected Provider files.

### Build validation

Supported hosts build and execute selected projects through:

- A direct C compiler command.
- CMake configure and build.
- Meson setup and compile.

Windows validates IOCP and WinSock. Linux validates epoll. macOS validates
kqueue. Every native host validates memory conformance.

### Compatibility validation

The suite proves:

- Empty Backend selection retains Stage 5 bytes and dependencies.
- Core runtime ABI v3 and Waker v1 remain unchanged.
- Stable Backend v1 header digests remain unchanged.
- Reference awaitable publication doesn't make its layout stable.
- `wasm32-wasi` packages only portable memory artifacts.

### Failure validation

Tests publish one valid selection, attempt an invalid or unsupported selection,
and compare the complete prior distribution byte for byte. They then publish a
different valid selection and confirm that stale files disappear.

## Acceptance criteria

Task 11 is complete when all of these statements are true:

- Default projects pay no Stage 6 artifact or linkage cost.
- Each supported selection publishes its exact deterministic file family.
- Shared Backend and awaitable artifacts appear once.
- Build manifests and the artifact manifest agree on sources and dependencies.
- IOCP is the only Provider that adds WinSock.
- Provider selection changes are atomic and remove stale artifacts.
- Direct C, CMake, and Meson projects build and run on supported hosts.
- Stable ABI, differential, Waker, executor, and generated-project regressions
  pass.

## Next steps

After Task 11, Task 12 validates the published memory Provider, stable headers,
reference awaitable, and generated owning root as a pinned `wasm32-wasi`
module without native sockets, shared memory, or threads.
