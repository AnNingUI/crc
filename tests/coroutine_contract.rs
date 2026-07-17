use std::fs;
use std::path::Path;
use std::process::Command;

use crc_lib::Compiler;
use crc_lib::backend_runtime::memory_net_awaitable_artifacts;
use crc_lib::config::{Config, OptimizationLevel};
use crc_lib::executor_runtime::portable_artifacts;
use crc_lib::runtime_abi::runtime_header;
use crc_lib::waker_abi::waker_header;

const CONTRACT_SOURCE: &str = r#"
#include <assert.h>
#include <stdlib.h>

typedef struct contract_operation_state {
    int mode;
    int polls;
} contract_operation_state;

static int contract_drops;
static int contract_defers;
static const cr_error contract_failure = {41, "contract operation failed"};

static cr_poll_status contract_operation_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    (void)poll_context;
    contract_operation_state *state = (contract_operation_state *)raw;
    if (state->polls++ == 0) return CR_POLL_PENDING;
    if (state->mode == 1) return CR_POLL_ERROR;
    if (state->mode == 2) return CR_POLL_CANCELED;
    *(int *)out_value = 17;
    return CR_POLL_READY;
}

static const cr_error *contract_operation_error(const void *raw) {
    (void)raw;
    return &contract_failure;
}

static void contract_operation_drop(void *raw) {
    contract_drops++;
    free(raw);
}

static const cr_awaitable_vtable contract_operation_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    0u,
    contract_operation_poll,
    contract_operation_error,
    contract_operation_drop,
    sizeof(int),
    _Alignof(int)
};

static cr_awaitable contract_operation(int mode) {
    contract_operation_state *state = malloc(sizeof(*state));
    assert(state != NULL);
    state->mode = mode;
    state->polls = 0;
    return (cr_awaitable){state, &contract_operation_vtable};
}

static void contract_record_defer(int mode) {
    (void)mode;
    contract_defers++;
}

__async int terminal_contract(void) {
    __yield 4;
    return 8;
}

__async int await_contract(int mode) {
    __defer contract_record_defer(mode);
    return __await contract_operation(mode);
}

int main(void) {
    cr_error create_error = {0, NULL};

    cr_terminal_contract_task *terminal =
        cr_terminal_contract_create(&create_error);
    assert(terminal != NULL);
    assert(cr_terminal_contract_poll(terminal, NULL) == CR_POLL_YIELDED);
    assert(*cr_terminal_contract_yielded(terminal) == 4);
    assert(cr_terminal_contract_poll(terminal, NULL) == CR_POLL_READY);
    assert(*cr_terminal_contract_result(terminal) == 8);
    assert(cr_terminal_contract_poll(terminal, NULL) == CR_POLL_READY);
    cr_terminal_contract_destroy(terminal);

    cr_await_contract_task *ready = cr_await_contract_create(0, &create_error);
    assert(ready != NULL);
    assert(cr_await_contract_poll(ready, NULL) == CR_POLL_PENDING);
    assert(cr_await_contract_poll(ready, NULL) == CR_POLL_READY);
    assert(*cr_await_contract_result(ready) == 17);
    assert(contract_drops == 1);
    assert(contract_defers == 1);
    assert(cr_await_contract_poll(ready, NULL) == CR_POLL_READY);
    assert(contract_drops == 1);
    assert(contract_defers == 1);
    cr_await_contract_destroy(ready);

    cr_await_contract_task *failed = cr_await_contract_create(1, &create_error);
    assert(failed != NULL);
    assert(cr_await_contract_poll(failed, NULL) == CR_POLL_PENDING);
    assert(cr_await_contract_poll(failed, NULL) == CR_POLL_ERROR);
    assert(cr_await_contract_error(failed)->code == 41);
    assert(contract_drops == 2);
    assert(contract_defers == 2);
    assert(cr_await_contract_poll(failed, NULL) == CR_POLL_ERROR);
    assert(contract_drops == 2);
    assert(contract_defers == 2);
    cr_await_contract_destroy(failed);

    cr_await_contract_task *canceled =
        cr_await_contract_create(2, &create_error);
    assert(canceled != NULL);
    assert(cr_await_contract_poll(canceled, NULL) == CR_POLL_PENDING);
    assert(cr_await_contract_poll(canceled, NULL) == CR_POLL_CANCELED);
    assert(contract_drops == 3);
    assert(contract_defers == 3);
    assert(cr_await_contract_poll(canceled, NULL) == CR_POLL_CANCELED);
    cr_await_contract_destroy(canceled);

    cr_await_contract_task *dropped =
        cr_await_contract_create(0, &create_error);
    assert(dropped != NULL);
    assert(cr_await_contract_poll(dropped, NULL) == CR_POLL_PENDING);
    cr_await_contract_destroy(dropped);
    assert(contract_drops == 4);
    assert(contract_defers == 4);

    cr_terminal_contract_task *borrowed_task =
        cr_terminal_contract_create(&create_error);
    assert(borrowed_task != NULL);
    cr_awaitable borrowed = cr_terminal_contract_as_awaitable(borrowed_task);
    int borrowed_value = 0;
    assert(borrowed.vtable->poll(borrowed.state, NULL, &borrowed_value) == CR_POLL_YIELDED);
    assert(borrowed_value == 4);
    assert(borrowed.vtable->poll(borrowed.state, NULL, &borrowed_value) == CR_POLL_READY);
    assert(borrowed_value == 8);
    borrowed.vtable->drop(borrowed.state);
    assert(cr_terminal_contract_poll(borrowed_task, NULL) == CR_POLL_READY);
    cr_terminal_contract_destroy(borrowed_task);

    cr_terminal_contract_task *owned_task =
        cr_terminal_contract_create(&create_error);
    assert(owned_task != NULL);
    cr_awaitable owned = cr_terminal_contract_into_awaitable(owned_task);
    int owned_value = 0;
    assert(owned.vtable->poll(owned.state, NULL, &owned_value) == CR_POLL_YIELDED);
    assert(owned.vtable->poll(owned.state, NULL, &owned_value) == CR_POLL_READY);
    assert(owned_value == 8);
    owned.vtable->drop(owned.state);

    return 0;
}
"#;

