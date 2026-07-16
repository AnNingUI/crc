use std::fs;
use std::path::Path;
use std::process::Command;

use crc_lib::Compiler;
use crc_lib::config::Config;
use crc_lib::executor_runtime::portable_artifacts;
use crc_lib::runtime_abi::runtime_header;
use crc_lib::waker_abi::waker_header;

fn available_compiler() -> &'static str {
    ["clang", "gcc"]
        .into_iter()
        .find(|compiler| {
            Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("Clang or GCC is required for cross-unit static-await tests")
}

fn write(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent directory is created");
    }
    fs::write(path, contents).expect("fixture file is written");
}

fn write_executor_runtime(root: &Path) {
    write(&root.join("include/cr_runtime.h"), runtime_header());
    write(&root.join("include/cr_waker.h"), waker_header());
    for artifact in portable_artifacts() {
        write(&root.join(artifact.path), artifact.contents);
    }
}

#[test]
fn cross_unit_direct_and_binding_use_the_public_typed_task_api() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = directory.path();
    write(
        &root.join("crc/include/child.hr"),
        r#"
#ifndef CHILD_HR
#define CHILD_HR

__async int child_value(int value);
const cr_poll_context *child_value_observed_context(void);

#endif
"#,
    );
    write(
        &root.join("crc/src/child.cr"),
        r#"
#include "child.hr"

#include <assert.h>

static const cr_poll_context *observed_context;

static cr_poll_status child_context_poll(
    void *state,
    const cr_poll_context *poll_context,
    void *out_value
) {
    observed_context = poll_context;
    *(int *)out_value = *(int *)state;
    return CR_POLL_READY;
}

static void child_context_drop(void *state) {
    (void)state;
}

static const cr_awaitable_vtable child_context_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    0u,
    child_context_poll,
    NULL,
    child_context_drop,
    sizeof(int),
    _Alignof(int)
};

static cr_awaitable child_context_event(int *value) {
    return (cr_awaitable){value, &child_context_vtable};
}

const cr_poll_context *child_value_observed_context(void) {
    return observed_context;
}

static __async int local(int value) {
    __yield value;
    int result = value + 1;
    return __await child_context_event(&result);
}

__async int child_value(int value) {
    return __await local(value);
}
"#,
    );
    write(
        &root.join("crc/src/parent.cr"),
        r#"
#include "child.hr"
#include "cr_executor.h"
#include <assert.h>

static const cr_poll_context *parent_observed_context;

static cr_poll_status parent_context_poll(
    void *state,
    const cr_poll_context *poll_context,
    void *out_value
) {
    parent_observed_context = poll_context;
    *(int *)out_value = *(int *)state;
    return CR_POLL_READY;
}

static void parent_context_drop(void *state) {
    (void)state;
}

static const cr_awaitable_vtable parent_context_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    0u,
    parent_context_poll,
    NULL,
    parent_context_drop,
    sizeof(int),
    _Alignof(int)
};

static cr_awaitable parent_context_event(int *value) {
    return (cr_awaitable){value, &parent_context_vtable};
}

static __async int local(int value) {
    return value + 10;
}

__async int direct_parent(int value) {
    int marker = 0;
    int marker_result = __await parent_context_event(&marker);
    (void)marker_result;
    return __await child_value(value);
}

__async int binding_parent(int value) {
    __async int bound = child_value(value);
    int first = __await bound;
    int second = __await bound;
    return first + second;
}

__async int local_parent(int value) {
    return __await local(value);
}

typedef struct direct_observation {
    int calls;
    cr_poll_status statuses[2];
    int values[2];
} direct_observation;

static void observe_direct(
    void *raw,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    direct_observation *observation = (direct_observation *)raw;
    assert(observation->calls < 2);
    assert(error == NULL);
    assert(value != NULL);
    observation->statuses[observation->calls] = status;
    observation->values[observation->calls] = *(const int *)value;
    observation->calls++;
}

