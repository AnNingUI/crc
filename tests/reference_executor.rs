use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::executor_runtime::portable_artifacts;
use crc_lib::runtime_abi::runtime_header;
use crc_lib::waker_abi::waker_header;

const WASI_SDK_VERSION: &str = include_str!("../tools/wasi-sdk.version");
const WASM_TOOLS_VERSION: &str = include_str!("../tools/wasm-tools.version");
const LIFECYCLE_SOURCE: &str = include_str!("fixtures/waker/executor_lifecycle.c");
const ALLOCATOR_HOOKS: &str = include_str!("fixtures/waker/executor_allocator_hooks.h");

const FIXTURE_SOURCE: &str = r#"
#include "cr_executor.h"

#include <assert.h>
#include <stdint.h>
#include <string.h>

enum test_mode {
    TEST_READY,
    TEST_YIELD_READY,
    TEST_PENDING_WAKE,
    TEST_PENDING_FOREVER,
    TEST_ERROR,
    TEST_CANCELED,
    TEST_INVALID_STATUS,
    TEST_ALIGNED,
    TEST_VOID
};

typedef struct test_state {
    int id;
    enum test_mode mode;
    int polls;
    int drops;
    cr_error error;
    cr_waker retained_waker;
} test_state;

typedef struct test_root {
    test_state state;
    cr_awaitable_vtable vtable;
} test_root;

typedef struct observation {
    int id;
    cr_poll_status status;
    int value;
    int error;
} observation;

typedef struct observation_log {
    observation entries[32];
    size_t count;
} observation_log;

typedef struct observer_binding {
    observation_log *log;
    int id;
    size_t value_size;
    size_t value_align;
} observer_binding;

static cr_poll_status test_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    test_state *state = (test_state *)raw;
    state->polls++;
    assert(poll_context != NULL);
    assert(
        (poll_context->available_capabilities & CR_POLL_CAP_WAKER) != 0u
    );
    assert(cr_waker_is_valid(poll_context->waker));

    if (state->mode == TEST_READY) {
        *(int *)out_value = state->id * 10;
        return CR_POLL_READY;
    }
    if (state->mode == TEST_YIELD_READY) {
        *(int *)out_value = state->id * 10 + state->polls;
        return state->polls == 1 ? CR_POLL_YIELDED : CR_POLL_READY;
    }
    if (state->mode == TEST_PENDING_WAKE) {
        if (state->polls == 1) {
            assert(cr_waker_clone(
                poll_context->waker,
                &state->retained_waker
            ));
            cr_waker_wake(&state->retained_waker);
            cr_waker_wake(&state->retained_waker);
            return CR_POLL_PENDING;
        }
        *(int *)out_value = state->id * 10;
        return CR_POLL_READY;
    }
    if (state->mode == TEST_PENDING_FOREVER) {
        if (!cr_waker_is_valid(&state->retained_waker)) {
            assert(cr_waker_clone(
                poll_context->waker,
                &state->retained_waker
            ));
        }
        return CR_POLL_PENDING;
    }
    if (state->mode == TEST_ERROR) {
        state->error = (cr_error){700 + state->id, "test root error"};
        return CR_POLL_ERROR;
    }
    if (state->mode == TEST_CANCELED) return CR_POLL_CANCELED;
    if (state->mode == TEST_INVALID_STATUS) return 99u;
    if (state->mode == TEST_ALIGNED) {
        assert(out_value != NULL);
        assert(((uintptr_t)out_value & UINT64_C(63)) == 0u);
        memset(out_value, 0, 64u);
        *(unsigned char *)out_value = 77u;
        return CR_POLL_READY;
    }
    assert(state->mode == TEST_VOID);
    assert(out_value == NULL);
    return CR_POLL_READY;
}

static const cr_error *test_error(const void *raw) {
    const test_state *state = (const test_state *)raw;
    return state->error.code != 0 ? &state->error : NULL;
}