const EXECUTOR_FORWARDING_SOURCE: &str = r#"
#include "cr_executor.h"

#include <assert.h>
#include <stdlib.h>

typedef struct forwarding_event_state {
    int value;
    int polls;
    cr_waker retained_waker;
} forwarding_event_state;

static const cr_poll_context *forwarding_context;
static int forwarding_drops;

static cr_poll_status forwarding_event_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    forwarding_event_state *state = (forwarding_event_state *)raw;
    assert(poll_context != NULL);
    assert(poll_context->abi_version == CR_POLL_CONTEXT_ABI_VERSION);
    assert(poll_context->struct_size >= CR_POLL_CONTEXT_V1_MIN_SIZE);
    assert(poll_context->available_capabilities == CR_POLL_CAP_WAKER);
    assert(cr_waker_is_valid(poll_context->waker));
    if (forwarding_context == NULL) {
        forwarding_context = poll_context;
    } else {
        assert(forwarding_context == poll_context);
    }
    state->polls++;
    if (state->polls == 1) {
        assert(cr_waker_clone(poll_context->waker, &state->retained_waker));
        cr_waker_wake(&state->retained_waker);
        cr_waker_wake(&state->retained_waker);
        return CR_POLL_PENDING;
    }
    *(int *)out_value = state->value;
    return CR_POLL_READY;
}

static void forwarding_event_drop(void *raw) {
    forwarding_event_state *state = (forwarding_event_state *)raw;
    forwarding_drops++;
    cr_waker_drop(&state->retained_waker);
    free(state);
}

static const cr_awaitable_vtable forwarding_event_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    CR_POLL_CAP_WAKER,
    forwarding_event_poll,
    NULL,
    forwarding_event_drop,
    sizeof(int),
    _Alignof(int)
};

static cr_awaitable forwarding_event(int value) {
    forwarding_event_state *state = calloc(1u, sizeof(*state));
    assert(state != NULL);
    state->value = value;
    return (cr_awaitable){state, &forwarding_event_vtable};
}

__async int forwarding_child(int value) {
    return __await forwarding_event(value);
}

__async int forwarding_root(void) {
    int first = __await forwarding_event(20);
    int second = __await forwarding_child(22);
    return first + second;
}

typedef struct forwarding_observation {
    int calls;
    cr_poll_status status;
    int value;
} forwarding_observation;

static void forwarding_observe(
    void *raw,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    forwarding_observation *observation = (forwarding_observation *)raw;
    observation->calls++;
    observation->status = status;
    assert(status == CR_POLL_READY);
    assert(value != NULL);
    assert(error == NULL);
    observation->value = *(const int *)value;
}

