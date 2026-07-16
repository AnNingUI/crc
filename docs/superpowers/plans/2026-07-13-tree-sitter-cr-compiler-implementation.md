# Tree-sitter CR compiler implementation plan

This plan migrates `crc` from the incomplete handwritten parser to the
Tree-sitter architecture defined in the approved design. Work proceeds through
tested vertical slices, and the existing production entry point changes only
after the replacement path proves equivalent or better.

## Phase 1: Grammar and syntax boundary

This phase creates the reproducible CR grammar and the Rust syntax facade.

1. Add pinned Tree-sitter Rust dependencies with Cargo.
2. Add a pnpm-managed grammar workspace based on `tree-sitter-c`.
3. Extend the C grammar with `__async`, `__await`, `__awite`, `__yield`, and
   `__defer` nodes.
4. Generate and vendor parser C sources, node types, and query files.
5. Add a Rust `syntax` module with `SyntaxUnit`, source spans, tree ownership,
   changed ranges, and structured parse diagnostics.
6. Test standard C, every CR extension, preprocessor nodes, incomplete edits,
   and incremental reparsing.

The phase gate requires grammar fixtures to parse without unexpected error
nodes and `cargo test --lib syntax` to pass.

## Phase 2: Active preprocessing view

This phase connects the lossless CST to the configured native C environment.

1. Discover or configure Clang, GCC, or MSVC preprocessing commands without
   coupling the compiler to one host toolchain.
2. Fingerprint include paths, forced includes, defines, target options, and the
   selected C standard.
3. Parse preprocessor line markers and correlate active declarations and
   typedef names with lossless source spans.
4. Preserve inactive branches while selecting exactly one semantic view.
5. Diagnose macro-generated CR structure, ambiguous conditional CR regions,
   structural expansions without a unique source-node correlation, and
   translation units that require an unavailable preprocessor.
6. Test active and inactive branches, macro-dependent typedef ambiguity,
   single-node type facts, macro-generated declarations and parameters,
   multi-token expansions, include failures, line remapping, and configuration
   changes.

The phase gate requires the same source to select different valid CR branches
under two macro configurations while diagnostics retain original source paths.

## Phase 3: Semantic identities and scoped CFG

This phase establishes replacement IR boundaries without mutating the old
string-identified CFG.

1. Add typed identifiers for declarations, scopes, labels, blocks, await slots,
   and result slots.
2. Convert transformed functions from Tree-sitter nodes into source-backed HIR.
3. Resolve local declarations, shadowing, labels, task bindings, and CR-specific
   placement rules.
4. Lower statements and sequenced expressions into a scoped CFG.
5. Record source and destination scope stacks on control-flow edges.
6. Test branches, loops, switch fallthrough, all jump kinds, short-circuit
   expressions, typedef names, and source diagnostics.

The phase gate requires one asynchronous function and one synchronous defer
function to reach the new CFG entirely through Tree-sitter-backed types.

## Phase 4: Scope exit and defer

This phase defines cleanup registration and scope-exit behavior independently
of the coroutine runtime ABI.

1. Validate portable defer call forms, C argument conversions, capture
   lifetimes, and supported complete value types.
2. Generate typed cleanup payloads and thunks.
3. Add stack-local cleanup storage for synchronous functions.
4. Represent asynchronous cleanup registration and scope exits in CFG without
   choosing a task-context ABI.
5. Insert cleanup execution for fallthrough, return, break, continue, and
   outward goto.
6. Enforce C variably modified object rules, path-based defer registration,
   unsupported call forms, and unsafe long-jump patterns.
7. Test nested, conditional, repeated, skipped, and exactly-once synchronous
   cleanup behavior plus asynchronous cleanup CFG placement.

The phase gate requires executable synchronous C fixtures to prove last-in,
first-out behavior on every supported exit path and IR tests proving that
suspension doesn't create a cleanup edge.

## Phase 5: Coroutine ABI and state machines

This phase implements the versioned polling contract from the design.

1. Generate task contexts, internal init/poll/drop entry points, and public
   opaque create/poll/destroy entry points.
