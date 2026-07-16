# Tree-sitter CR compiler design

This document defines the long-term architecture for `crc`, a Rust source-to-
source compiler that extends C with coroutine and scope-exit syntax. The design
replaces the handwritten full-C parser with an incremental Tree-sitter front
end while retaining a compiler-owned semantic IR, CFG, lowering passes, and C
back ends.

## Goals

The compiler must preserve normal C behavior while giving CR extensions
well-defined control-flow semantics. It must also support fast development-mode
rebuilds and produce C that ordinary toolchains can compile.

- Accept C source with `__async`, `__await`, the compatibility spelling
  `__awite`, `__yield`, and `__defer`.
- Preserve comments, preprocessor constructs, and unaffected C source.
- Lower asynchronous functions to resumable state machines and lower
  synchronous functions that contain `__defer` without changing their calling
  convention.
- Lift only values that remain live across suspension points.
- Run deferred expressions in last-in, first-out order on real scope exits.
- Generate portable C by default and support GNU computed goto as an optional
  back end.
- Provide precise, source-based diagnostics for unsupported or unsafe control
  flow.
- Support project creation, builds, checks, incremental development mode, and
  cleanup through the CLI described in `Task.md`.

## Non-goals

The first production milestone does not reimplement a complete C type checker
or linker. Native C compilers remain the authority for full C type rules and
ABI validation.

- The CR semantic layer doesn't replace Clang, GCC, or MSVC diagnostics for
  ordinary C.
- The first milestone doesn't add scheduling primitives such as channels,
  `select`, or structured concurrency.
- Computed goto isn't required for portable output.
- Unaffected C functions aren't normalized or reformatted.

## Architecture

The compiler uses a lossless syntax layer for editing and a compact semantic
layer for transformations. This separation provides broad C syntax coverage
without coupling coroutine semantics to Tree-sitter node details.

```text
CR source
  -> Tree-sitter CR concrete syntax tree
  -> source map, scopes, names, and labels
  -> CR HIR
  -> control-flow graph
  -> scope-exit and defer lowering
  -> coroutine and suspension lowering
  -> liveness-based variable lifting
  -> CFG cleanup and validation
  -> portable C or GNU C back end
  -> optional native C compiler verification
```

Tree-sitter owns concrete syntax, incremental reparsing, error recovery, byte
ranges, and changed ranges. The CR compiler owns extension semantics, scope
rules, CFG construction, data-flow analysis, diagnostics, and generated code.

## Compiler interfaces

Migration introduces new interfaces instead of adapting the current
string-identified CFG in place. This keeps declaration identity, scope exits,
diagnostics, and incremental state explicit.

- `SyntaxUnit` owns source text, the Tree-sitter tree, parse diagnostics,
  dependency directives, and a grammar/configuration fingerprint.
- `HirUnit` uses typed `DeclarationId`, `ScopeId`, `LabelId`, and source spans.
- `CfgFunction` owns basic blocks and typed edges. Every edge records its source
  and destination scope stacks.
- `Suspend` and `Yield` are distinct terminators with typed continuation,
  await-slot, result-slot, and source information.
- `CompileResult` returns diagnostics, dependency paths, and a set of generated
  artifacts. Errors don't write partial artifacts.
- `Diagnostic` has severity, stable code, message, primary span, related spans,
  and optional fix text. `anyhow` remains for unexpected infrastructure errors,
  not user-facing compiler failures.

The fixed pass order is syntax validation, HIR, CFG, scope-exit/defer,
coroutine splitting, liveness/variable lifting, validation, optimization, and
C emission. The current semantic `goto_rewrite` pass is retired; computed goto
is exclusively a C backend choice. One resolved backend setting in codegen
configuration replaces the duplicate build and codegen booleans.

## Tree-sitter CR grammar

The parser is a maintained extension of the C grammar instead of a token-level
rewrite. CR constructs therefore appear as explicit syntax nodes with stable
source ranges.

- `__async` is a function specifier on function definitions and a task-binding
  specifier on local declarations initialized by an asynchronous call.
- `__await` and `__awite` produce the same await-expression node.
- `__yield` is an expression with an optional value.
- `__defer expression;` is a statement scoped to its containing compound
  statement.
