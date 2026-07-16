use std::fs;
use std::path::Path;
use std::process::Command;

use crc_lib::Compiler;
use crc_lib::config::Config;
use crc_lib::runtime_abi::runtime_header;

fn available_compiler() -> &'static str {
    ["clang", "gcc"]
        .into_iter()
        .find(|compiler| {
            Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("Clang or GCC is required for static-await tests")
}

fn compile_and_run(source: &str) -> String {
    compile_and_run_with_options(source, &[], &[])
}

fn compile_and_run_with_options(
    source: &str,
    compiler_args: &[&str],
    support_files: &[(&str, &str)],
) -> String {
    let generated = Compiler::new(Config::default())
        .compile_source(source, Path::new("static-await.cr"))
        .expect("static-await source compiles");
    let directory = tempfile::tempdir().expect("temporary directory");
    fs::write(directory.path().join("cr_runtime.h"), runtime_header())
        .expect("runtime header is written");
    fs::write(directory.path().join("static-await.c"), &generated)
        .expect("generated source is written");
    for (name, contents) in support_files {
        fs::write(directory.path().join(name), contents).expect("support file is written");
    }
    let executable = if cfg!(windows) {
        "static-await.exe"
    } else {
        "static-await"
    };
    let mut command = Command::new(available_compiler());
    command.args(["-std=c11", "-Wall", "-Wextra", "-Werror"]);
    command.args(compiler_args);
    let compilation = command
        .args(["static-await.c", "-o"])
        .arg(executable)
        .current_dir(directory.path())
        .output()
        .expect("native compiler runs");
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    let execution = Command::new(directory.path().join(executable))
        .current_dir(directory.path())
        .output()
        .expect("generated executable runs");
    assert!(
        execution.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    generated
}

#[test]
fn two_phase_layouts_support_a_later_defined_static_child() {
    let generated = compile_and_run(
        r#"
#include <assert.h>

static int marker_before;

__async int parent(int value) {
    return __await child(value);
}

static int marker_between;

__async int child(int value) {
    __yield value;
    return value + 1;
}

int main(void) {
    cr_parent_task task;
    cr_parent_init(&task, 7);
    assert(cr_parent_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_parent_yielded(&task) == 7);
    assert(cr_parent_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_parent_result(&task) == 8);
    cr_parent_drop(&task);
    return marker_before + marker_between;
}
"#,
    );

    let child_layout = generated
        .find("struct cr_child_task {")
        .expect("child layout is emitted");
    let parent_layout = generated
        .find("struct cr_parent_task {")
        .expect("parent layout is emitted");
    let child_create_prototype = generated
        .find("cr_child_task *cr_child_create(int cr_arg_0, cr_error *out_error);")
        .expect("child create prototype is emitted");
    let parent_poll = generated
        .find("cr_poll_status cr_parent_poll")
        .expect("parent body is emitted");
    let marker_before = generated
        .find("static int marker_before")
        .expect("first marker is preserved");
    let marker_between = generated
        .find("static int marker_between")
        .expect("second marker is preserved");
    let parent_drop = generated
        .find("void cr_parent_drop")
        .expect("parent drop is emitted");
    let parent_poll_body = &generated[parent_poll..parent_drop];

    assert!(child_create_prototype < child_layout);
    assert!(child_layout < parent_layout);
    assert!(parent_layout < parent_poll);
    assert!(marker_before < marker_between);
    assert_eq!(generated.matches("struct cr_child_task {").count(), 1);
    assert_eq!(generated.matches("struct cr_parent_task {").count(), 1);
    assert!(generated[parent_layout..parent_poll].contains("cr_child_task cr_child_0;"));
    assert!(parent_poll_body.contains("cr_child_init(&ctx->cr_child_0"));
    assert!(parent_poll_body.contains("cr_child_poll(&ctx->cr_child_0, poll_context)"));
    assert!(!parent_poll_body.contains("cr_child_create("));
    assert!(!parent_poll_body.contains("into_awaitable"));
    assert!(!parent_poll_body.contains(".vtable"));
    assert!(!parent_poll_body.contains("malloc("));
}

#[test]
fn internal_static_child_uses_one_qualified_stem_everywhere() {
    let generated = compile_and_run(
        r#"
#include <assert.h>

__async int parent(int value) {
    return __await local(value);
}

static __async int local(int value) {
    return value + 2;
}

int main(void) {
    cr_parent_task task;
    cr_parent_init(&task, 40);
    assert(cr_parent_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_parent_result(&task) == 42);
    cr_parent_drop(&task);
    return 0;
}
"#,
    );

    let stem = "cr_static_await_cr_local";
    assert!(generated.contains(&format!(
        "static cr_poll_status {stem}_poll({stem}_task *task, const cr_poll_context *poll_context);"
    )));
    assert!(generated.contains(&format!("{stem}_init(&ctx->cr_child_0")));
    assert!(generated.contains(&format!("{stem}_poll(&ctx->cr_child_0, poll_context)")));
    assert!(generated.contains(&format!("cr_poll_status {stem}_poll(")));
    assert!(!generated.contains(&format!("{stem}_create(")));
    assert!(!generated.contains(&format!("{stem}_into_awaitable(")));
}

#[test]
fn moved_layouts_preserve_macro_and_conditional_body_positions() {
    let generated = compile_and_run(
        r#"
#include <assert.h>

__async int parent(int value) {
    return __await child(value);
}

#define CHILD_BIAS 3
#if 1
static int conditional_marker;
#endif

__async int child(int value) {
    return value + CHILD_BIAS;
}

int main(void) {
    cr_parent_task task;
    cr_parent_init(&task, 39);
    assert(cr_parent_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_parent_result(&task) == 42);
    cr_parent_drop(&task);
    return conditional_marker;
}
"#,
    );

    let child_layout = generated
        .find("struct cr_child_task {")
        .expect("child layout is emitted");
    let define = generated
        .find("#define CHILD_BIAS 3")
        .expect("macro remains in source");
    let child_body = generated
        .find(
            "cr_poll_status cr_child_poll(cr_child_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("child body is emitted");
    let parent_body = generated
        .find("cr_poll_status cr_parent_poll(cr_parent_task *ctx, const cr_poll_context *poll_context) {")
        .expect("parent body is emitted");
    let parent_drop = generated
        .find("void cr_parent_drop(cr_parent_task *ctx) {")
        .expect("parent drop is emitted");
    let conditional_end = define
        + generated[define..]
            .find("#endif")
            .expect("conditional remains");

    assert!(define < conditional_end);
    assert!(conditional_end < child_layout);
    assert!(child_layout < child_body);
    let parent_poll = &generated[parent_body..parent_drop];
    assert!(parent_poll.contains("cr_child_create("));
    assert!(parent_poll.contains("cr_child_poll(ctx->cr_boxed_"));
    assert!(parent_poll.contains("cr_child_destroy(ctx->cr_boxed_"));
    assert!(!parent_poll.contains("into_awaitable"));
    assert!(!parent_poll.contains(".vtable"));
}

#[test]
fn embedded_direct_arguments_execute_once_in_source_order() {
    let generated = compile_and_run(
        r#"
#include <assert.h>

static int order;

static int next_value(int expected) {
    assert(order == expected);
    order += 1;
    return order;
}

__async int child(int first, int second) {
    return first * 10 + second;
}

__async int parent(void) {
    return __await child(next_value(0), next_value(1));
}

int main(void) {
    cr_parent_task task;
    cr_parent_init(&task);
    assert(cr_parent_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_parent_result(&task) == 12);
    assert(order == 2);
    cr_parent_drop(&task);
    return 0;
}
"#,
    );

    let parent_poll = generated
        .find("cr_poll_status cr_parent_poll")
        .expect("parent poll is emitted");
    let parent_drop = generated
        .find("void cr_parent_drop")
        .expect("parent drop is emitted");
    let body = &generated[parent_poll..parent_drop];

    let first = body
        .find("cr_child_0_arg_0 = next_value(0);")
        .expect("first argument is materialized");
    let second = body
        .find("cr_child_0_arg_1 = next_value(1);")
        .expect("second argument is materialized");
    assert!(first < second);
    assert_eq!(body.matches("next_value(0)").count(), 1);
    assert_eq!(body.matches("next_value(1)").count(), 1);
}

#[test]
fn embedded_direct_finalizes_once_across_terminal_and_parent_drop_paths() {
    compile_and_run(
        r#"
#include <assert.h>
#include <stdlib.h>

typedef struct operation_state {
    int polls;
    int mode;
    cr_error error;
} operation_state;

static int operation_drops;

static cr_poll_status operation_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    (void)poll_context;
    operation_state *state = (operation_state *)raw;
    state->polls += 1;
    if (state->polls == 1) return CR_POLL_PENDING;
    if (state->mode == 1) return CR_POLL_ERROR;
    if (state->mode == 2) return CR_POLL_CANCELED;
    *(int *)out_value = 42;
    return CR_POLL_READY;
}

static const cr_error *operation_error(const void *raw) {
    return &((const operation_state *)raw)->error;
}

static void operation_drop(void *raw) {
    operation_drops += 1;
    free(raw);
}

static const cr_awaitable_vtable operation_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    0u,
    operation_poll,
    operation_error,
    operation_drop,
    sizeof(int),
    _Alignof(int)
};

static cr_awaitable operation(int mode) {
    operation_state *state = (operation_state *)calloc(1, sizeof(*state));
    assert(state != NULL);
    state->mode = mode;
    state->error = (cr_error){77, "child failure"};
    return (cr_awaitable){state, &operation_vtable};
}

__async int child(int mode) {
    return __await operation(mode);
}

__async int parent(int mode) {
    return __await child(mode);
}

int main(void) {
    cr_parent_task ready;
    cr_parent_init(&ready, 0);
    assert(cr_parent_poll(&ready, NULL) == CR_POLL_PENDING);
    assert(cr_parent_poll(&ready, NULL) == CR_POLL_READY);
    assert(*cr_parent_result(&ready) == 42);
    assert(operation_drops == 1);
    cr_parent_drop(&ready);
    assert(operation_drops == 1);

    cr_parent_task failed;
    cr_parent_init(&failed, 1);
    assert(cr_parent_poll(&failed, NULL) == CR_POLL_PENDING);
    assert(cr_parent_poll(&failed, NULL) == CR_POLL_ERROR);
    assert(cr_parent_error(&failed)->code == 77);
    assert(operation_drops == 2);
    cr_parent_drop(&failed);
    assert(operation_drops == 2);

    cr_parent_task canceled;
    cr_parent_init(&canceled, 2);
    assert(cr_parent_poll(&canceled, NULL) == CR_POLL_PENDING);
    assert(cr_parent_poll(&canceled, NULL) == CR_POLL_CANCELED);
    assert(operation_drops == 3);
    cr_parent_drop(&canceled);
    assert(operation_drops == 3);

    cr_parent_task dropped;
    cr_parent_init(&dropped, 0);
    assert(cr_parent_poll(&dropped, NULL) == CR_POLL_PENDING);
    cr_parent_drop(&dropped);
    assert(operation_drops == 4);
    cr_parent_drop(&dropped);
    assert(operation_drops == 4);
    return 0;
}
"#,
    );
}

#[test]
fn embedded_binding_reuses_one_typed_child_and_preserves_lexical_lifetime() {
    let generated = compile_and_run(
        r#"
#include <assert.h>

static int starts;
static int cleanup_order;

static int start_value(int value) {
    starts += 1;
    return value;
}

static void record_cleanup(int value) {
    cleanup_order = cleanup_order * 10 + value;
}

__async int child(int value) {
    __defer record_cleanup(1);
    __yield value;
    return value + 1;
}

__async int repeated(int value) {
    __async int bound = child(start_value(value));
    int first = __await bound;
    int second = __await bound;
    return first + second;
}

__async int never_awaited(int value) {
    __async int bound = child(start_value(value));
    return value;
}

__async int skipped_activation(int value) {
    goto await_bound;
    __async int bound = child(start_value(value));
await_bound:
    return __await bound;
}

__async int reexecuted(void) {
    int index = 0;
    int total = 0;
again:
    __async int bound = child(start_value(index));
    int value = __await bound;
    total += value;
    index += 1;
    if (index < 2) goto again;
    return total;
}

__async int lifo(void) {
    __async int bound = child(7);
    __defer record_cleanup(2);
    return __await bound;
}

int main(void) {
    cr_repeated_task repeated;
    cr_repeated_init(&repeated, 5);
    assert(cr_repeated_poll(&repeated, NULL) == CR_POLL_YIELDED);
    assert(*cr_repeated_yielded(&repeated) == 5);
    assert(cr_repeated_poll(&repeated, NULL) == CR_POLL_READY);
    assert(*cr_repeated_result(&repeated) == 12);
    assert(starts == 1);
    cr_repeated_drop(&repeated);

    cr_never_awaited_task never_awaited;
    cr_never_awaited_init(&never_awaited, 9);
    assert(cr_never_awaited_poll(&never_awaited, NULL) == CR_POLL_READY);
    assert(starts == 2);
    cr_never_awaited_drop(&never_awaited);

    cr_skipped_activation_task skipped;
    cr_skipped_activation_init(&skipped, 11);
    assert(cr_skipped_activation_poll(&skipped, NULL) == CR_POLL_ERROR);
    assert(cr_skipped_activation_error(&skipped)->code == 1109);
    assert(starts == 2);
    cr_skipped_activation_drop(&skipped);

    cr_reexecuted_task reexecuted;
    cr_reexecuted_init(&reexecuted);
    cr_poll_status status;
    do {
        status = cr_reexecuted_poll(&reexecuted, NULL);
    } while (status == CR_POLL_YIELDED);
    assert(status == CR_POLL_READY);
    assert(*cr_reexecuted_result(&reexecuted) == 3);
    assert(starts == 4);
    cr_reexecuted_drop(&reexecuted);

    cleanup_order = 0;
    cr_lifo_task lifo;
    cr_lifo_init(&lifo);
    assert(cr_lifo_poll(&lifo, NULL) == CR_POLL_YIELDED);
    cr_lifo_drop(&lifo);
    assert(cleanup_order == 21);
    return 0;
}
"#,
    );

    let layout = generated
        .find("struct cr_repeated_task {")
        .expect("binding parent layout is emitted");
    let poll = generated
        .find("cr_poll_status cr_repeated_poll")
        .expect("binding parent poll is emitted");
    let drop = generated
        .find("void cr_repeated_drop")
        .expect("binding parent drop is emitted");
    let layout = &generated[layout..poll];
    let poll = &generated[poll..drop];

    assert!(layout.contains("cr_child_task cr_v_"));
    assert!(poll.contains("cr_child_init(&ctx->cr_v_"));
    assert!(poll.contains("cr_child_poll(&ctx->cr_v_"));
    assert!(poll.contains("_generation++"));
    assert!(!poll.contains("cr_child_create("));
    assert!(!poll.contains("into_awaitable"));
    assert!(!poll.contains(".vtable"));
}

#[test]
fn embedded_binding_cleanup_push_failure_clears_the_new_generation() {
    let generated = compile_and_run_with_options(
        r#"
#include <assert.h>
#include <stdlib.h>

static int fail_next_malloc;

void *cr_test_malloc(size_t size) {
    if (fail_next_malloc != 0) {
        fail_next_malloc = 0;
        return NULL;
    }
    return calloc(1, size);
}

__async int child(int value) {
    return value;
}

__async int parent(void) {
    __async int bound = child(42);
    return 0;
}

int main(void) {
    cr_parent_task task;
    cr_parent_init(&task);
    fail_next_malloc = 1;
    assert(cr_parent_poll(&task, NULL) == CR_POLL_ERROR);
    assert(cr_parent_error(&task)->code == 1002);
    assert(!task.cr_v_1_bound_active);
    assert(task.cr_v_1_bound_generation == 1);
    cr_parent_drop(&task);
    assert(!task.cr_v_1_bound_active);
    return 0;
}
"#,
        &["-include", "allocator.h", "-Dmalloc=cr_test_malloc"],
        &[(
            "allocator.h",
            "#include <stddef.h>\nvoid *cr_test_malloc(size_t size);\n",
        )],
    );

    let push = generated
        .find("if (!cr_cleanup_push(&ctx->cleanups")
        .expect("binding cleanup registration is emitted");
    let failure = &generated[push..];
    let helper_call = failure
        .find("_cleanup(&cr_binding_payload_1);")
        .expect("failed registration immediately invokes typed cleanup");
    let sticky_error = failure
        .find("cleanup allocation failed")
        .expect("failed registration reports the stable error");
    assert!(helper_call < sticky_error);
}

#[test]
fn boxed_recursive_direct_and_binding_children_have_finite_typed_layouts() {
    let generated = compile_and_run(
        r#"
#include <assert.h>

__async int recursive_direct(int value) {
    if (value == 0) return 0;
    return value + __await recursive_direct(value - 1);
}

__async int recursive_binding(int value) {
    if (value == 0) return 0;
    __async int next = recursive_binding(value - 1);
    int result = __await next;
    return result + 1;
}

int main(void) {
    cr_recursive_direct_task direct;
    cr_recursive_direct_init(&direct, 5);
    assert(cr_recursive_direct_poll(&direct, NULL) == CR_POLL_READY);
    assert(*cr_recursive_direct_result(&direct) == 15);
    cr_recursive_direct_drop(&direct);

    cr_recursive_binding_task binding;
    cr_recursive_binding_init(&binding, 5);
    assert(cr_recursive_binding_poll(&binding, NULL) == CR_POLL_READY);
    assert(*cr_recursive_binding_result(&binding) == 5);
    cr_recursive_binding_drop(&binding);
    return 0;
}
"#,
    );

    let direct_layout = generated
        .find("struct cr_recursive_direct_task {")
        .expect("recursive direct layout is emitted");
    let direct_poll = generated
        .find(
            "cr_poll_status cr_recursive_direct_poll(cr_recursive_direct_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("recursive direct poll is emitted");
    let direct_drop = generated
        .find("void cr_recursive_direct_drop(cr_recursive_direct_task *ctx) {")
        .expect("recursive direct drop is emitted");
    let direct_layout = &generated[direct_layout..direct_poll];
    let direct_poll = &generated[direct_poll..direct_drop];
    assert!(direct_layout.contains("cr_recursive_direct_task *cr_boxed_0;"));
    assert!(direct_poll.contains("cr_recursive_direct_create("));
    assert!(direct_poll.contains("cr_recursive_direct_poll(ctx->cr_boxed_0, poll_context)"));
    assert!(direct_poll.contains("cr_recursive_direct_destroy(ctx->cr_boxed_0)"));
    assert!(!direct_poll.contains("into_awaitable"));
    assert!(!direct_poll.contains(".vtable"));

    let binding_layout = generated
        .find("struct cr_recursive_binding_task {")
        .expect("recursive binding layout is emitted");
    let binding_poll = generated
        .find(
            "cr_poll_status cr_recursive_binding_poll(cr_recursive_binding_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("recursive binding poll is emitted");
    let binding_drop = generated
        .find("void cr_recursive_binding_drop(cr_recursive_binding_task *ctx) {")
        .expect("recursive binding drop is emitted");
    let binding_layout = &generated[binding_layout..binding_poll];
    let binding_poll = &generated[binding_poll..binding_drop];
    assert!(binding_layout.contains("cr_recursive_binding_task *cr_v_"));
    assert!(binding_poll.contains("cr_recursive_binding_create("));
    assert!(binding_poll.contains("cr_recursive_binding_poll(ctx->cr_v_"));
    assert!(!binding_poll.contains("into_awaitable"));
    assert!(!binding_poll.contains(".vtable"));
}

#[test]
fn embedded_and_boxed_static_children_preserve_exact_non_null_poll_context() {
    let generated = compile_and_run(
        r#"
#include <assert.h>
#include <stdint.h>

static const cr_poll_context *expected_context;
static int context_observations;

static cr_poll_status capture_context_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    assert(poll_context == expected_context);
    context_observations++;
    *(int *)out_value = (int)(intptr_t)raw;
    return CR_POLL_READY;
}

static void capture_context_drop(void *raw) {
    (void)raw;
}

static const cr_awaitable_vtable capture_context_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    0u,
    capture_context_poll,
    NULL,
    capture_context_drop,
    sizeof(int),
    _Alignof(int)
};

static cr_awaitable capture_context(int value) {
    return (cr_awaitable){(void *)(intptr_t)value, &capture_context_vtable};
}

__async int context_child(int value) {
    return __await capture_context(value);
}

__async int context_parent(int value) {
    return __await context_child(value);
}

__async int recursive_context(int depth) {
    if (depth == 0) return __await capture_context(40);
    return 1 + __await recursive_context(depth - 1);
}

int main(void) {
    int opaque_waker_storage = 0;
    cr_poll_status status;
    cr_poll_context context = {
        CR_POLL_CONTEXT_ABI_VERSION,
        sizeof(cr_poll_context),
        0u,
        (const cr_waker *)&opaque_waker_storage
    };
    cr_context_parent_task parent;
    cr_recursive_context_task recursive;
    expected_context = &context;

    cr_context_parent_init(&parent, 42);
    status = cr_context_parent_poll(&parent, &context);
    while (status == CR_POLL_PENDING) {
        status = cr_context_parent_poll(&parent, &context);
    }
    assert(status == CR_POLL_READY);
    assert(*cr_context_parent_result(&parent) == 42);
    cr_context_parent_drop(&parent);

    cr_recursive_context_init(&recursive, 2);
    status = cr_recursive_context_poll(&recursive, &context);
    while (status == CR_POLL_PENDING) {
        status = cr_recursive_context_poll(&recursive, &context);
    }
    assert(status == CR_POLL_READY);
    assert(*cr_recursive_context_result(&recursive) == 42);
    cr_recursive_context_drop(&recursive);
    assert(context_observations == 2);
    return 0;
}
"#,
    );

    let parent_start = generated
        .find(
            "cr_poll_status cr_context_parent_poll(cr_context_parent_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("embedded parent poll is emitted");
    let parent_drop = generated[parent_start..]
        .find("void cr_context_parent_drop(")
        .map(|offset| parent_start + offset)
        .expect("embedded parent drop is emitted");
    let parent_poll = &generated[parent_start..parent_drop];
    assert!(parent_poll.contains("cr_context_child_poll(&ctx->cr_child_0, poll_context)"));
    assert!(!parent_poll.contains("cr_context_child_into_awaitable("));
    assert!(!parent_poll.contains("cr_context_child_as_awaitable("));
    assert!(!parent_poll.contains(".vtable->poll"));

    let recursive_start = generated
        .find(
            "cr_poll_status cr_recursive_context_poll(cr_recursive_context_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("recursive poll is emitted");
    let recursive_drop = generated[recursive_start..]
        .find("void cr_recursive_context_drop(")
        .map(|offset| recursive_start + offset)
        .expect("recursive drop is emitted");
    let recursive_poll = &generated[recursive_start..recursive_drop];
    assert!(recursive_poll.contains("cr_recursive_context_poll(ctx->cr_boxed_"));
    assert!(recursive_poll.contains(", poll_context)"));
    assert!(!recursive_poll.contains("cr_recursive_context_into_awaitable("));
    assert!(!recursive_poll.contains("cr_recursive_context_as_awaitable("));
}

#[test]
fn boxed_recursive_mutual_cycle_boxes_only_the_cycle_closing_edge() {
    let generated = compile_and_run(
        r#"
#include <assert.h>

__async int even_value(int value) {
    if (value == 0) return 1;
    return __await odd_value(value - 1);
}

__async int odd_value(int value) {
    if (value == 0) return 0;
    return __await even_value(value - 1);
}

int main(void) {
    cr_even_value_task even;
    cr_even_value_init(&even, 10);
    assert(cr_even_value_poll(&even, NULL) == CR_POLL_READY);
    assert(*cr_even_value_result(&even) == 1);
    cr_even_value_drop(&even);

    cr_odd_value_task odd;
    cr_odd_value_init(&odd, 9);
    assert(cr_odd_value_poll(&odd, NULL) == CR_POLL_READY);
    assert(*cr_odd_value_result(&odd) == 1);
    cr_odd_value_drop(&odd);
    return 0;
}
"#,
    );

    let boxed_fields = generated.matches("_task *cr_boxed_").count();
    let embedded_fields = generated.matches("_task cr_child_").count();
    assert_eq!(boxed_fields, 1);
    assert_eq!(embedded_fields, 1);
    assert!(!generated.contains("cr_even_value_into_awaitable(cr_even_value_create("));
    assert!(!generated.contains("cr_odd_value_into_awaitable(cr_odd_value_create("));
}

#[test]
fn boxed_recursive_allocation_failures_are_null_safe_and_exactly_once() {
    let generated = compile_and_run_with_options(
        r#"
#include <assert.h>
#include <stdlib.h>

static int allocation_calls;
static int fail_on_call;

void *cr_test_malloc(size_t size) {
    allocation_calls += 1;
    if (allocation_calls == fail_on_call) return NULL;
    return calloc(1, size);
}

__async int recursive_direct(int value) {
    if (value == 0) return 0;
    return 1 + __await recursive_direct(value - 1);
}

__async int recursive_binding(int value) {
    if (value == 0) return 0;
    __async int next = recursive_binding(value - 1);
    return 1 + __await next;
}

int main(void) {
    cr_recursive_direct_task direct;
    cr_recursive_direct_init(&direct, 1);
    allocation_calls = 0;
    fail_on_call = 1;
    assert(cr_recursive_direct_poll(&direct, NULL) == CR_POLL_ERROR);
    assert(cr_recursive_direct_error(&direct)->code == 1006);
    assert(direct.cr_boxed_0 == NULL);
    assert(!direct.cr_boxed_0_active);
    cr_recursive_direct_drop(&direct);

    cr_recursive_binding_task create_failed;
    cr_recursive_binding_init(&create_failed, 1);
    allocation_calls = 0;
    fail_on_call = 1;
    assert(cr_recursive_binding_poll(&create_failed, NULL) == CR_POLL_ERROR);
    assert(cr_recursive_binding_error(&create_failed)->code == 1006);
    assert(create_failed.cr_v_1_next == NULL);
    assert(!create_failed.cr_v_1_next_active);
    cr_recursive_binding_drop(&create_failed);

    cr_recursive_binding_task cleanup_failed;
    cr_recursive_binding_init(&cleanup_failed, 1);
    allocation_calls = 0;
    fail_on_call = 2;
    assert(cr_recursive_binding_poll(&cleanup_failed, NULL) == CR_POLL_ERROR);
    assert(cr_recursive_binding_error(&cleanup_failed)->code == 1002);
    assert(cleanup_failed.cr_v_1_next == NULL);
    assert(!cleanup_failed.cr_v_1_next_active);
    assert(cleanup_failed.cr_v_1_next_generation == 1);
    cr_recursive_binding_drop(&cleanup_failed);
    assert(cleanup_failed.cr_v_1_next == NULL);
    return 0;
}
"#,
        &["-include", "allocator.h", "-Dmalloc=cr_test_malloc"],
        &[(
            "allocator.h",
            "#include <stddef.h>\nvoid *cr_test_malloc(size_t size);\n",
        )],
    );

    let direct_create = generated
        .find("ctx->cr_boxed_0 = cr_recursive_direct_create(")
        .expect("boxed direct create is emitted");
    let direct_poll = generated[direct_create..]
        .find("cr_recursive_direct_poll(ctx->cr_boxed_0, poll_context)")
        .expect("boxed direct poll is emitted after activation");
    let null_guard = generated[direct_create..]
        .find("if (ctx->cr_boxed_0 == NULL)")
        .expect("boxed direct null guard is emitted");
    assert!(null_guard < direct_poll);
    assert!(
        !generated[direct_create..direct_create + direct_poll].contains("destroy(ctx->cr_boxed_0)")
    );
}

#[test]
fn boxed_recursive_internal_function_emits_heap_api_without_dynamic_adapters() {
    let generated = compile_and_run(
        r#"
#include <assert.h>

static __async int local_recursive(int value) {
    if (value == 0) return 0;
    return 1 + __await local_recursive(value - 1);
}

__async int wrapper(int value) {
    return __await local_recursive(value);
}

int main(void) {
    cr_wrapper_task task;
    cr_wrapper_init(&task, 4);
    assert(cr_wrapper_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_wrapper_result(&task) == 4);
    cr_wrapper_drop(&task);
    return 0;
}
"#,
    );

    let stem = "cr_static_await_cr_local_recursive";
    assert!(generated.contains(&format!("{stem}_create(")));
    assert!(generated.contains(&format!("{stem}_poll(ctx->cr_boxed_0, poll_context)")));
    assert!(generated.contains(&format!("{stem}_destroy(ctx->cr_boxed_0)")));
    assert!(!generated.contains(&format!("{stem}_into_awaitable(")));
    assert!(!generated.contains(&format!("{stem}_as_awaitable(")));
    assert!(!generated.contains(&format!("{stem}_owning_awaitable_vtable")));
}

#[test]
fn sequential_direct_children_finalize_once_across_parent_drop() {
    let generated = compile_and_run(
        r#"
#include <assert.h>

static int finalized;

static void record_finalize(int child) {
    finalized = finalized * 10 + child;
}

__async int lifecycle_child(int child) {
    __defer record_finalize(child);
    __yield child;
    return child * 10;
}

__async int lifecycle_parent(void) {
    int first = __await lifecycle_child(1);
    int second = __await lifecycle_child(2);
    return first + second;
}

int main(void) {
    cr_lifecycle_parent_task parent;
    cr_lifecycle_parent_init(&parent);
    assert(cr_lifecycle_parent_poll(&parent, NULL) == CR_POLL_YIELDED);
    assert(*cr_lifecycle_parent_yielded(&parent) == 1);
    assert(finalized == 0);
    assert(cr_lifecycle_parent_poll(&parent, NULL) == CR_POLL_YIELDED);
    assert(*cr_lifecycle_parent_yielded(&parent) == 2);
    assert(finalized == 1);
    cr_lifecycle_parent_drop(&parent);
    assert(finalized == 12);
    return 0;
}
"#,
    );

    let layout = generated
        .find("struct cr_lifecycle_parent_task {")
        .expect("parent layout is emitted");
    let poll = generated
        .find("cr_poll_status cr_lifecycle_parent_poll(")
        .expect("parent poll is emitted");
    let layout = &generated[layout..poll];
    assert!(layout.contains("cr_lifecycle_child_task cr_child_0;"));
    assert!(layout.contains("bool cr_child_0_active;"));
    assert!(layout.contains("cr_lifecycle_child_task cr_child_1;"));
    assert!(layout.contains("bool cr_child_1_active;"));
    assert!(layout.contains("} cr_slot_0;"));
    assert!(generated.contains(
        "if (ctx->cr_child_1_active) {\n        cr_lifecycle_child_drop(&ctx->cr_slot_0.cr_child_1);"
    ));
}