int main(void) {
    cr_error error = {0, NULL};
    cr_executor *executor = cr_executor_create_single(&error);
    cr_forwarding_root_task *task;
    cr_executor_task *ticket = NULL;
    forwarding_observation observation = {0, CR_POLL_PENDING, 0};
    cr_awaitable root;

    assert(executor != NULL);
    task = cr_forwarding_root_create(&error);
    assert(task != NULL);
    root = cr_forwarding_root_into_awaitable(task);
    assert(cr_executor_spawn(
        executor,
        &root,
        forwarding_observe,
        &observation,
        &error,
        &ticket
    ));
    assert(root.state == NULL && root.vtable == NULL);
    assert(cr_executor_run_ready(executor) == 3u);
    assert(observation.calls == 1);
    assert(observation.status == CR_POLL_READY);
    assert(observation.value == 42);
    assert(forwarding_context != NULL);
    assert(forwarding_drops == 2);
    cr_executor_task_release(ticket);
    cr_executor_destroy(executor);
    return 0;
}
"#;

const STAGE4_EXECUTOR_APP: &str = r#"
#include "cr_executor.h"

#include <assert.h>

cr_awaitable stage4_abi_v3_root_into_awaitable(void);

typedef struct stage4_observation {
    int calls;
    cr_poll_status statuses[2];
    int values[2];
} stage4_observation;

static void observe_stage4(
    void *raw,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    stage4_observation *observation = (stage4_observation *)raw;
    assert(observation->calls < 2);
    assert(status == CR_POLL_YIELDED || status == CR_POLL_READY);
    assert(value != NULL);
    assert(error == NULL);
    observation->statuses[observation->calls] = status;
    observation->values[observation->calls] = *(const int *)value;
    observation->calls++;
}

int main(void) {
    cr_error error = {0, NULL};
    cr_executor *executor = cr_executor_create_single(&error);
    cr_executor_task *ticket = NULL;
    stage4_observation observation = {0};
    cr_awaitable root = stage4_abi_v3_root_into_awaitable();

    assert(executor != NULL);
    assert(cr_executor_spawn(
        executor,
        &root,
        observe_stage4,
        &observation,
        &error,
        &ticket
    ));
    assert(cr_executor_run_ready(executor) == 2u);
    assert(observation.calls == 2);
    assert(observation.statuses[0] == CR_POLL_YIELDED);
    assert(observation.values[0] == 17);
    assert(observation.statuses[1] == CR_POLL_READY);
    assert(observation.values[1] == 42);
    cr_executor_task_release(ticket);
    cr_executor_destroy(executor);
    return 0;
}
"#;

const GENERATED_BACKEND_ROOT_SOURCE: &str = r#"
#include "cr_waker.h"
#include "cr_backend_internal.h"

#include <assert.h>
#include <stddef.h>

typedef union backend_opaque_storage {
    max_align_t alignment;
    unsigned char bytes[512];
} backend_opaque_storage;

static cr_awaitable pending_receive;

static cr_awaitable generated_receive(void) {
    cr_awaitable result = pending_receive;
    pending_receive = (cr_awaitable){NULL, NULL};
    return result;
}

static __async unsigned long long backend_child(void) {
    return __await generated_receive();
}

__async unsigned long long backend_root(void) {
    return __await backend_child();
}