- Standard C preprocessor nodes remain available and lossless.
- Error and missing nodes are retained so `crc dev` can diagnose incomplete
  edits without crashing.

The grammar package is vendored or pinned so builds don't depend on an
uncontrolled grammar update. Its Rust binding exposes the parser language and
queries used by the front end.

## Preprocessing and headers

Tree-sitter provides the lossless structural view, while a configured native C
preprocessor provides the active macro and include view used for semantic
checks. The compiler associates preprocessor line markers with original source
spans and never emits the expanded view as the user's output.

Line markers identify the originating file and line but don't provide a unique
token-origin graph. The compiler therefore treats the original CST as the only
source of transformed structure. Macro expansion can provide type, typedef,
constant, and dependency facts when one source node maps to one active-view
construct. A macro can't create multiple declarations, parameters, labels,
statements, scopes, or control-flow edges inside a transformed function. If an
expansion changes that structure or can't be associated uniquely with its
source invocation, the compiler reports a preprocessing-correlation diagnostic.

Macro definitions, include paths, forced includes, target defines, and the C
standard are part of the compilation fingerprint. Typedef classification and
declaration identity are resolved in the active view. If no supported native
preprocessor is configured, the compiler can still transform a translation
unit whose CR-bearing regions don't depend on conditional or macro-generated
structure; otherwise it reports that preprocessing is required.

CR headers use the `.hr` extension. A source header at
`crc/include/path/name.hr` produces `crc/dist/include/path/name.h`. Includes of
`.hr` files resolve through the CR include root and are rewritten to their
generated `.h` paths in transformed output. Ordinary `.h` files remain C
dependencies and aren't rewritten. Header generation precedes dependent source
generation, and transitive header dependencies participate in invalidation.

A public declaration `__async R work(T value);` produces an opaque task type
and create, poll, drop, result, yield, error, and awaitable declarations in the
generated `.h` file. Cross-translation-unit callers use `cr_work_create` and
`cr_work_destroy` because the concrete context layout belongs to the defining
translation unit. Calls within that defining unit can use caller-owned storage
and the internal init/drop entry points. A failed public create returns `NULL`
and writes a stable allocation error to caller-provided storage.

```c
typedef struct cr_work_task cr_work_task;
cr_work_task *cr_work_create(T value, cr_error *out_error);
cr_poll_status cr_work_poll(cr_work_task *task);
void cr_work_destroy(cr_work_task *task);
const R *cr_work_result(const cr_work_task *task);
const R *cr_work_yielded(const cr_work_task *task);
const cr_error *cr_work_error(const cr_work_task *task);
cr_awaitable cr_work_as_awaitable(cr_work_task *task);
cr_awaitable cr_work_into_awaitable(cr_work_task *task);
```

`out_error` can be `NULL`. On success, create clears a supplied error record.
On failure, it writes an error whose message remains valid for the program's
lifetime. This contract avoids hidden thread-local state and works in
freestanding or embedded runtimes that provide the configured allocator hooks.

The versioned runtime header is always generated at
`crc/dist/include/cr_runtime.h` and included as `"cr_runtime.h"`. Both CMake
and Meson add `crc/dist/include` to the generated include path.

## Source preservation

The compiler rewrites only functions and declarations that need CR lowering.
Ordinary source is copied from the original byte ranges so macros, comments,
formatting, and unknown implementation extensions survive unchanged.

At the translation-unit level, the emitter walks top-level nodes in source
order. It copies ordinary nodes verbatim and replaces every function that
contains CR syntax. Asynchronous functions receive generated declarations,
context structures, and polling functions. Synchronous functions that contain
only `__defer` keep their original signature and receive scope-exit lowering.
Generated support declarations are inserted at deterministic source
boundaries.

External generated symbols use the configured prefix plus the source-level
external function name, so `.hr` declarations and definitions agree across
translation units. Internal-linkage functions add a stable suffix derived from
the normalized project-relative source path and declaration identity. Helper
symbols use the same stable identity and never depend on traversal order. The
compiler diagnoses collisions with user declarations or two public CR symbols;
changing the configured prefix is the supported resolution.

