use std::fs;
use std::path::Path;
use std::process::Command;

use crc_lib::Compiler;
use crc_lib::config::{Config, OptimizationLevel};
use crc_lib::runtime_abi::runtime_header;

const REUSE_SOURCE: &str = r#"
#include <assert.h>
#include <stdio.h>

__async int child(int value) {
    return value + 1;
}

__async int sequential(void) {
    int first = __await child(10);
    int second = __await child(20);
    return first + second;
}

int main(void) {
    cr_sequential_task task;
    cr_sequential_init(&task);
    assert(cr_sequential_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_sequential_result(&task) == 32);
    printf("%zu\n", sizeof(task));
    cr_sequential_drop(&task);
    return 0;
}
"#;

const DYNAMIC_REUSE_SOURCE: &str = r#"
#include <assert.h>
#include <stdio.h>

typedef struct operation_state {
    int value;
    int polls;
} operation_state;

static operation_state operations[2];
static int operation_count;
static int dropped;
static cr_error operation_error;

static cr_poll_status operation_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    (void)poll_context;
    operation_state *state = (operation_state *)raw;
    state->polls++;
    if (state->polls == 1) return CR_POLL_PENDING;
    *(int *)out_value = state->value;
    return CR_POLL_READY;
}

static const cr_error *operation_get_error(const void *raw) {
    (void)raw;
    return &operation_error;
}

static void operation_drop(void *raw) {
    (void)raw;
    dropped++;
}

static const cr_awaitable_vtable operation_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    0u,
    operation_poll,
    operation_get_error,
    operation_drop,
    sizeof(int),
    _Alignof(int)
};

static cr_awaitable external_value(int value) {
    operation_state *state = &operations[operation_count++];
    state->value = value;
    state->polls = 0;
    return (cr_awaitable){state, &operation_vtable};
}

__async int dynamic_sequential(void) {
    int first = __await external_value(10);
    int second = __await external_value(20);
    return first + second;
}

int main(void) {
    cr_dynamic_sequential_task task;
    cr_dynamic_sequential_init(&task);
    assert(cr_dynamic_sequential_poll(&task, NULL) == CR_POLL_PENDING);
    assert(cr_dynamic_sequential_poll(&task, NULL) == CR_POLL_PENDING);
    assert(cr_dynamic_sequential_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_dynamic_sequential_result(&task) == 30);
    assert(dropped == 2);
    printf("%zu\n", sizeof(task));
    cr_dynamic_sequential_drop(&task);
    assert(dropped == 2);
    return 0;
}
"#;

const DECLARATOR_REUSE_SOURCE: &str = r#"
#include <assert.h>
#include <stdio.h>

static int add_one(int value) { return value + 1; }
static int add_two(int value) { return value + 2; }
static void consume(int value) { (void)value; }

__async int array_flow(void) {
    char first[64] = {1};
    __yield 1;
    consume(first[0]);
    long long second[8] = {5};
    __yield 5;
    return (int)second[0] + 1;
}

__async int pointer_flow(void) {
    int (*first)(int) = add_one;
    __yield 2;
    consume(first(1));
    int (*second)(int) = add_two;
    __yield 3;
    return second(2);
}

int main(void) {
    cr_array_flow_task arrays;
    cr_array_flow_init(&arrays);
    assert(cr_array_flow_poll(&arrays, NULL) == CR_POLL_YIELDED);
    assert(*cr_array_flow_yielded(&arrays) == 1);
    assert(cr_array_flow_poll(&arrays, NULL) == CR_POLL_YIELDED);
    assert(*cr_array_flow_yielded(&arrays) == 5);
    assert(cr_array_flow_poll(&arrays, NULL) == CR_POLL_READY);
    assert(*cr_array_flow_result(&arrays) == 6);
    cr_array_flow_drop(&arrays);

    cr_pointer_flow_task pointers;
    cr_pointer_flow_init(&pointers);
    assert(cr_pointer_flow_poll(&pointers, NULL) == CR_POLL_YIELDED);
    assert(*cr_pointer_flow_yielded(&pointers) == 2);
    assert(cr_pointer_flow_poll(&pointers, NULL) == CR_POLL_YIELDED);
    assert(*cr_pointer_flow_yielded(&pointers) == 3);
    assert(cr_pointer_flow_poll(&pointers, NULL) == CR_POLL_READY);
    assert(*cr_pointer_flow_result(&pointers) == 4);
    cr_pointer_flow_drop(&pointers);

    printf("%zu\n", sizeof(arrays) + sizeof(pointers));
    return 0;
}
"#;

fn available_compilers() -> Vec<&'static str> {
    ["clang", "gcc"]
        .into_iter()
        .filter(|compiler| {
            Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .collect()
}

fn available_compiler() -> &'static str {
    available_compilers()
        .into_iter()
        .next()
        .expect("Clang or GCC is required for context-layout tests")
}

fn compile(level: OptimizationLevel) -> String {
    compile_source(REUSE_SOURCE, "context-layout.cr", level)
}

fn compile_source(source: &str, path: &str, level: OptimizationLevel) -> String {
    let mut config = Config::default();
    config.build.optimization = level;
    Compiler::new(config)
        .compile_source(source, Path::new(path))
        .expect("context-layout fixture compiles")
}

fn compile_and_measure(source: &str, level: OptimizationLevel) -> usize {
    compile_and_measure_with(source, level, available_compiler())
}

