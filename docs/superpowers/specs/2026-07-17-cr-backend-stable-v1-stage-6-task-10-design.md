# CR backend stable v1 design

This specification defines the stable Backend core and net-receive v1 boundary
for Stage 6 Task 10. It freezes the smallest append-only C prefixes and
observable semantics proven by the memory, IOCP, epoll, and kqueue providers.

The stable boundary lets a third party implement a compatible Provider from
the public headers. It doesn't expose CR task layout, Provider state, native
event records, or reference implementation internals.

## Goals

Task 10 converts the cross-provider evidence from Task 9 into a durable public
contract.

The stable v1 boundary must:

- Preserve core runtime ABI v3 and Waker v1 unchanged.
- Support third-party Backend Provider implementations.
- Keep every runtime-owned and compiler-owned object opaque.
- Freeze only fields used by every validated Provider model.
- Use `abi_version` and `struct_size` for append-only evolution.
- Keep portable control flow independent of native error values.
- Compile as portable C11 for native targets and `wasm32-wasi`.
- Preserve synchronous drop, cancellation, and quiescence.

Task 10 doesn't stabilize a reference Provider symbol, reference awaitable
layout, executor, reactor, timer, dynamic plugin loader, or additional network
operation.

## Compatibility classes

The public surface uses three explicit compatibility classes.

### Stable v1

Stable v1 includes observable semantics, identity constants, numeric portable
categories, public function contracts, fixed by-value records, and the declared
minimum prefix of each pointer-transported versioned record.

A compatible implementation must preserve every v1 field's offset, width,
order, and meaning. A compatible future version can append fields after a
pointer-transported v1 minimum prefix. A by-value or embedded record requires a
new type or outer ABI identity when its size changes.

### Opaque

Opaque types expose identity and ownership through pointers, not layout.

The opaque set contains:

- `cr_backend`.
- `cr_net_receive_operation`.
- Provider state.
- Provider operation internals.
- Task, executor, and Waker state.
- IOCP, epoll, and kqueue event records.
- Queue, lock, thread, batch, generation, and token state.

No public size, offset, field, or inline storage promise applies to an opaque
type.

### Experimental

Experimental components can evolve without preserving source or binary
compatibility.

The experimental set contains:

- Memory, IOCP, epoll, and kqueue reference Provider descriptor symbols.
- The reference net-receive awaitable and its state layout.
- Provider-specific test and diagnostic hooks.
- Future Backend extensions that don't have a separate approved RFC.

## Backend core v1

`cr_backend.h` defines the stable Backend core Provider contract.

### Identity and negotiation

The Backend core identity is a stable 128-bit extension identity. Extension
identity equality compares both 64-bit words.

A Provider and consumer negotiate compatibility through:

- A nonzero identity.
- `abi_version >= 1`.
- `struct_size` covering the required v1 minimum prefix.
- Non-null function slots required by the v1 contract.

An incompatible model must use a new identity. It can't reinterpret an
existing v1 prefix.

### Stable records

The following records have stable v1 layouts:

- `cr_extension_id`.
- `cr_storage_layout`.
- `cr_backend_error`.
- `cr_backend_pump_result`.
- `cr_backend_provider_desc`.
- `cr_backend_extension_desc`.

The v1 minimum-size macros define accepted pointer-transported prefixes. Future
compatible versions can append fields only where transport doesn't change a
function signature or shift later fields in an outer record. They can't
reorder, remove, narrow, or reuse a v1 field.

`cr_extension_id` is a fixed 16-byte by-value v1 type. Changing its size
requires a new API or ABI identity.

Unknown capability bits don't invalidate an otherwise compatible record. A
consumer must check a known bit before depending on its behavior.

### Stable operations

The Provider function table and public wrappers stabilize these operations:

- Create one Backend from a compatible Provider descriptor.
- Query one extension by identity and requested ABI version.
- Pump the owner thread with a relative timeout and event budget.
- Interrupt a live Backend from another thread.
- Shut down active work on the owner thread.
- Destroy a shut-down or idle Backend on the owner thread.

Create, query, pump, shutdown, and destroy are owner-thread operations.
Interrupt is the only cross-thread Backend core operation in v1.

Provider callbacks don't poll a task, resume a coroutine, or store task and
executor pointers.

### Pump contract

Pump reports one stable reason:

- `Progress` when it dispatches at least one completion or control event.
- `Timeout` when the relative timeout expires without progress.
- `Interrupted` when interrupt work ends the wait.
- `Error` when the pump operation fails.

`max_events` is nonzero and bounds records dispatched by one call. A Provider
can use any native wait batch internally. An interrupted wait preserves the
remaining relative timeout.

The stable record exposes the dispatched count and portable error category.
Native error domain and code are optional diagnostics.

## Net receive v1

`cr_net.h` defines one stable one-shot connected-socket receive extension.

