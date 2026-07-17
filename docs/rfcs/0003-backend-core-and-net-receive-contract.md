# RFC0003: Backend core and net-receive contract

This RFC defines the stable CR Backend core v1 and one-shot net-receive v1
extension. The contract supports third-party Providers while keeping Backend
instances, receive operations, tasks, executors, and native events opaque.

> **Note:** This contract was accepted on July 17, 2026, after memory, IOCP,
> epoll, and kqueue passed the same differential lifecycle suite.

This RFC extends
[RFC0001](0001-core-coroutine-contract.md) and composes with
[RFC0002](0002-waker-contract.md). It doesn't change core runtime ABI v3,
Waker v1, generated task layout, or synchronous drop.

## Contract boundary

Backend v1 stabilizes observable behavior and append-only public record
prefixes. It doesn't stabilize one Provider implementation or native event
model.

The stable contract covers:

- Backend identity, creation, extension query, pumping, interruption,
  shutdown, and destruction.
- Provider and extension version negotiation.
- Portable Backend and net error categories.
- Receive initialization, submission, cancellation, terminal callback,
  quiescence, reuse, and destruction.
- Borrowed socket, buffer, callback, and operation-storage ownership.
- Append-only v1 prefixes declared by `cr_backend.h` and `cr_net.h`.

Backend and receive-operation layouts remain opaque. Reference Provider
symbols, native event records, the reference receive awaitable, executors,
reactors, timers, plugins, and other I/O operations remain experimental.

## ABI identity and evolution

The stable version constants are:

```c
#define CR_BACKEND_ABI_VERSION 1u
#define CR_NET_ABI_VERSION 1u
```

The Stage 6 experimental names remain source-compatible aliases. New code uses
the stable names.

Backend core and net receive use distinct stable 128-bit identities. A
compatible descriptor has a matching valid identity, sufficient ABI version,
`struct_size` covering the v1 prefix, and every required v1 function slot.

Unknown trailing fields and capability bits don't invalidate a compatible
descriptor. A consumer checks a known bit before depending on it.

A compatible version can append fields after a pointer-transported v1 prefix
when doing so doesn't shift an embedded outer record. It can't reorder, remove,
narrow, reuse, or change a v1 field. Fixed by-value records require a new type,
function signature, or identity when their size changes.

## Stable Backend records

`cr_backend.h` gives these records stable v1 layouts:

- `cr_extension_id`.
- `cr_storage_layout`.
- `cr_backend_error`.
- `cr_backend_pump_result`.
- `cr_backend_provider_desc`.
- `cr_backend_extension_desc`.

The `*_V1_MIN_SIZE` macros define pointer-transported frozen prefixes.
Provider-private data stays after a compatible public prefix or behind an
opaque pointer.

`cr_extension_id` is a fixed 16-byte by-value v1 type. Its size can't change
under the existing function signatures.

`cr_storage_layout` reports a nonzero size and power-of-two alignment. It
describes caller-owned opaque storage without exposing fields.

## Backend ownership

A compatible Provider descriptor has module lifetime. `cr_backend_create`
creates one opaque Backend and binds it to that Provider.

Create, extension query, pump, Provider extension operations, shutdown, and
destroy run on the Backend owner thread. Wrong-thread use fails with the
portable wrong-thread category and performs no partial action.

`cr_backend_interrupt` is the only cross-thread Backend core v1 operation. The
caller keeps the Backend alive until every possible interrupt call returns.
Shutdown and destruction don't race new interrupt calls.

A Provider callback publishes completion on the owner thread. It never polls
or resumes a task and never stores a task or executor pointer.

## Pump contract

Pump accepts a relative nanosecond timeout and nonzero `max_events` budget. One
call reports `Progress`, `Timeout`, `Interrupted`, or `Error`.

`events_dispatched` never exceeds `max_events`. A Provider can use any private
native batch size. An interrupted native wait preserves the remaining relative
timeout.

Pump errors report a portable category. Native error domain and code are
optional diagnostics and don't select portable control flow.

## Net-receive extension

`cr_net.h` defines one stable one-shot receive over an already-connected
borrowed socket.

The stable records are:

- `cr_native_socket_handle`.
- `cr_net_error`.
- `cr_net_receive_completion`.
- `cr_net_extension_desc`.