static void test_drop(void *raw) {
    test_state *state = (test_state *)raw;
    state->drops++;
    cr_waker_drop(&state->retained_waker);
}

static void test_root_init(
    test_root *root,
    int id,
    enum test_mode mode,
    size_t value_size,
    size_t value_align
) {
    memset(root, 0, sizeof(*root));
    root->state.id = id;
    root->state.mode = mode;
    root->vtable = (cr_awaitable_vtable){
        CR_AWAITABLE_VTABLE_ABI_VERSION,
        sizeof(cr_awaitable_vtable),
        mode == TEST_YIELD_READY ? CR_AWAITABLE_CAN_YIELD : 0u,
        CR_POLL_CAP_WAKER,
        test_poll,
        test_error,
        test_drop,
        value_size,
        value_align
    };
}

static cr_awaitable test_root_awaitable(test_root *root) {
    return (cr_awaitable){&root->state, &root->vtable};
}

static void observe(
    void *raw,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    observer_binding *binding = (observer_binding *)raw;
    observation *entry;
    assert(binding->log->count < 32u);
    entry = &binding->log->entries[binding->log->count++];
    entry->id = binding->id;
    entry->status = status;
    entry->value = 0;
    entry->error = 0;

    if (status == CR_POLL_READY || status == CR_POLL_YIELDED) {
        if (binding->value_size == 0u) {
            assert(value == NULL);
        } else {
            assert(value != NULL);
            assert(
                ((uintptr_t)value & (binding->value_align - 1u)) == 0u
            );
            if (binding->value_size == sizeof(int)) {
                entry->value = *(const int *)value;
            } else {
                entry->value = *(const unsigned char *)value;
            }
        }
        assert(error == NULL);
    } else if (status == CR_POLL_ERROR) {
        assert(value == NULL);
        assert(error != NULL);
        entry->error = error->code;
    } else {
        assert(status == CR_POLL_CANCELED);
        assert(value == NULL);
        assert(error == NULL);
    }
}

static observer_binding bind_observer(
    observation_log *log,
    int id,
    size_t value_size,
    size_t value_align
) {
    return (observer_binding){log, id, value_size, value_align};
}

static void assert_source_moved(const cr_awaitable *source) {
    assert(source->state == NULL);
    assert(source->vtable == NULL);
}

static void test_fifo_yield_coalescing_and_terminal_statuses(void) {
    cr_error error = {99, "unchanged"};
    cr_executor *executor = cr_executor_create_single(&error);
    assert(executor != NULL);
    assert(error.code == 0 && error.message == NULL);
    assert(!cr_executor_wait_one(executor));

    test_root roots[6];
    enum test_mode modes[6] = {
        TEST_YIELD_READY,
        TEST_READY,
        TEST_PENDING_WAKE,
        TEST_ERROR,
        TEST_CANCELED,
        TEST_INVALID_STATUS
    };
    observation_log log = {{{0}}, 0u};
    observer_binding bindings[6];
    cr_executor_task *tickets[6] = {NULL};

    for (int index = 0; index < 6; index++) {
        int id = index + 1;
        test_root_init(
            &roots[index],
            id,
            modes[index],
            sizeof(int),
            _Alignof(int)
        );
        bindings[index] = bind_observer(
            &log,
            id,
            sizeof(int),
            _Alignof(int)
        );
        cr_awaitable source = test_root_awaitable(&roots[index]);
        assert(cr_executor_spawn(
            executor,
            &source,
            observe,
            &bindings[index],
            &error,
            &tickets[index]
        ));
        assert_source_moved(&source);
    }

    assert(cr_executor_run_ready(executor) == 8u);
    assert(log.count == 7u);
    assert(log.entries[0].id == 1);
    assert(log.entries[0].status == CR_POLL_YIELDED);
    assert(log.entries[0].value == 11);
    assert(log.entries[1].id == 2);
    assert(log.entries[1].status == CR_POLL_READY);
    assert(log.entries[2].id == 4);
    assert(log.entries[2].status == CR_POLL_ERROR);
    assert(log.entries[2].error == 704);
    assert(log.entries[3].id == 5);
    assert(log.entries[3].status == CR_POLL_CANCELED);
    assert(log.entries[4].id == 6);
    assert(log.entries[4].status == CR_POLL_ERROR);
    assert(log.entries[4].error == CR_ERROR_INVALID_POLL_STATUS);
    assert(log.entries[5].id == 1);
    assert(log.entries[5].status == CR_POLL_READY);
    assert(log.entries[5].value == 12);
    assert(log.entries[6].id == 3);
    assert(log.entries[6].status == CR_POLL_READY);
    assert(log.entries[6].value == 30);

    assert(roots[0].state.polls == 2);
    assert(roots[2].state.polls == 2);
    for (int index = 0; index < 6; index++) {
        assert(roots[index].state.drops == 1);
        cr_executor_task_release(tickets[index]);
    }
    cr_executor_destroy(executor);
}