int main(void) {
    const cr_extension_id net_id = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    cr_backend *backend = NULL;
    cr_backend_error backend_error;
    cr_backend_pump_result pump;
    cr_net_error net_error;
    cr_error error = {0, NULL};
    const cr_backend_extension_desc *base;
    const cr_net_extension_desc *net;
    backend_opaque_storage awaitable_storage = {0};
    backend_opaque_storage operation_storage = {0};
    unsigned char buffer[16] = {0};
    cr_net_receive_awaitable_state *state =
        (cr_net_receive_awaitable_state *)(void *)awaitable_storage.bytes;
    cr_net_receive_operation *operation =
        (cr_net_receive_operation *)(void *)operation_storage.bytes;
    cr_backend_root_task *task;
    cr_awaitable root;
    unsigned long long value = 0u;

    assert(cr_backend_create(
        &cr_backend_memory_provider_desc,
        &backend,
        &backend_error
    ));
    base = cr_backend_query_extension(
        backend,
        net_id,
        CR_NET_EXPERIMENTAL_ABI_VERSION,
        &backend_error
    );
    assert(base != NULL);
    net = (const cr_net_extension_desc *)(const void *)base;
    assert(cr_net_receive_awaitable_initialize(
        state,
        sizeof(awaitable_storage),
        backend,
        net,
        operation,
        sizeof(operation_storage),
        (cr_native_socket_handle){
            CR_NATIVE_SOCKET_MEMORY,
            0u,
            (uintptr_t)31u
        },
        buffer,
        sizeof(buffer),
        &pending_receive,
        &error
    ));

    task = cr_backend_root_create(&error);
    assert(task != NULL);
    root = cr_backend_root_into_awaitable(task);
    assert(root.vtable->poll(root.state, NULL, &value) == CR_POLL_PENDING);
    assert(cr_backend_memory_complete_ready(
        backend,
        operation,
        "abc",
        3u,
        &net_error
    ));
    assert(cr_backend_pump(backend, 0u, 1u, &pump));
    assert(root.vtable->poll(root.state, NULL, &value) == CR_POLL_READY);
    assert(value == 3u);
    root.vtable->drop(root.state);
    assert(cr_backend_destroy(backend, &backend_error));
    return 0;
}
"#;