The compiler parses one configured preprocessor view at a time. CR keywords
must be structurally present in the source CST; a macro can't generate or erase
a CR keyword, function boundary, defer statement, label, or suspension point.
Macros can remain inside ordinary expressions and declarations. Conditional
branches that contain CR syntax require a selected macro configuration and
must remain structurally valid in the lossless CST. Ambiguous branches produce
a diagnostic. The compiler never silently drops a preprocessor branch.

Only the active branch is semantically lowered for a build. An inactive branch
is preserved byte-for-byte, including CR syntax, because the configured C
preprocessor removes it before C parsing. The artifact manifest records the
macro fingerprint. A different configuration forces regeneration and lowers
the newly active branch. If branch boundaries can't be mapped without leaving
active CR syntax in C output, compilation fails before writing artifacts.

## Semantic front end

The semantic front end converts every function that contains CR syntax into
HIR. Expressions that don't affect CR data-flow can retain source-backed
representations until code generation.

Each HIR node records its source span and scope identifier. Function HIR owns
parameters, local declarations, labels, statements, and extension expressions.
Name resolution tracks variables separately from labels and members. It also
records declaration identity so shadowed variables don't share liveness data.

The front end validates these CR-specific rules:

- `__await` and `__yield` occur only inside `__async` functions.
- `__defer` occurs inside a compound statement.
- Labels are unique within a function.
- A jump obeys C rules for variably modified identifiers and CR rules for
  compiler-generated suspension continuations.
- Unsupported preprocessor ambiguity is reported before lowering.

The jump restriction is specific to transformed CR functions. Ordinary C
`goto` can enter a nested block or skip a non-variably-modified declaration,
including a `__defer` statement. A skipped defer isn't registered. The compiler
rejects jumps into the scope of a variably modified identifier, into an internal
suspension continuation, or through control flow whose cleanup stack can't be
represented by path-based dynamic registration. It reports the source and
destination scopes for every rejected edge. Unaffected C functions retain
native C behavior.

A transformed function also rejects suspension in an unevaluated context,
constant expression, variably modified type, `asm goto`, or a labels-as-values
expression. It rejects `setjmp` or `longjmp` when control could cross a
suspension or active defer. `volatile` and atomic operations remain explicit
side effects and are evaluated exactly once. Address-taken values are lifted
conservatively when an address can survive suspension. A `register` declaration
that requires lifting is rejected because generated context storage can't
preserve that qualifier's constraints.

Expression lowering preserves C sequencing and short-circuit behavior.
Suspension in `&&`, `||`, conditional, comma, call-argument, initializer, and
assignment expressions becomes explicit CFG blocks in source evaluation order.
If the compiler can't prove the required order for a compiler extension, it
reports that construct instead of changing its behavior.

## Control-flow graph

Every transformed function is lowered to a CFG. This includes asynchronous
functions and synchronous functions that contain `__defer`. Basic blocks
contain side-effecting instructions and end in exactly one terminator.

```text
Terminator = Goto | Branch | Switch | Suspend | Yield | Return | Unreachable
```

CFG construction assigns scope stacks to edges. `break`, `continue`, `return`,
and `goto` become explicit edges or terminators. This representation lets later
passes handle all exits consistently instead of duplicating behavior for each
statement form. Synchronous defer-only functions run scope-exit lowering and C
emission but skip coroutine state assignment and suspension liveness.

## Defer and scope-exit semantics

Deferred expressions belong to the lexical scope in which they appear. They
execute in last-in, first-out order whenever control really leaves that scope.
Suspension doesn't leave a scope and therefore doesn't run deferred actions.

`__defer` accepts a call expression. The function designator and each argument
are evaluated exactly once when execution reaches the defer statement. The
compiler stores those captured values in a typed cleanup record and calls a
generated cleanup thunk at scope exit. This registration-time capture matches
Go-style defer behavior and prevents later assignments from changing the
registered call.

Cleanup registration is dynamic. A defer in a conditional branch registers
only when that branch executes, and a defer in a loop registers once per
execution. Each transformed synchronous function owns a stack-local cleanup
stack. Each asynchronous context owns a cleanup stack that survives polling.
The stack can grow for repeated registrations and reports allocation failure
through the generated error path. This representation preserves nested and
same-scope LIFO order without unreliable static activation flags.