The extension descriptor begins with the common prefix, publishes the opaque
operation-storage layout, and provides initialize, submit, cancel, quiesce,
and destroy slots.

The native socket handle is a fixed-size tagged by-value record. v1 defines
WinSock, POSIX file descriptor, and memory-conformance kinds. Its size can't
change under the existing extension identity. A Provider doesn't close it.

## Receive ownership

The caller allocates operation storage with at least the reported size and
alignment. Its contents remain opaque.

Initialize borrows one connected socket, one pinned writable buffer, one
operation-storage region, and one callback target for a generation. The caller
keeps them valid until quiescence returns. The Provider doesn't free them.

Reuse requires terminal delivery, successful quiescence, and explicit
reinitialization. Destroyed storage can't be revived as the same generation.

## Submit and terminal completion

Submit succeeds at most once per initialized generation. Immediate submit
failure leaves the operation quiescent, reports a portable error, and produces
no completion callback.

Every successful submit produces exactly one terminal callback:

- `CR_NET_RECEIVE_READY` reports transferred bytes, including zero-byte EOF.
- `CR_NET_RECEIVE_ERROR` reports a portable net error category.
- `CR_NET_RECEIVE_CANCELED` reports that cancellation won the race.

The completion record is borrowed for the callback duration. A consumer copies
fields it retains. Native diagnostics don't select portable control flow.

## Cancellation

Cancel requests a terminal outcome. It doesn't choose the winner of a race
with data, EOF, or network failure. Repeated cancellation before terminal
delivery is idempotent.

A Provider delivers at most one terminal callback, never revives a terminal
generation, and doesn't access the buffer after quiescence. Cancellation
doesn't allocate or add asynchronous drop.

## Quiescence and shutdown

Quiescence is the synchronous destruction fence. On success, a submitted
generation has delivered its terminal callback, the Provider retains no
borrowed operation resource, and no later callback can occur for that
generation.

Quiescence can dispatch unrelated completions needed to drain a native queue.
It doesn't poll a task.

Backend shutdown synchronously cancels and quiesces every active operation.
Backend destruction releases Provider-owned allocations and native handles but
doesn't close borrowed sockets.

## Error contract

Backend and net operations use stable portable categories. Success reports the
`None` category.

Native domains can identify errno, WinSock, Win32, or memory diagnostics.
Native values vary by platform and Provider. Portable consumers don't branch
on them.

An error can't partially submit an operation, consume caller storage, produce
multiple callbacks, or bypass quiescence.

## Waker relationship

Backend operations are Waker-free. A receive callback publishes terminal
completion on the owner thread.

The experimental reference awaitable copies completion, publishes readiness,
and wakes its retained Waker according to RFC0002. Manual null-context polling
remains valid.

Backend v1 doesn't add a Waker field, task pointer, executor pointer,
continuation, or EventSource base.

## Provider evidence

Memory validates deterministic native and `wasm32-wasi` conformance. IOCP
validates completion. epoll and kqueue independently validate readiness.

The Task 9 differential suite proves identical portable transcripts for
success, EOF, network error, repeated cancellation, timeout, interrupt,
quiescence, reuse, and shutdown on Windows, Linux, and macOS.

Raw completion packets, readiness flags, event tokens, native errors, and wait
batch details remain outside the stable transcript and public prefix.

## Conformance

A conforming implementation passes these gates:

- Stable identity and numeric category checks.
- Frozen byte digests for the public v1 regions.
- Field order, offset, width, and minimum-size checks.
- Unknown tail acceptance and truncated prefix rejection.
- Native C11 and pinned `wasm32-wasi` compilation.
- Exactly-once callback, cancellation, quiescence, reuse, and shutdown tests.
- Differential execution on Windows, Linux, and macOS.
- Public-header audits that reject native and runtime-object details.

Unit tests of private state don't replace supported-host execution.

## Compatibility classification

Backend core v1 and net receive v1 are stable extension ABIs. Reference
Providers and the reference receive awaitable remain experimental.

Future ABI work can't weaken this contract without a new approved RFC.
Implementation state machines, allocation strategies, queues, and native APIs
can change when stable behavior and prefixes remain unchanged.

## Next steps

Stage 6 publishes stable headers with selected experimental reference Provider
sources, validates memory as WebAssembly, and runs final native, ABI, race,
build-system, and portability gates.