2. Implement awaitable value layout metadata, move-only ownership, ready and
   yielded values, errors, cancellation, and protocol validation.
3. Split CFGs at await and yield continuations with stable state identifiers.
4. Add persistent asynchronous cleanup storage and integrate cleanup with
   cancellation, drop, terminal error, and allocation failure.
5. Preserve active child operations and cleanup stacks across pending polls.
6. Implement terminal-state idempotence and invalid-state errors.
7. Implement borrowed and owning task adapters plus explicit public create
   allocation errors.
8. Add a minimal portable C11 emitter for task contexts, switch dispatch, and
   runtime fixtures without liveness-based layout minimization.
9. Generate the versioned runtime header and reference runtime implementation.
10. Test immediate-ready, repeated-pending, yielded, ready, error, canceled,
   layout mismatch, allocation failure, adapter drops, and repeated-terminal
   polling.

The phase gate requires deterministic runtime fixtures to execute correctly
with both Clang and GCC locally.

## Phase 6: Liveness and C back ends

This phase minimizes context layouts and completes both C back ends from the
same lowered CFG.

1. Implement backwards liveness with declaration identities.
2. Lift parameters, locals, temporaries, and address-taken values that survive
   suspension.
3. Keep non-crossing values as C locals and reject incompatible `register`
   values.
4. Replace the Phase 5 conservative task layout with the analyzed portable C11
   layout while preserving the polling ABI.
5. Emit optional GNU computed goto from the same state representation.
6. Preserve unaffected translation-unit text and replace only transformed
   functions and declarations.
7. Compile generated fixtures with Clang and GCC and compare runtime results.

The phase gate requires valid portable C11 output, valid computed-goto output,
and context-layout tests for shadowing and address escape.

## Phase 7: Headers, CLI, templates, and incremental builds

This phase connects the new compiler pipeline to the product workflows in
`Task.md`.

1. Generate `.hr` declarations as deterministic `.h` artifacts.
2. Generate `cr_runtime.h` at the specified include path.
3. Define linkage-aware, path-stable symbol identities and collision
   diagnostics for public and internal declarations.
4. Add two-translation-unit public ABI compile, link, and execution tests.
5. Consolidate duplicate and inline templates into one versioned template set.
6. Make `create`, `build`, `check`, `dev`, `clean`, `config`, and `init` use the
   resolved project model.
7. Add artifact manifests, staging-directory publication, stale-output removal,
   and safe cleanup boundaries.
8. Add dependency fingerprints for CR/C headers, macros, configuration,
   grammar, compiler, and runtime versions.
9. Implement incremental Tree-sitter edits and transitive invalidation for
   content, create, rename, and delete events.

The phase gate requires temporary-project tests for both build systems and
failed-edit recovery that preserves the previous complete output set.

## Phase 8: Cutover and removal

This phase makes the new architecture authoritative and removes obsolete code.

1. Switch `Compiler::compile_source` after all new-pipeline acceptance tests
   pass.
2. Switch watcher caches and project builds to `CompileResult` artifacts.
3. Remove the Chumsky parser, old AST, old CFG, semantic `goto_rewrite`, old
   code generator, duplicate templates, and duplicate backend configuration.
4. Remove every unused dependency and production compatibility shim.
5. Run formatting, all-target checks, all-target tests, Clippy with warnings as
   errors, native compiler checks, two-translation-unit runtime executions,
   CMake, and Meson builds.

The final gate is the complete acceptance list in the approved design. No
legacy production path or warning suppression can remain.

## Verification commands

These commands form the local Rust and native verification baseline. CI adds
the pinned MSVC and operating-system matrix from the design.

```powershell
cargo fmt --check
cargo check --all-targets
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
clang -std=c11 -fsyntax-only <generated-file.c>
gcc -std=c11 -fsyntax-only <generated-file.c>
cmake -S <generated-project> -B <build-directory>
cmake --build <build-directory>
meson setup <build-directory> <generated-project>
meson compile -C <build-directory>
```

## Next steps

Start Phase 1 by adding pinned package-manager dependencies, generating the CR
grammar, and writing syntax-boundary tests before changing the compiler entry
point.