Portable defer capture requires a direct call whose function prototype and
argument types resolve in the active translation unit. Normal C argument
conversions happen at registration. Array and function arguments decay to
pointers. Scalar, pointer, and complete structure or union parameter values are
copied into a generated payload with fundamental C alignment. Qualifiers on
by-value parameters don't change the stored representation. Atomic lvalues are
read exactly once by their normal C value conversion.

Variadic calls, calls without a prototype, unresolved indirect calls,
incomplete value types, variably modified value payloads, and over-aligned or
compiler-specific calling conventions produce a diagnostic. A pointer capture,
including one derived from a compound literal or array, preserves only the
pointer value; the programmer remains responsible for the pointee lifetime.
Complete aggregate arguments are copied by value. These restrictions can be
expanded by later back ends without changing registration semantics.

The scope-exit pass computes the scopes exited by every CFG edge and inserts the
required cleanup instructions. It covers fallthrough, `break`, `continue`,
`return`, and legal outward `goto` edges. Cleanup obligations are dynamic, so a
jump before or after a defer statement is legal when ordinary C permits it; a
skipped statement registers nothing. An outward jump runs every registered
cleanup for the scopes it leaves. `switch` fallthrough doesn't exit the switch
scope; `break` does.

Cleanup insertion runs before coroutine state splitting. This ordering ensures
that a suspend/resume boundary preserves active defers while every final exit
runs them exactly once.

Dropping or canceling an asynchronous context runs all registered cleanups once
and marks the context canceled. Polling a canceled or completed context returns
its terminal status without rerunning cleanup. Abandoning a context without
calling its drop function is an API violation. C `longjmp`, process termination,
and thread termination bypass CR cleanup guarantees; transformed functions
diagnose `setjmp` and `longjmp` patterns that could cross an active cleanup.

Cleanup-stack growth in an asynchronous function stores a structured
out-of-memory error and follows the normal error path. A synchronous function
can't expose that status without changing its C signature, so allocation
failure calls the runtime's configured, non-returning `cr_oom_abort` hook. The
default hook aborts; embedded runtimes can replace it with another non-returning
policy.

## Coroutine lowering

Await and yield terminators split the CFG into resume regions. Each reachable
suspension continuation receives a stable state identifier.

For an await expression, generated code evaluates the awaited operation once,
stores its awaitable handle and result slot in the context, and polls that
handle. A pending poll stores the current state and returns pending. Resumption
polls the same handle again; it never recreates the operation. A ready poll
moves the value into the await expression, drops the awaitable handle, clears
the slot, and continues. An error poll records the error and takes the function
error path.

When an awaited operation yields, the poll callback writes its yielded value to
the same caller-provided value storage used for a ready value. The status tells
the caller which value was written. The parent copies that value to its own
yield slot and returns `CR_POLL_YIELDED` without advancing past the await. Yield
propagation requires matching value layouts and permission to yield. A mismatch
is a protocol error. Cancellation drops the child operation, cancels the parent
task, runs its cleanup stack, and returns `CR_POLL_CANCELED`.

Yield stores its optional value, stores the continuation state, and returns a
distinct yielded status. The next poll clears the yielded marker and resumes
the continuation. A yield value has the asynchronous function's declared
result type. `__yield;` is valid only for an asynchronous `void` function.

Normal completion stores the user result separately from poll status, runs
remaining cleanups, and marks the context ready. Invalid state values lead to
a deterministic error status rather than undefined control flow. Repeated
polling after ready, error, or cancellation returns the same terminal status.

## Variable lifting

Variable lifting uses backwards liveness over the lowered CFG. A declaration is
lifted when its value can be used after a suspension point reachable from its
definition.

Lifted fields use declaration identities, not source names, so shadowing is
safe. Parameters and temporaries follow the same analysis. Address-taken values
are conservatively lifted when their address can survive a suspension.

The generated context contains state, completion data, lifted parameters,
lifted locals, and any backend-required await temporaries. Values that don't
cross suspension remain ordinary C locals.

## C back ends

Both back ends consume the same lowered CFG and runtime ABI. Backend selection
therefore doesn't affect CR semantics.

The portable back end emits a `switch (ctx->state)` dispatcher and standard C
control flow. CR-generated output requires at least C11 and supports C11, C17,
and C23 targets. The GNU back end
may emit a computed-goto table when configuration and target capabilities allow
it. Configuration errors are reported instead of silently emitting a compiler
extension for an incompatible target.

