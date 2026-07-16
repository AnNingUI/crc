# CR backend differential validation design

This specification defines Stage 6 Task 9. It compares the observable receive
lifecycle of the memory, IOCP, epoll, and kqueue providers without exposing or
stabilizing provider-specific implementation details.

> **Note:** This is a preview feature currently under active development.

The differential gate uses one canonical transcript for each scenario. Every
supported host runs its local provider and the portable memory provider against
the same expected transcript. A platform can therefore produce a complete
result without downloading logs from another CI job.

## Goals

Task 9 must prove that completion and readiness providers implement one common
receive lifecycle before Task 10 freezes any public semantic prefix.

The implementation must:

- Compare terminal kind, transferred bytes, portable error category, callback
  count, wake count, quiescence, reuse, and destruction safety.
- Run the memory provider on every supported native host.
- Run IOCP on Windows, epoll on Linux, and kqueue on macOS.
- Keep raw native handles, error codes, queue records, and readiness flags out
  of the canonical transcript.
- Avoid timing sleeps and nondeterministic race assertions.
- Keep `CR_RUNTIME_ABI_VERSION`, Waker v1, static await, and synchronous drop
  unchanged.
- Keep the backend and net declarations experimental until Task 10.

Task 9 does not introduce new backend operations, networking capabilities,
runtime threads, dynamic plugins, or a cross-job CI aggregation service.

## Architecture

The differential gate has three independent layers: a scenario fixture, a
provider adapter, and a Rust test driver.

### Scenario fixture

One shared C fixture defines the scenario identifiers, canonical event names,
and transcript writer. It observes only behavior available through the common
backend and net records.

Each transcript line uses this stable test-only shape:

```text
<scenario> <sequence> <event> <terminal> <bytes> <category> <count>
```

The transcript is a test protocol, not a public CR ABI. Task 9 can revise it
when a missing observable prevents an honest comparison.

### Provider adapters

Each provider adapter supplies only the platform actions needed to drive the
shared scenario:

- Create and destroy the backend.
- Create a connected receive source when the provider uses native sockets.
- Submit one receive operation.
- Publish deterministic data, EOF, error, cancel, interrupt, and stale-event
  conditions through existing fixture hooks.
- Pump until the scenario reaches its deterministic boundary.
- Quiesce and reinitialize the operation.

The memory adapter models the same boundaries without an operating-system
event source. Native adapters retain their private hooks and event records.

### Rust test driver

`tests/backend_differential.rs` exports the generated artifacts and shared
fixtures into a temporary directory, compiles the applicable provider adapter,
runs it, parses its transcript, and compares it with the canonical expectation.

The driver selects providers by host:

- Windows runs memory and IOCP.
- Linux runs memory and epoll.
- macOS runs memory and kqueue.
- Other hosts run memory only when a native C11 compiler is available.

Provider absence on its required host is a test failure. A provider from
another host is not simulated as acceptance evidence.

## Canonical scenarios

The first differential suite covers behaviors already implemented by every
provider.

### Successful receive

The provider receives known bytes and emits exactly one `Ready` terminal
callback. The transcript records the expected byte count, one wake, successful
quiescence, and no later callback.

### EOF receive

The peer closes its send side without payload. The provider emits one `Ready`
terminal callback with zero bytes. Native EOF flags remain private.

### Network error

The deterministic hook injects a receive failure. The provider emits one
`Error` terminal callback with the shared network category. Native error domain
and code don't participate in equality.

### Cancellation

The operation is canceled before data becomes terminal. The provider emits one
terminal outcome, quiesces, and never invokes the callback again. The
deterministic fixture selects `Canceled`; separate provider tests continue to
cover real race winners.

### Repeated cancellation

The fixture sends the cancellation request more than once before terminal
delivery. The transcript remains identical to one cancellation request.

### Interrupt and timeout

An interrupt wakes a blocked or waiting pump without creating a receive
completion. A zero-work timeout returns without a callback or wake. Provider
control-record details remain private.

### Stale event and reuse

The fixture retires one generation, reinitializes the same caller storage, and
injects or drains a stale record from the old generation. Only the new
generation can produce a terminal callback.

### Shutdown

Backend destruction cancels and quiesces the active operation before returning.
The transcript records one terminal callback and proves that no callback occurs
after storage release.

## Comparison rules

The test driver normalizes output before comparison.

It compares:

- Scenario and event ordering.
- Terminal kind.
- Transferred byte count for `Ready`.
- Portable category for `Error`.
- Callback and wake counts.
- Quiescent, reusable, and destroyed boundaries.

It ignores:

- Native error domain and native error code.
- Socket, queue, event, and operation addresses.
- IOCP completion keys and `OVERLAPPED` fields.
- epoll event masks and generation-token representation.
- kqueue filters, flags, `fflags`, and `udata` representation.
- Internal pump batch size and the number of native wait calls.

Every transcript must be complete, contain known events only, and terminate at
the expected destruction boundary. Extra callbacks, wakes, or terminal records
fail the test even when the expected prefix matches.

## Failure handling

A differential mismatch is classified before code changes are made.

1. Fix the provider when its behavior violates the approved Stage 6 contract.
2. Fix the fixture when it observes a provider-private detail or introduces
   nondeterminism.
3. Change the experimental common declaration only when all provider models
   prove that the existing contract can't represent an honest shared behavior.
4. Stop Task 9 when a correction requires changing core ABI v3, Waker v1,
   synchronous drop, or ownership rules.

Task 9 must not hide mismatches with platform-specific golden transcripts.

## CI execution

Each supported operating system runs the same Rust integration target:

```text
cargo test --test backend_differential -- --nocapture
```

The repository keeps one canonical expectation per scenario. CI jobs don't
upload transcripts for later comparison. On failure, the test prints the
provider name, scenario, expected transcript, and actual transcript.

The existing macOS workflow adds this focused gate after the kqueue gate.
Windows and Linux workflows must run the same target when they are added or
extended for Stage 6 final validation.

## Acceptance criteria

Task 9 is complete when all of the following statements are true:

- Memory matches the canonical transcript on every available native host.
- IOCP matches it on a real Windows host.
- epoll matches it on a real Linux host.
- kqueue matches it on a real macOS host.
- Every successful submit produces exactly one terminal callback.
- Quiescence prevents later access and callbacks for the retired generation.
- Reuse rejects stale provider records.
- Raw provider event details remain absent from the shared transcript and
  public common prefix.
- No stable ABI declaration is introduced before Task 10.

## Next steps

After this specification is approved, implementation creates the shared
fixture, provider adapters, Rust differential driver, and focused CI gates.
Task 10 then freezes only the semantic prefix supported by the resulting
cross-provider evidence.