static void test_aligned_and_void_results(void) {
    cr_error error = {0};
    cr_executor *executor = cr_executor_create_single(&error);
    test_root aligned;
    test_root void_root;
    observation_log log = {{{0}}, 0u};
    observer_binding aligned_binding = bind_observer(&log, 7, 64u, 64u);
    observer_binding void_binding = bind_observer(&log, 8, 0u, 0u);
    cr_executor_task *aligned_ticket = NULL;
    cr_executor_task *void_ticket = NULL;

    assert(executor != NULL);
    test_root_init(&aligned, 7, TEST_ALIGNED, 64u, 64u);
    test_root_init(&void_root, 8, TEST_VOID, 0u, 0u);
    cr_awaitable aligned_source = test_root_awaitable(&aligned);
    cr_awaitable void_source = test_root_awaitable(&void_root);
    assert(cr_executor_spawn(
        executor,
        &aligned_source,
        observe,
        &aligned_binding,
        &error,
        &aligned_ticket
    ));
    assert(cr_executor_spawn(
        executor,
        &void_source,
        observe,
        &void_binding,
        &error,
        &void_ticket
    ));
    assert(cr_executor_run_ready(executor) == 2u);
    assert(log.count == 2u);
    assert(log.entries[0].id == 7 && log.entries[0].value == 77);
    assert(log.entries[1].id == 8 && log.entries[1].value == 0);
    assert(aligned.state.drops == 1);
    assert(void_root.state.drops == 1);
    cr_executor_task_release(aligned_ticket);
    cr_executor_task_release(void_ticket);
    cr_executor_destroy(executor);
}