Generated identifiers use the configured prefix and collision-safe internal
suffixes. The emitter includes source line directives only when configured.

## Runtime ABI

Templates and generated code share one versioned runtime contract. The runtime
header is treated as compiler-owned output rather than a separate example with
independent signatures.

Poll status never shares a value with the user's function result. The runtime
defines these stable states:

```c
typedef enum cr_poll_status {
    CR_POLL_PENDING,
    CR_POLL_YIELDED,
    CR_POLL_READY,
    CR_POLL_ERROR,
    CR_POLL_CANCELED
} cr_poll_status;
```

Every task and awaitable error uses a stable numeric code and optional static
message.

```c
typedef struct cr_error {
    int code;
    const char *message;
} cr_error;
```

An awaitable is a move-only, type-erased polling object. Its poll callback
writes a status-specific value into caller-provided storage for
`CR_POLL_READY` or `CR_POLL_YIELDED`. Value layout metadata lets generated code
validate the expected result type before polling. After an error poll, its error
callback returns the operation error before drop releases the operation.

```c
typedef struct cr_awaitable {
    void *state;
    cr_poll_status (*poll)(void *state, void *out_value);
    const cr_error *(*error)(void *state);
    void (*drop)(void *state);
    size_t value_size;
    size_t value_align;
    uint32_t flags;
} cr_awaitable;

#define CR_AWAITABLE_CAN_YIELD 0x1u
#define CR_AWAITABLE_OWNS_STATE 0x2u
```

The output storage must satisfy `value_size` and `value_align`. A `void`
operation uses zero for both fields and never writes output. Returning yielded
without `CR_AWAITABLE_CAN_YIELD`, returning a value with an incompatible
layout, or returning error without a valid error callback is a protocol error
with a compiler-defined fallback code. The parent copies an operation error
into its own context before dropping the child awaitable.

Awaitables aren't copied after ownership transfers into a task slot.
`cr_work_as_awaitable` creates a borrowed adapter: its drop callback drops the
task's active work but doesn't free task storage. The caller must retain the
storage and must not poll it independently while borrowed. The public
`cr_work_into_awaitable` consumes a heap task returned by create and sets
`CR_AWAITABLE_OWNS_STATE`; its drop callback destroys and frees that task.
Every terminal ready, error, or canceled path invokes drop exactly once and
clears the parent slot.

For a source function `__async R work(T value)`, the compiler emits a concrete
`cr_work_task` context and these entry points:

```c
void cr_work_init(cr_work_task *task, T value);
cr_poll_status cr_work_poll(cr_work_task *task);
void cr_work_drop(cr_work_task *task);
const R *cr_work_result(const cr_work_task *task);
const R *cr_work_yielded(const cr_work_task *task);
const cr_error *cr_work_error(const cr_work_task *task);
cr_awaitable cr_work_as_awaitable(cr_work_task *task);
cr_awaitable cr_work_into_awaitable(cr_work_task *task);
```

`void` functions omit value-returning accessors. `init` captures parameters and
sets the task to its entry state. `poll` owns all progress and writes result,
yield, or error fields in the context. Accessors are valid only for their
matching status. `drop` is idempotent, cancels an active task, drops an active
child awaitable, and runs registered defers. Context memory remains owned by
the caller; generated functions don't free the context itself.

CR local syntax `__async R task = work(args);` declares compiler-managed child
task storage and calls its initializer. `__await task` polls that child and
produces its `R` result. Direct `__await work(args)` allocates an equivalent
child-task slot in the parent context, initializes it once, and reuses it until
completion. Cross-translation-unit calls use create plus the owning adapter.
Other awaited operations must produce `cr_awaitable`; adapters such as timers
and socket operations use the same layout, status, and ownership contract.

The runtime header includes an ABI version constant. Generated C checks that
version at compile time. Project templates call only functions declared by the
same generated runtime header.

The initial runtime can remain a small reference implementation. Its purpose is
to compile and run generated examples; applications can later replace it with a
compatible scheduler.

## Incremental development mode

Development mode keeps parsed trees and generated output in memory by source
path. File changes are debounced, reparsed with the previous Tree-sitter tree,
and mapped through changed byte ranges.