fn available_compiler() -> &'static str {
    ["clang", "gcc"]
        .into_iter()
        .find(|compiler| {
            Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("Clang or GCC is required for coroutine contract tests")
}

fn compile(source: &str, name: &str) -> String {
    Compiler::new(Config::default())
        .compile_source(source, Path::new(name))
        .expect("CR source compiles")
}

fn compile_with_optimization(source: &str, name: &str, optimization: OptimizationLevel) -> String {
    let mut config = Config::default();
    config.build.optimization = optimization;
    Compiler::new(config)
        .compile_source(source, Path::new(name))
        .expect("CR source compiles")
}

fn compile_and_run_c(source: &str) {
    let directory = tempfile::tempdir().expect("temporary directory");
    fs::write(directory.path().join("cr_runtime.h"), runtime_header())
        .expect("runtime header is written");
    fs::write(directory.path().join("contract.c"), source).expect("generated C is written");
    let executable = if cfg!(windows) {
        "contract.exe"
    } else {
        "contract"
    };
    let compilation = Command::new(available_compiler())
        .arg("-std=c11")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("contract.c")
        .arg("-o")
        .arg(executable)
        .current_dir(directory.path())
        .output()
        .expect("native C compiler runs");
    assert!(
        compilation.status.success(),
        "{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    let execution = Command::new(directory.path().join(executable))
        .current_dir(directory.path())
        .output()
        .expect("contract executable runs");
    assert!(
        execution.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
}

fn write_executor_runtime(directory: &Path) {
    fs::create_dir_all(directory.join("include")).expect("include directory is created");
    fs::create_dir_all(directory.join("runtime")).expect("runtime directory is created");
    fs::write(directory.join("include/cr_runtime.h"), runtime_header())
        .expect("runtime header is written");
    fs::write(directory.join("include/cr_waker.h"), waker_header())
        .expect("Waker header is written");
    for artifact in portable_artifacts() {
        let path = directory.join(artifact.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("executor artifact directory is created");
        }
        fs::write(path, artifact.contents).expect("executor artifact is written");
    }
}

fn executor_source_paths() -> Vec<&'static str> {
    portable_artifacts()
        .iter()
        .filter(|artifact| artifact.is_source)
        .map(|artifact| artifact.path)
        .collect()
}

fn write_backend_runtime(directory: &Path) {
    fs::create_dir_all(directory.join("include")).expect("include directory is created");
    fs::create_dir_all(directory.join("runtime")).expect("runtime directory is created");
    fs::write(directory.join("include/cr_runtime.h"), runtime_header())
        .expect("runtime header is written");
    fs::write(directory.join("include/cr_waker.h"), waker_header())
        .expect("Waker header is written");
    for artifact in memory_net_awaitable_artifacts() {
        let path = directory.join(artifact.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("backend artifact directory is created");
        }
        fs::write(path, artifact.contents).expect("backend artifact is written");
    }
}

fn backend_source_paths() -> Vec<&'static str> {
    memory_net_awaitable_artifacts()
        .into_iter()
        .filter(|artifact| artifact.is_source)
        .map(|artifact| artifact.path)
        .collect()
}

fn assert_success(output: &std::process::Output, action: &str) {
    assert!(
        output.status.success(),
        "{action}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn public_coroutine_contract_is_executable() {
    compile_and_run_c(&compile(CONTRACT_SOURCE, "coroutine-contract.cr"));
}

#[test]
fn generated_root_runs_on_executor_and_forwards_one_context_through_static_children() {
    let generated = compile(EXECUTOR_FORWARDING_SOURCE, "executor-forwarding.cr");
    let root_poll_start = generated
        .find(
            "cr_poll_status cr_forwarding_root_poll(cr_forwarding_root_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("root poll is emitted");
    let root_drop_start = generated[root_poll_start..]
        .find("void cr_forwarding_root_drop(cr_forwarding_root_task *ctx) {")
        .map(|offset| root_poll_start + offset)
        .expect("root drop is emitted");
    let root_poll = &generated[root_poll_start..root_drop_start];
    assert!(root_poll.contains("cr_forwarding_child_poll(&ctx->cr_child_"));
    assert!(root_poll.contains(", poll_context)"));
    assert!(!root_poll.contains("cr_forwarding_child_into_awaitable("));
    assert!(!root_poll.contains("cr_forwarding_child_as_awaitable("));
    assert!(!root_poll.contains("cr_forwarding_child_owning_awaitable_vtable"));

    let directory = tempfile::tempdir().expect("temporary directory");
    write_executor_runtime(directory.path());
    fs::write(directory.path().join("generated.c"), generated)
        .expect("generated source is written");
    let executable = if cfg!(windows) {
        "executor-forwarding.exe"
    } else {
        "executor-forwarding"
    };
    let mut command = Command::new(available_compiler());
    command.args(["-std=c11", "-Wall", "-Wextra", "-Werror", "generated.c"]);
    command.args(executor_source_paths());
    command
        .args(["-I", "include", "-I", "runtime", "-o"])
        .arg(executable)
        .current_dir(directory.path());
    let compilation = command.output().expect("native compiler runs");
    assert_success(
        &compilation,
        "generated executor forwarding fixture compiles",
    );
    let execution = Command::new(directory.path().join(executable))
        .current_dir(directory.path())
        .output()
        .expect("generated executor fixture runs");
    assert_success(&execution, "generated executor forwarding fixture runs");
}

#[test]
fn generated_owning_root_composes_with_reference_receive_awaitable() {
    let generated = compile(GENERATED_BACKEND_ROOT_SOURCE, "backend-root.cr");
    let root_poll_start = generated
        .find(
            "cr_poll_status cr_backend_root_poll(cr_backend_root_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("backend root poll is emitted");
    let root_drop_start = generated[root_poll_start..]
        .find("void cr_backend_root_drop(cr_backend_root_task *ctx) {")
        .map(|offset| root_poll_start + offset)
        .expect("backend root drop is emitted");
    let root_poll = &generated[root_poll_start..root_drop_start];
    assert!(root_poll.contains("cr_backend_child_poll(&ctx->cr_child_"));
    assert!(!root_poll.contains("cr_backend_child_into_awaitable("));
    assert!(!root_poll.contains("cr_backend_child_as_awaitable("));

    let directory = tempfile::tempdir().expect("temporary directory");
    write_backend_runtime(directory.path());
    fs::write(directory.path().join("generated-backend.c"), generated)
        .expect("generated backend source is written");
    let executable = if cfg!(windows) {
        "generated-backend.exe"
    } else {
        "generated-backend"
    };
    let mut command = Command::new(available_compiler());
    command.args([
        "-std=c11",
        "-Wall",
        "-Wextra",
        "-Werror",
        "generated-backend.c",
    ]);
    command.args(backend_source_paths());
    command
        .args(["-I", "include", "-I", "runtime", "-o"])
        .arg(executable)
        .current_dir(directory.path());
    let compilation = command.output().expect("native compiler runs");
    assert_success(
        &compilation,
        "generated backend owning-root fixture compiles",
    );
    let execution = Command::new(directory.path().join(executable))
        .current_dir(directory.path())
        .output()
        .expect("generated backend fixture runs");
    assert_success(&execution, "generated backend owning-root fixture runs");
}

#[test]
fn frozen_stage4_object_links_unchanged_into_stage5_executor() {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_executor_runtime(directory.path());
    fs::write(
        directory.path().join("stage4_abi_v3_root.c"),
        include_str!("fixtures/waker/stage4_abi_v3_root.c"),
    )
    .expect("frozen Stage 4 fixture is written");
    fs::write(directory.path().join("stage5_app.c"), STAGE4_EXECUTOR_APP)
        .expect("Stage 5 application is written");

    let object = "stage4_abi_v3_root.o";
    let object_compilation = Command::new(available_compiler())
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-I",
            "include",
            "-c",
            "stage4_abi_v3_root.c",
            "-o",
            object,
        ])
        .current_dir(directory.path())
        .output()
        .expect("frozen Stage 4 translation unit compiles");
    assert_success(
        &object_compilation,
        "frozen Stage 4 translation unit compiles independently",
    );

    let executable = if cfg!(windows) {
        "stage4-stage5.exe"
    } else {
        "stage4-stage5"
    };
    let mut link = Command::new(available_compiler());
    link.args([
        "-std=c11",
        "-Wall",
        "-Wextra",
        "-Werror",
        "stage5_app.c",
        object,
    ]);
    link.args(executor_source_paths());
    link.args(["-I", "include", "-I", "runtime", "-o"])
        .arg(executable)
        .current_dir(directory.path());
    let linked = link.output().expect("Stage 5 application links");
    assert_success(
        &linked,
        "unchanged Stage 4 object links into Stage 5 executor",
    );
    let execution = Command::new(directory.path().join(executable))
        .current_dir(directory.path())
        .output()
        .expect("Stage 4 compatibility executable runs");
    assert_success(&execution, "Stage 4 compatibility executable runs");
}

#[test]
fn representative_abi_v3_output_is_byte_stable() {
    let canonical_lf = |text: &str| text.replace("\r\n", "\n").replace('\r', "\n");
    let source = canonical_lf(include_str!("fixtures/planning/representative.cr"));
    let actual = compile_with_optimization(&source, "representative.cr", OptimizationLevel::None);
    if std::env::var_os("CRC_UPDATE_ABI_GOLDEN").is_some() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/planning/abi-v3-baseline.c");
        fs::write(path, &actual).expect("ABI v3 golden is updated");
    }
    let expected = canonical_lf(include_str!("fixtures/planning/abi-v3-baseline.c"));
    assert_eq!(actual, expected);
}

#[test]
fn stage4_optimized_levels_reuse_storage_without_changing_none() {
    let source = include_str!("fixtures/planning/representative.cr");
    let baseline = compile_with_optimization(source, "representative.cr", OptimizationLevel::None);
    assert!(!baseline.contains("cr_slot_"));
    let speed = compile_with_optimization(source, "representative.cr", OptimizationLevel::Speed);
    assert!(speed.contains("} cr_slot_0;"));
    assert_ne!(speed, baseline);
    let size = compile_with_optimization(source, "representative.cr", OptimizationLevel::Size);
    let aggressive =
        compile_with_optimization(source, "representative.cr", OptimizationLevel::Aggressive);
    assert!(size.contains("cr_child_value_task cr_child_1;"));
    assert!(size.contains("cr_awaitable cr_await_2;"));
    assert_ne!(size, speed);
    assert_eq!(
        aggressive, size,
        "Aggressive planning must retain the verified size-layout incumbent"
    );
}

#[test]
fn representative_dispatch_matrix_keeps_only_dynamic_awaits_on_vtables() {
    let source = include_str!("fixtures/planning/representative.cr");
    let generated = compile(source, "representative.cr");
    let poll = generated
        .find(
            "cr_poll_status cr_representative_poll(cr_representative_task *ctx, const cr_poll_context *poll_context) {",
        )
        .expect("representative poll is emitted");
    let drop = generated
        .find("void cr_representative_drop(cr_representative_task *ctx) {")
        .expect("representative drop is emitted");
    let poll = &generated[poll..drop];

    assert!(poll.contains("cr_child_value_init(&ctx->cr_v_1_bound"));
    assert!(poll.contains("cr_child_value_poll(&ctx->cr_v_1_bound, poll_context)"));
    assert!(poll.contains("cr_child_value_init(&ctx->cr_child_1"));
    assert!(poll.contains("cr_child_value_poll(&ctx->cr_child_1, poll_context)"));
    assert!(!poll.contains("cr_child_value_create("));
    assert!(!poll.contains("cr_child_value_into_awaitable("));
    assert!(poll.contains("ctx->cr_await_2 = external_value("));
    assert!(poll.contains("ctx->cr_await_2.vtable->poll("));
}