static void test_spawn_validation_and_move_ownership(void) {
    cr_error error = {0};
    cr_executor *executor = cr_executor_create_single(&error);
    test_root root;
    cr_executor_task *ticket = (cr_executor_task *)(uintptr_t)1u;
    assert(executor != NULL);

    test_root_init(&root, 9, TEST_READY, sizeof(int), _Alignof(int));
    root.vtable.abi_version = 0u;
    cr_awaitable invalid = test_root_awaitable(&root);
    assert(!cr_executor_spawn(
        executor, &invalid, NULL, NULL, &error, &ticket
    ));
    assert(error.code == CR_ERROR_INVALID_AWAITABLE_ABI);
    assert(ticket == NULL);
    assert(invalid.state == &root.state && invalid.vtable == &root.vtable);
    assert(root.state.drops == 0);

    root.vtable.abi_version = CR_AWAITABLE_VTABLE_ABI_VERSION;
    root.vtable.poll = NULL;
    ticket = (cr_executor_task *)(uintptr_t)1u;
    assert(!cr_executor_spawn(
        executor, &invalid, NULL, NULL, &error, &ticket
    ));
    assert(error.code == CR_ERROR_MISSING_AWAITABLE_CALLBACK);
    assert(ticket == NULL);
    assert(invalid.state == &root.state && invalid.vtable == &root.vtable);

    root.vtable.poll = test_poll;
    root.vtable.value_align = 3u;
    assert(!cr_executor_spawn(
        executor, &invalid, NULL, NULL, &error, &ticket
    ));
    assert(error.code == CR_ERROR_AWAITABLE_LAYOUT_MISMATCH);
    assert(invalid.state == &root.state && invalid.vtable == &root.vtable);

    root.vtable.value_size = SIZE_MAX;
    root.vtable.value_align = 2u;
    assert(!cr_executor_spawn(
        executor, &invalid, NULL, NULL, &error, &ticket
    ));
    assert(error.code == CR_ERROR_AWAITABLE_LAYOUT_MISMATCH);
    assert(invalid.state == &root.state && invalid.vtable == &root.vtable);

    root.vtable.value_size = sizeof(int);
    root.vtable.value_align = _Alignof(int);
    assert(cr_executor_spawn(
        executor, &invalid, NULL, NULL, &error, &ticket
    ));
    assert_source_moved(&invalid);
    assert(cr_executor_run_ready(executor) == 1u);
    assert(root.state.drops == 1);
    cr_executor_task_release(ticket);

    cr_executor_shutdown(executor);
    test_root closed_root;
    test_root_init(
        &closed_root, 10, TEST_READY, sizeof(int), _Alignof(int)
    );
    cr_awaitable closed_source = test_root_awaitable(&closed_root);
    assert(!cr_executor_spawn(
        executor, &closed_source, NULL, NULL, &error, &ticket
    ));
    assert(error.code == CR_EXECUTOR_ERROR_CLOSED);
    assert(closed_source.state == &closed_root.state);
    assert(closed_root.state.drops == 0);
    cr_executor_destroy(executor);
}

static void test_cancel_and_requested_shutdown(void) {
    cr_error error = {0};
    cr_executor *executor = cr_executor_create_single(&error);
    test_root first;
    test_root second;
    observation_log log = {{{0}}, 0u};
    observer_binding first_binding = bind_observer(
        &log, 11, sizeof(int), _Alignof(int)
    );
    observer_binding second_binding = bind_observer(
        &log, 12, sizeof(int), _Alignof(int)
    );
    cr_executor_task *first_ticket = NULL;
    cr_executor_task *second_ticket = NULL;

    assert(executor != NULL);
    test_root_init(
        &first, 11, TEST_PENDING_FOREVER, sizeof(int), _Alignof(int)
    );
    test_root_init(
        &second, 12, TEST_PENDING_FOREVER, sizeof(int), _Alignof(int)
    );
    cr_awaitable first_source = test_root_awaitable(&first);
    cr_awaitable second_source = test_root_awaitable(&second);
    assert(cr_executor_spawn(
        executor,
        &first_source,
        observe,
        &first_binding,
        &error,
        &first_ticket
    ));
    assert(cr_executor_spawn(
        executor,
        &second_source,
        observe,
        &second_binding,
        &error,
        &second_ticket
    ));
    assert(cr_executor_run_ready(executor) == 2u);
    assert(log.count == 0u);

    cr_executor_cancel(first_ticket);
    cr_executor_cancel(first_ticket);
    assert(log.count == 1u);
    assert(log.entries[0].id == 11);
    assert(log.entries[0].status == CR_POLL_CANCELED);
    assert(first.state.drops == 1);

    cr_executor_request_shutdown(executor);
    assert(cr_executor_run_ready(executor) == 0u);
    assert(log.count == 2u);
    assert(log.entries[1].id == 12);
    assert(log.entries[1].status == CR_POLL_CANCELED);
    assert(second.state.drops == 1);

    cr_executor_task_release(first_ticket);
    cr_executor_task_release(second_ticket);
    cr_executor_destroy(executor);
}

int main(void) {
    cr_error unsupported = {0};
    assert(cr_executor_create_threaded(&unsupported) == NULL);
    assert(unsupported.code == CR_EXECUTOR_ERROR_UNSUPPORTED);
    test_fifo_yield_coalescing_and_terminal_statuses();
    test_aligned_and_void_results();
    test_spawn_validation_and_move_ownership();
    test_cancel_and_requested_shutdown();
    return 0;
}
"#;