For a file-content change, the compiler computes the smallest byte edit from
the previous and current buffers, converts its endpoints to Tree-sitter points,
applies `Tree::edit`, and reparses with the edited old tree. Multiple disjoint
editor edits can be supplied directly by a future LSP. Missing old text,
encoding changes, grammar changes, configuration changes, or inconsistent edit
ranges force a full reparse. Semantic lowering remains translation-unit
granular until dependency-safe function-level caching is proven.

The dependency graph tracks `.cr`, `.hr`, ordinary C headers, transitive
includes, macro configuration, compiler version, grammar version, runtime ABI,
and complete resolved configuration. It handles content changes, creation,
rename, and deletion. A change recompiles the affected translation unit and all
transitive dependents. A deleted or renamed source removes its stale generated
artifact only after the replacement build plan succeeds.

One build publishes all generated artifacts through a staging directory and
atomic same-volume renames. A failed build keeps the complete previous artifact
set. A no-op event doesn't rewrite files or change their timestamps.

## Diagnostics

Every compiler diagnostic contains a category, message, source file, byte
range, line and column, and optional related spans. The CLI renders concise
human-readable diagnostics and can later expose structured output for an LSP.

The parser reports Tree-sitter error and missing nodes. Semantic validation
reports illegal extension placement and scope-crossing jumps. Lowering reports
unsupported constructs only when they affect transformation safety. Native C
verification errors are remapped through generated source information when
possible.

## CLI behavior

The CLI remains the product entry point described by `Task.md`. Commands share
the same configuration loader and compiler pipeline.

- `crc create NAME` creates the documented CMake and Meson project layout.
- `crc build` generates `.c` and `.h` artifacts in `crc/dist`. An explicit
  configuration option can additionally invoke CMake or Meson; generation
  success isn't confused with a native build result.
- `crc check` performs parsing, CR validation, and optional native C checking
  without replacing successful outputs.
- `crc dev` performs an initial build, watches dependencies, and recompiles
  affected files.
- `crc clean`, `crc config`, and `crc init` operate on the same resolved project
  root and configuration.

Commands discover the project root by walking from `--root` or the current
directory to the nearest `crc.toml`. Explicit CLI values override environment
values, which override project configuration, which overrides defaults.
`create` refuses an existing nonempty target unless a future explicit force
option is supplied. `clean` resolves and verifies every path under the project
root before removal. Failed checks or builds return a nonzero exit code;
diagnostic-only warnings don't.

Artifact mapping is deterministic: `crc/src/a/b.cr` produces
`crc/dist/a/b.c`, and `crc/include/a/b.hr` produces
`crc/dist/include/a/b.h`. The manifest records generated inputs and outputs so
removed sources delete stale artifacts during the next successful build.

Templates must be tested as product inputs. The generated example must use
valid CR syntax, match the runtime ABI, and produce C accepted by an available
native compiler.

The repository has one compiler-owned template source. Existing inline CLI
templates and duplicate template directories are migrated to that source and
removed after equivalence tests pass. The template and runtime ABI carry the
same compiler version.

## Migration strategy

Migration proceeds through vertical slices so the repository always moves
toward the new architecture. The handwritten Chumsky parser isn't expanded
beyond work needed to remove it safely.

1. Add the pinned Tree-sitter language package, `SyntaxUnit`, diagnostics, and
   parser tests for C and CR extension nodes.
2. Add native preprocessor discovery, configuration fingerprints, line-marker
   correlation, inactive-branch preservation, and explicit diagnostics for
   structural macro expansions that can't map to the source CST.
3. Add new identity-based HIR and scoped CFG types beside the existing types.
   Build one Tree-sitter asynchronous function and one synchronous defer
   function through this vertical path.
4. Add source-preserving translation-unit emission and replace transformed
   synchronous and asynchronous functions through the new path.
5. Implement the versioned runtime ABI, dynamic cleanup stack, fixed pass
   order, coroutine polling, liveness, and both C back ends against executable
   fixtures.
6. Switch `Compiler::compile_source` only after the new path passes standard C,
   CR grammar, generated-C, runtime, and diagnostic tests. Switch watcher caches
   only after incremental edit and failure-recovery tests pass.