fn compile_and_measure_with(source: &str, level: OptimizationLevel, compiler: &str) -> usize {
    let directory = tempfile::tempdir().expect("temporary directory");
    fs::write(directory.path().join("cr_runtime.h"), runtime_header())
        .expect("runtime header is written");
    fs::write(directory.path().join("context-layout.c"), source).expect("generated C is written");
    let executable = if cfg!(windows) {
        "context-layout.exe"
    } else {
        "context-layout"
    };
    let compilation = Command::new(compiler)
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "context-layout.c",
            "-o",
        ])
        .arg(executable)
        .current_dir(directory.path())
        .output()
        .expect("native C compiler runs");
    assert!(
        compilation.status.success(),
        "{compiler} {level:?} compilation failed:\n{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    let execution = Command::new(directory.path().join(executable))
        .current_dir(directory.path())
        .output()
        .expect("context-layout executable runs");
    assert!(
        execution.status.success(),
        "{compiler} {level:?} execution failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
    String::from_utf8(execution.stdout)
        .expect("size output is UTF-8")
        .trim()
        .parse()
        .expect("size output is numeric")
}

#[test]
fn speed_reuses_storage_without_runtime_dispatch_or_lifecycle_changes() {
    let none = compile(OptimizationLevel::None);
    let speed = compile(OptimizationLevel::Speed);

    assert!(!none.contains("cr_slot_"));
    assert!(speed.contains("union {"));
    assert!(speed.contains("} cr_slot_0;"));
    assert!(speed.contains("ctx->cr_slot_"));

    let poll_start = speed
        .find("cr_poll_status cr_sequential_poll(")
        .expect("sequential poll");
    let poll_end = speed[poll_start..]
        .find("\n}\n")
        .map(|offset| poll_start + offset)
        .expect("sequential poll end");
    let poll = &speed[poll_start..poll_end];
    assert!(!poll.contains("malloc("));
    assert!(!poll.contains(".vtable->poll"));

    let none_size = compile_and_measure(&none, OptimizationLevel::None);
    let speed_size = compile_and_measure(&speed, OptimizationLevel::Speed);
    assert!(
        speed_size < none_size,
        "Speed task size {speed_size} must be smaller than None task size {none_size}"
    );
}

#[test]
fn size_cross_type_layout_is_deterministic_and_no_larger_than_speed() {
    let speed = compile(OptimizationLevel::Speed);
    let size = compile(OptimizationLevel::Size);
    let repeated = compile(OptimizationLevel::Size);
    let aggressive = compile(OptimizationLevel::Aggressive);
    let aggressive_repeated = compile(OptimizationLevel::Aggressive);
    assert_eq!(size, repeated);
    assert_eq!(aggressive, aggressive_repeated);
    assert!(size.contains("int cr_v_2_second;"));
    assert!(size.contains("cr_child_task cr_child_0;"));
    assert!(size.contains("ctx->cr_slot_0.cr_v_2_second"));

    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        let speed_size = compile_and_measure_with(&speed, OptimizationLevel::Speed, compiler);
        let size_size = compile_and_measure_with(&size, OptimizationLevel::Size, compiler);
        let aggressive_size =
            compile_and_measure_with(&aggressive, OptimizationLevel::Aggressive, compiler);
        assert!(size_size <= speed_size, "{compiler}");
        assert!(aggressive_size <= size_size, "{compiler}");
    }
}

#[test]
fn speed_reuses_dynamic_awaitables_only_after_the_previous_drop() {
    let none = compile_source(
        DYNAMIC_REUSE_SOURCE,
        "dynamic-context-layout.cr",
        OptimizationLevel::None,
    );
    let speed = compile_source(
        DYNAMIC_REUSE_SOURCE,
        "dynamic-context-layout.cr",
        OptimizationLevel::Speed,
    );
    assert!(!none.contains("cr_slot_"));
    assert!(speed.contains("cr_awaitable cr_await_0;"));
    assert!(speed.contains("cr_awaitable cr_await_1;"));
    assert!(speed.contains("bool cr_await_0_active;"));
    assert!(speed.contains("bool cr_await_1_active;"));
    assert!(speed.contains("ctx->cr_slot_0.cr_await_0"));
    assert!(speed.contains("ctx->cr_slot_0.cr_await_1"));

    let none_size = compile_and_measure(&none, OptimizationLevel::None);
    let speed_size = compile_and_measure(&speed, OptimizationLevel::Speed);
    assert!(speed_size < none_size);
}

#[test]
fn size_preserves_array_and_function_pointer_union_declarators() {
    let speed = compile_source(
        DECLARATOR_REUSE_SOURCE,
        "declarator-context-layout.cr",
        OptimizationLevel::Speed,
    );
    let size = compile_source(
        DECLARATOR_REUSE_SOURCE,
        "declarator-context-layout.cr",
        OptimizationLevel::Size,
    );
    assert!(size.contains("union {"));
    assert!(size.contains("[64];"));
    assert!(size.contains("[8];"));
    assert!(size.contains("(*cr_v_"));
    assert!(size.contains("ctx->cr_slot_"));

    let speed_size = compile_and_measure(&speed, OptimizationLevel::Speed);
    let size_size = compile_and_measure(&size, OptimizationLevel::Size);
    assert!(size_size < speed_size);
}