fn run(command: &mut Command) -> Output {
    command.output().expect("command starts")
}

fn output_text(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

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

fn write_runtime(directory: &Path) {
    fs::create_dir_all(directory.join("include")).expect("include directory");
    fs::create_dir_all(directory.join("runtime")).expect("runtime directory");
    fs::write(directory.join("include/cr_runtime.h"), runtime_header()).expect("runtime header");
    fs::write(directory.join("include/cr_waker.h"), waker_header()).expect("Waker header");
    for artifact in portable_artifacts() {
        let path = directory.join(artifact.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("artifact parent");
        }
        fs::write(path, artifact.contents).expect("executor artifact");
    }
    fs::write(directory.join("fixture.c"), FIXTURE_SOURCE).expect("executor fixture");
}

fn write_lifecycle_runtime(directory: &Path) {
    write_runtime(directory);
    fs::write(directory.join("lifecycle.c"), LIFECYCLE_SOURCE).expect("lifecycle fixture");
    fs::write(directory.join("allocator_hooks.h"), ALLOCATOR_HOOKS)
        .expect("allocator hook declarations");
}

fn add_allocator_overrides(command: &mut Command) {
    command.args([
        "-include",
        "allocator_hooks.h",
        "-DCR_EXECUTOR_MALLOC=test_executor_malloc",
        "-DCR_EXECUTOR_CALLOC=test_executor_calloc",
        "-DCR_EXECUTOR_FREE=test_executor_free",
    ]);
}

fn runtime_source_paths() -> Vec<&'static str> {
    portable_artifacts()
        .iter()
        .filter(|artifact| artifact.is_source)
        .map(|artifact| artifact.path)
        .collect()
}

fn required_wasm() -> bool {
    env::var("CRC_REQUIRE_WASM").is_ok_and(|value| value == "1")
}

fn discover_wasm_tools() -> Option<(PathBuf, PathBuf)> {
    let Some(wasi_root) = env::var_os("WASI_SDK_PATH").map(PathBuf::from) else {
        if required_wasm() {
            panic!("CRC_REQUIRE_WASM=1 requires WASI_SDK_PATH");
        }
        eprintln!("skipping executor WASI gate: WASI_SDK_PATH is not set");
        return None;
    };
    let wasm_tools = PathBuf::from(if cfg!(windows) {
        "wasm-tools.exe"
    } else {
        "wasm-tools"
    });
    let version = run(Command::new(&wasm_tools).arg("--version"));
    if !version.status.success() {
        if required_wasm() {
            panic!("wasm-tools is required: {}", output_text(&version));
        }
        eprintln!("skipping executor WASI gate: wasm-tools is unavailable");
        return None;
    }
    Some((wasi_root, wasm_tools))
}

#[test]
fn single_thread_executor_runs_portable_fifo_with_clang_and_gcc() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");

    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_runtime(directory.path());
        let executable = if cfg!(windows) {
            "reference-executor.exe"
        } else {
            "reference-executor"
        };
        let mut command = Command::new(compiler);
        command.args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fno-inline",
            "fixture.c",
        ]);
        command.args(runtime_source_paths());
        command
            .args(["-I", "include", "-I", "runtime", "-o"])
            .arg(executable)
            .current_dir(directory.path());
        let compilation = run(&mut command);
        assert!(
            compilation.status.success(),
            "{compiler}: {}",
            output_text(&compilation)
        );
        let execution = run(&mut Command::new(directory.path().join(executable)));
        assert!(
            execution.status.success(),
            "{compiler}: {}",
            output_text(&execution)
        );
    }
}