7. Remove the old parser AST, Chumsky dependency, string-identified CFG,
   semantic `goto_rewrite`, duplicate backend option, and old code generator
   only after searches and all-target builds prove they have no production
   callers.
8. Align `.hr` generation, runtime templates, all CLI commands, dependency
   fingerprints, project manifests, and native verification.

Existing HIR, CFG, pass, code-generation, configuration, watcher, and CLI code
is retained only where tests demonstrate that it satisfies this design.

## Testing strategy

Tests prove semantics at each boundary and cover the complete generated
artifact. Snapshot tests alone aren't sufficient because generated C must also
compile and, for runtime cases, execute correctly.

- Grammar tests cover standard C constructs, every CR extension, incomplete
  edits, preprocessor nodes and configurations, typedef ambiguity, CR headers,
  and source ranges.
- HIR and CFG tests cover scopes, labels, loops, branches, shadowing, and all
  exit kinds.
- Pass tests cover immediate-ready, pending, and error awaits; yielded values;
  repeated resume; nested, conditional, repeated, and synchronous defers;
  switch fallthrough; every goto direction; shadowing; address escape;
  liveness; and optimizer invariants.
- Backend tests cover portable switch and computed goto output.
- End-to-end tests compile representative `.cr` files, compile generated C
  with an available native compiler, and run deterministic examples.
- CLI tests create a temporary project and exercise create, build, check,
  clean, `.hr` generation, stale artifact deletion, and incremental no-op,
  header-change, configuration-change, rename, deletion, and failed-edit
  recovery behavior.
- ABI tests exercise layout mismatch, yielded-value propagation, borrowed and
  owning adapter drops, allocation failure, runtime version rejection, and
  repeated terminal polling.
- Cross-translation-unit tests compile a generated `.h`, its generated `.c`,
  and an ordinary C caller as separate translation units before linking and
  executing them.

CI declares its native matrix instead of relying on whichever tool happens to
be installed. The initial locked matrix is:

- Ubuntu 24.04 with GCC 14 and Clang 18 checks portable C11, C17, and C23.
- Ubuntu 24.04 with GCC 14 and Clang 18 checks computed-goto C11 and C17.
- Windows Server 2025 with MSVC 19.44 checks portable C11 and C17.
- CMake 3.28 or later builds templates on Ubuntu and Windows.
- Meson 1.3 or later builds templates on Ubuntu and Windows.

Exact patch releases, runner images, and container digests are locked in a
repository toolchain manifest and CI configuration. Updating that manifest is
an explicit maintenance change. Local development can report skipped external
tools, but release acceptance requires the complete locked matrix.

Rust verification requires formatting, compilation, tests, and Clippy with
warnings treated as errors. No compiler-warning suppression is introduced to
hide incomplete migration work.

## Acceptance criteria

The migration is complete only when evidence covers the entire `Task.md`
workflow. Passing a parser-only or Rust-only check isn't enough.

- The Rust project passes `cargo fmt --check`, `cargo check --all-targets`,
  `cargo test --all-targets`, and `cargo clippy --all-targets -- -D warnings`.
- CR extension fixtures produce the expected CFG and generated C.
- Generated portable C passes a native compiler syntax check.
- Runtime fixtures demonstrate suspension, resumption, yield, variable
  preservation, child yield/error/cancel propagation, layout validation,
  allocation failure, and exactly-once deferred cleanup.
- A two-translation-unit fixture uses a generated `.hr` public ABI, including
  create, poll, result or yield access, error access, and destroy.
- `crc create` produces the exact documented layout, and the generated example
  builds and runs through both CMake and Meson in release acceptance.
- `crc check` performs no artifact writes. A successful build removes stale
  artifacts and publishes a complete output set atomically.
- Development mode preserves timestamps on a no-op, invalidates transitive
  header and configuration dependencies, handles create, rename, and delete,
  and preserves the previous valid output after a failed edit.
- Generated code rejects an incompatible runtime ABI version at compile time.
- No active production path depends on the removed handwritten parser.
- The release matrix verifies portable output on Clang, GCC, and MSVC and
  computed-goto output on Clang and GCC.

## Next steps

Implementation starts with the Tree-sitter language boundary and executable
acceptance fixtures. Later passes are migrated only after their inputs and
outputs are covered by tests.
