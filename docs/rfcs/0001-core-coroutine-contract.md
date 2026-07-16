# RFC0001: Core coroutine contract

This RFC defines the observable coroutine behavior that CR preserves across
compiler and runtime evolution. It stabilizes semantics, not generated task or
context layouts.

> **Note:** Core ABI v3 is the implemented runtime boundary. CR doesn't provide
> an ABI v2 compatibility path. The stable Waker extension contract is defined
> by [RFC0002](0002-waker-contract.md); its implementation is pending.
> Reference executor, reactor, and backend APIs remain experimental.

## Contract boundary

CR keeps public behavior separate from compiler-private representation. The
following behavior is stable across the ABI migration:

- Poll status transitions.
- Result, yield, and error accessor validity.
- Cancellation and synchronous drop behavior.
- Child and defer cleanup ordering.
- Borrowed and owning adapter semantics.

Task fields, resume-state numbers, child slots, cleanup records, and CIR types
remain compiler-private. The public dynamic awaitable contains only opaque
state and a versioned vtable pointer.

## Poll lifecycle

A task starts in a nonterminal state and reaches one of three sticky terminal
states. Its observable lifecycle is:

```text
Created -> Pending* <-> Yielded -> Ready
                     \-> Error
                     \-> Canceled
```

The lifecycle follows these rules:

- `CR_POLL_PENDING` can occur any number of times.
- `CR_POLL_YIELDED` is transient and exposes the current yielded value.
- The next poll after yielded resumes task execution.
- `CR_POLL_READY`, `CR_POLL_ERROR`, and `CR_POLL_CANCELED` are terminal.
- A sequential poll after a terminal status returns the same status without
  executing user work or cleanup again.
- An error or cancellation can't transition back to pending.
- A task can't be polled concurrently or reentrantly.

Concurrent or reentrant poll is an unchecked caller contract violation. CR
doesn't require a release task to store a concurrency guard or recover from
that misuse.

## Accessors

Each accessor has a status precondition. Calling an accessor outside its valid
status violates the caller contract.

- The result accessor is valid only after `CR_POLL_READY`.
- The yielded accessor is valid only while the last poll returned
  `CR_POLL_YIELDED`.
- The error accessor is valid only after `CR_POLL_ERROR`.
- `void` tasks expose no result or yielded accessor.

An error record returned through the stable ABI contains a numeric code and an
optional immutable UTF-8 message. ABI-provided messages have module lifetime,
and a parent copies a child error before it drops the child.

## Drop and cancellation

Task drop is synchronous, can't suspend, and is idempotent while storage remains
valid. Public destroy finalizes and frees a heap task and can run only once.

Dropping an active task performs these actions:

1. Finalize the child at the current suspension point, if present.
2. Run declaration-owned child cleanups and `__defer` callbacks in reverse
   dynamic-activation order.
3. Mark retained task storage canceled.
4. Return without scheduling asynchronous cleanup.

Dropping a ready or error task doesn't replace its terminal status. Polling or
accessing a task after public destroy is invalid because its storage is gone.

`__defer` must call synchronous code and can't suspend. A resource that needs
asynchronous shutdown requires explicit awaited control flow before hard drop.

## Child ownership

Every active child has one owner. The owner finalizes it exactly once on its
defined terminal or scope-exit path.

A direct await owns one child generation at that await edge. It retains the
child across pending polls and finalizes it at terminal completion. Reentering
the edge later creates a new generation.

An `__async` task binding owns one declaration-scoped child generation. Awaiting
the binding doesn't transfer ownership. Multiple awaits observe the same child,
including its sticky terminal result. Scope exit finalizes the activated
binding exactly once.

## Adapter ownership

A dynamic awaitable is move-only after it transfers into a parent slot. ABI v3
exposes borrowed and owning generated adapters.

- `*_as_awaitable` borrows task storage. Its drop behavior finalizes active work
  but doesn't free the caller's task allocation.
- `*_into_awaitable` consumes a heap task returned by create. Its drop behavior
  finalizes and frees the task.
- The parent calls the selected dynamic drop callback at most once.
- The source awaitable can't be copied, polled, or dropped independently after
  transfer.

## Compatibility classification

The architecture uses these compatibility classes:

- **Implemented core contract:** lifecycle, accessor validity, ownership,
  cancellation, cleanup behavior, fixed-width poll status, nullable poll
  context, and the versioned dynamic-await vtable.
- **Accepted stable extension contract:** Waker v1 ownership and wake behavior
  defined by [RFC0002](0002-waker-contract.md). Its Stage 5 implementation is
  pending.
- **Implemented compiler extension:** typed static dispatch remains governed by
  the core lifecycle and ownership rules in this RFC.
- **Experimental extensions:** reference executor, reactor, backend SPI,
  generator integration, and stronger dynamic type identity until their
  separate gates pass.

Future ABI work can't weaken the stable semantics in this RFC without a new
approved RFC. Append-only public structures must use their version and minimum
prefix contracts.

## Conformance

Executable generated-C tests define conformance. They must verify repeated
pending and terminal polls, transient yield, ready values, propagated error,
cancellation, exactly-once child drop, exactly-once defer execution, and both
adapter ownership modes.

The conformance suite must run through the public compiler entry point. Unit
tests of internal state structures don't replace generated-C execution.

## Next steps

Stages 2 through 4 completed core ABI v3, typed static await, and coroutine CFG
optimization without changing this semantic contract. Stage 5 implements the
separate Waker contract in [RFC0002](0002-waker-contract.md) before adding
experimental reference executor behavior.