int main(void) {
    cr_error error = {0, NULL};
    cr_executor *executor = cr_executor_create_single(&error);
    cr_executor_task *ticket = NULL;
    cr_direct_parent_task *direct = cr_direct_parent_create(5, &error);
    cr_awaitable root = cr_direct_parent_into_awaitable(direct);
    direct_observation observation = {0};
    assert(executor != NULL);
    assert(direct != NULL);
    assert(cr_executor_spawn(
        executor,
        &root,
        observe_direct,
        &observation,
        &error,
        &ticket
    ));
    assert(cr_executor_run_ready(executor) == 2u);
    assert(observation.calls == 2);
    assert(observation.statuses[0] == CR_POLL_YIELDED);
    assert(observation.values[0] == 5);
    assert(observation.statuses[1] == CR_POLL_READY);
    assert(observation.values[1] == 6);
    assert(parent_observed_context != NULL);
    assert(child_value_observed_context() == parent_observed_context);
    cr_executor_task_release(ticket);
    cr_executor_destroy(executor);

    cr_binding_parent_task binding;
    cr_binding_parent_init(&binding, 7);
    assert(cr_binding_parent_poll(&binding, NULL) == CR_POLL_YIELDED);
    assert(*cr_binding_parent_yielded(&binding) == 7);
    assert(cr_binding_parent_poll(&binding, NULL) == CR_POLL_READY);
    assert(*cr_binding_parent_result(&binding) == 16);
    cr_binding_parent_drop(&binding);

    cr_local_parent_task local_task;
    cr_local_parent_init(&local_task, 32);
    assert(cr_local_parent_poll(&local_task, NULL) == CR_POLL_READY);
    assert(*cr_local_parent_result(&local_task) == 42);
    cr_local_parent_drop(&local_task);
    return 0;
}
"#,
    );

    Compiler::new(Config::default())
        .build_project(root)
        .expect("cross-unit project builds");

    write_executor_runtime(&root.join("crc/dist"));

    let parent =
        fs::read_to_string(root.join("crc/dist/parent.c")).expect("generated parent source exists");
    let child =
        fs::read_to_string(root.join("crc/dist/child.c")).expect("generated child source exists");
    let header = fs::read_to_string(root.join("crc/dist/include/child.h"))
        .expect("generated public header exists");

    assert!(header.contains("typedef struct cr_child_value_task cr_child_value_task;"));
    assert!(!header.contains("struct cr_child_value_task {"));
    assert!(!parent.contains("struct cr_child_value_task {"));
    assert!(child.contains("struct cr_child_value_task {"));
    assert!(parent.contains("cr_child_value_task *cr_boxed_"));
    assert!(parent.contains("cr_child_value_task *cr_v_"));
    assert!(parent.contains("cr_child_value_create("));
    assert!(parent.contains("cr_child_value_poll(ctx->cr_boxed_"));
    assert!(parent.contains("*cr_child_value_result(ctx->cr_boxed_"));
    assert!(parent.contains("*cr_child_value_yielded(ctx->cr_boxed_"));
    assert!(parent.contains("cr_child_value_error(ctx->cr_boxed_"));
    assert!(parent.contains("cr_child_value_destroy(ctx->cr_boxed_"));
    assert!(!parent.contains("cr_child_value_into_awaitable(cr_child_value_create("));
    assert!(!parent.contains("cr_child_value_owning_awaitable_vtable"));
    assert!(!parent.contains("cr_child_value_borrowed_awaitable_vtable"));
    assert!(!parent.contains("struct cr_child_value_task {"));
    assert!(
        !parent.contains(
            "cr_child_value_task *cr_child_value_create(int value, cr_error *out_error) {"
        )
    );
    assert!(!parent.contains("void cr_child_value_destroy(cr_child_value_task *task) {"));
    assert!(
        child.contains(
            "cr_child_value_task *cr_child_value_create(int value, cr_error *out_error) {"
        )
    );
    assert!(child.contains("void cr_child_value_destroy(cr_child_value_task *task) {"));
    assert!(child.contains("cr_crc_src_child_cr_local"));
    assert!(parent.contains("cr_crc_src_parent_cr_local"));

    let direct_poll_start = parent
        .find(
            "cr_poll_status cr_direct_parent_poll(cr_direct_parent_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("direct parent poll is emitted");
    let direct_drop_start = parent[direct_poll_start..]
        .find("void cr_direct_parent_drop(cr_direct_parent_task *ctx) {")
        .map(|offset| direct_poll_start + offset)
        .expect("direct parent drop is emitted");
    let direct_poll = &parent[direct_poll_start..direct_drop_start];
    assert!(direct_poll.contains("cr_child_value_poll(ctx->cr_boxed_"));
    assert!(direct_poll.contains(", poll_context)"));
    assert!(!direct_poll.contains("cr_child_value_into_awaitable("));
    assert!(!direct_poll.contains("cr_child_value_as_awaitable("));
    assert!(!direct_poll.contains("cr_child_value_owning_awaitable_vtable"));
    assert!(!direct_poll.contains("cr_child_value_borrowed_awaitable_vtable"));

    let binding_poll_start = parent
        .find(
            "cr_poll_status cr_binding_parent_poll(cr_binding_parent_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("binding parent poll is emitted");
    let binding_drop_start = parent[binding_poll_start..]
        .find("void cr_binding_parent_drop(cr_binding_parent_task *ctx) {")
        .map(|offset| binding_poll_start + offset)
        .expect("binding parent drop is emitted");
    let binding_poll = &parent[binding_poll_start..binding_drop_start];
    assert!(binding_poll.contains("cr_child_value_poll(ctx->cr_v_"));
    assert!(binding_poll.contains(", poll_context)"));
    assert!(!binding_poll.contains("cr_child_value_into_awaitable("));
    assert!(!binding_poll.contains("cr_child_value_as_awaitable("));
    assert!(!binding_poll.contains(".vtable->poll"));

    let executable = if cfg!(windows) {
        "cross-unit.exe"
    } else {
        "cross-unit"
    };
    let mut command = Command::new(available_compiler());
    command.args([
        "-std=c11",
        "-Wall",
        "-Wextra",
        "-Werror",
        "crc/dist/child.c",
        "crc/dist/parent.c",
    ]);
    command.args(
        portable_artifacts()
            .iter()
            .filter(|artifact| artifact.is_source)
            .map(|artifact| root.join("crc/dist").join(artifact.path)),
    );
    let compilation = command
        .args([
            "-I",
            "crc/dist/include",
            "-I",
            "crc/dist/runtime",
            "-o",
            executable,
        ])
        .current_dir(root)
        .output()
        .expect("native compiler runs");
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    let execution = Command::new(root.join(executable))
        .current_dir(root)
        .output()
        .expect("cross-unit executable runs");
    assert!(
        execution.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
}

#[test]
fn cross_unit_signature_type_must_be_visible_in_the_parent_unit() {
    let directory = tempfile::tempdir().expect("temporary project");
    let root = directory.path();
    write(
        &root.join("crc/include/child.hr"),
        "__async int hidden_child(hidden_value value);\n",
    );
    write(
        &root.join("crc/src/child.cr"),
        r#"
typedef int hidden_value;
#include "child.hr"

__async int hidden_child(hidden_value value) {
    return value;
}
"#,
    );
    write(
        &root.join("crc/src/parent.cr"),
        r#"
#include "child.hr"

__async int parent(void) {
    return __await hidden_child(42);
}

typedef int hidden_value;
"#,
    );

    let error = Compiler::new(Config::default())
        .build_project(root)
        .expect_err("invisible signature type must be rejected");
    assert!(error.to_string().contains("CRC6007"), "{error:#}");
}