### Stable records

The following records have stable v1 layouts:

- `cr_native_socket_handle`.
- `cr_net_error`.
- `cr_net_receive_completion`.
- `cr_net_extension_desc`.

The native socket handle is a fixed-size tagged by-value record. v1 recognizes
WinSock, POSIX file descriptor, and memory-conformance kinds. Changing its size
requires a new extension identity. The Provider never closes a borrowed socket.

The extension descriptor publishes the receive-operation storage layout and
the initialize, submit, cancel, quiesce, and destroy function slots.

### Operation storage

The caller queries `receive_operation_layout`, allocates storage with at least
the reported size and alignment, and keeps it alive through quiescence.

The storage is caller-owned and Provider-initialized. Its contents and layout
remain opaque. Initialization begins one generation. Reuse requires terminal
delivery, quiescence, and explicit reinitialization.

### Receive ownership

Initialize borrows these values for the operation generation:

- One connected native socket.
- One pinned writable buffer.
- One caller-owned operation storage region.
- One callback target and callback context.

The caller keeps every borrowed value valid until quiescence returns. The
Provider neither closes the socket nor frees the buffer or operation storage.

### Terminal completion

Every successful submit produces exactly one terminal callback with one kind:

- `Ready` with a transferred byte count, including zero-byte EOF.
- `Error` with a portable net error category.
- `Canceled` when cancellation wins the terminal race.

The callback borrows the completion record for the callback duration. A
consumer copies fields that it retains.

The native error domain and code are optional diagnostics. Portable control
flow depends only on terminal kind and portable category.

### Cancellation and quiescence

Cancel requests a terminal result but doesn't select the winner of a race with
data, EOF, or network failure. Repeated cancellation before terminal delivery
is idempotent.

Quiescence is the synchronous destruction fence. When it returns successfully:

- A submitted operation has delivered its one terminal callback.
- The Provider no longer references the socket, buffer, operation storage, or
  callback target for that generation.
- No later event can invoke the callback for that generation.
- The caller can explicitly reinitialize the operation storage.

Operation destroy is valid after quiescence. Backend shutdown synchronously
cancels and quiesces every active operation before it returns.

## Error contract

Backend and net errors use stable portable categories. A successful operation
reports the `None` category.

Native domains distinguish optional errno, WinSock, Win32, and memory-provider
diagnostics. A Provider can report no native diagnostic. Native numeric values
aren't portable and don't select cross-platform control flow.

Immediate submit failure leaves the operation quiescent and produces no
completion callback. A failure after successful submit is delivered through
the one terminal callback.

## Waker and awaitable relationship

Backend Provider operations are Waker-free. They publish terminal completion
through the receive callback on the owner thread.

The experimental reference awaitable copies the completion, publishes its own
readiness, and wakes a retained Waker. Its manual polling and Waker-aware
polling behavior remains governed by RFC0001 and RFC0002, but its state layout
and adapter implementation remain experimental.

Backend v1 doesn't introduce a continuation, task pointer, executor pointer,
or EventSource base type.

## Header and symbol policy

Stable declarations remain in `cr_backend.h` and `cr_net.h`. Each stable
version macro uses a v1 name without the `EXPERIMENTAL` qualifier.

Temporary experimental version names can remain as source-compatible aliases
during Stage 6. Stable tests and new code use the stable names.

The public headers label stable and experimental sections explicitly.
Reference Provider descriptor symbols remain declared only by private runtime
headers and don't become part of the stable public link ABI.

## Conformance

Conformance combines semantic execution and frozen-prefix checks.

The stable suite must verify:

- Exact stable identity and numeric category values.
- Record field order, offset, width, and minimum prefix size.
- Compatibility with records that append unknown trailing fields.
- Rejection of truncated records and missing required function slots.
- Native and `wasm32-wasi` C11 compilation.
- Byte stability of the checked-in stable header fixture.
- Absence of native event records and implementation fields from public
  headers.
- Task 9's identical memory, IOCP, epoll, and kqueue transcripts.

The frozen fixture records only stable declarations. Experimental comments,
reference Provider symbols, and private implementation sources don't affect
the stable golden comparison.

## RFC relationship

RFC0003 records the stable Backend core and net-receive contract defined here.
RFC0001 classifies Backend v1 as a stable extension while keeping core runtime
ABI v3 unchanged. RFC0002 remains the unchanged Waker contract.

An incompatible change requires a new approved RFC, new version or identity,
and a migration plan. Implementation refactoring that preserves this contract
doesn't require an ABI revision.

## Next steps

After this specification is approved, Task 10 creates RFC0003, updates RFC0001
and the Stage 6 design, marks stable constants and prefixes in the generated
headers, adds frozen fixtures, and runs native and WebAssembly compatibility
gates. Project packaging treats Backend v1 as stable only after RFC0003 receives
explicit user approval.