#[test]
fn single_thread_executor_links_and_validates_for_wasm32_wasi() {
    let Some((wasi_root, wasm_tools)) = discover_wasm_tools() else {
        return;
    };
    let installed_wasi = fs::read_to_string(wasi_root.join("VERSION")).expect("WASI VERSION file");
    assert_eq!(installed_wasi.lines().next(), Some(WASI_SDK_VERSION.trim()));
    let tools_version = run(Command::new(&wasm_tools).arg("--version"));
    assert!(
        tools_version.status.success(),
        "{}",
        output_text(&tools_version)
    );
    assert!(
        String::from_utf8_lossy(&tools_version.stdout)
            .starts_with(&format!("wasm-tools {} ", WASM_TOOLS_VERSION.trim()))
    );
    let clang = wasi_root
        .join("bin")
        .join(if cfg!(windows) { "clang.exe" } else { "clang" });
    assert!(clang.is_file(), "missing WASI Clang: {}", clang.display());

    let directory = tempfile::tempdir().expect("temporary directory");
    write_runtime(directory.path());
    let mut command = Command::new(clang);
    command
        .arg("--target=wasm32-wasi")
        .arg(format!(
            "--sysroot={}",
            wasi_root.join("share/wasi-sysroot").display()
        ))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "fixture.c"]);
    command.args(runtime_source_paths());
    command
        .args(["-I", "include", "-I", "runtime", "-o", "executor.wasm"])
        .current_dir(directory.path());
    let compilation = run(&mut command);
    assert!(
        compilation.status.success(),
        "{}",
        output_text(&compilation)
    );
    let validation = run(Command::new(&wasm_tools)
        .args(["validate", "executor.wasm"])
        .current_dir(directory.path()));
    assert!(validation.status.success(), "{}", output_text(&validation));
    assert!(directory.path().join(OsStr::new("executor.wasm")).is_file());
}

#[test]
fn lifecycle_control_block_outlives_payload_ticket_queue_and_waker_edges() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");

    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_lifecycle_runtime(directory.path());
        let executable = if cfg!(windows) {
            "executor-lifecycle.exe"
        } else {
            "executor-lifecycle"
        };
        let mut command = Command::new(compiler);
        command.args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-fno-inline"]);
        add_allocator_overrides(&mut command);
        command.arg("lifecycle.c");
        command.args(runtime_source_paths());
        command
            .args(["-I", "include", "-I", "runtime", "-o"])
            .arg(executable)
            .current_dir(directory.path());
        let compilation = run(&mut command);
        assert!(
            compilation.status.success(),
            "{compiler}: {}",
            output_text(&compilation)
        );
        let execution = run(&mut Command::new(directory.path().join(executable)));
        assert!(
            execution.status.success(),
            "{compiler}: {}",
            output_text(&execution)
        );
    }
}

#[test]
fn lifecycle_fixture_links_and_validates_for_wasm32_wasi() {
    let Some((wasi_root, wasm_tools)) = discover_wasm_tools() else {
        return;
    };
    let clang = wasi_root
        .join("bin")
        .join(if cfg!(windows) { "clang.exe" } else { "clang" });
    assert!(clang.is_file(), "missing WASI Clang: {}", clang.display());

    let directory = tempfile::tempdir().expect("temporary directory");
    write_lifecycle_runtime(directory.path());
    let mut command = Command::new(clang);
    command
        .arg("--target=wasm32-wasi")
        .arg(format!(
            "--sysroot={}",
            wasi_root.join("share/wasi-sysroot").display()
        ))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"]);
    add_allocator_overrides(&mut command);
    command.arg("lifecycle.c");
    command.args(runtime_source_paths());
    command
        .args(["-I", "include", "-I", "runtime", "-o", "lifecycle.wasm"])
        .current_dir(directory.path());
    let compilation = run(&mut command);
    assert!(
        compilation.status.success(),
        "{}",
        output_text(&compilation)
    );
    let validation = run(Command::new(&wasm_tools)
        .args(["validate", "lifecycle.wasm"])
        .current_dir(directory.path()));
    assert!(validation.status.success(), "{}", output_text(&validation));
}
