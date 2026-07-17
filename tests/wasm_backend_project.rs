use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const WASI_SDK_VERSION: &str = include_str!("../tools/wasi-sdk.version");
const WASM_TOOLS_VERSION: &str = include_str!("../tools/wasm-tools.version");

const PROJECT_HEADER: &str = r#"
#ifndef CR_WASM_BACKEND_MAIN_H
#define CR_WASM_BACKEND_MAIN_H

cr_awaitable backend_receive(void);
__async unsigned long long backend_root(void);
__async int aggressive_layout(void);

#endif
"#;

const PROJECT_SOURCE: &str = r#"
#include "main.hr"

static __async unsigned long long backend_child(void) {
    return __await backend_receive();
}

__async unsigned long long backend_root(void) {
    return __await backend_child();
}

static void consume_layout_value(int value) {
    (void)value;
}

__async int aggressive_layout(void) {
    char first[64] = {1};
    __yield 1;
    consume_layout_value(first[0]);
    long long second[8] = {2};
    __yield 2;
    return (int)second[0];
}
"#;

const APPLICATION_SOURCE: &str = r#"
#include "main.h"
#include "cr_backend_internal.h"
#include "cr_executor.h"

#include <assert.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>

typedef union opaque_storage {
    max_align_t alignment;
    unsigned char bytes[1024];
} opaque_storage;

typedef struct receive_slot {
    opaque_storage awaitable;
    opaque_storage operation;
    unsigned char buffer[32];
} receive_slot;

typedef struct observation {
    unsigned int calls;
    cr_poll_status status;
    unsigned long long value;
} observation;

static cr_awaitable pending_receive;
static unsigned int trace_counts[10];

cr_awaitable backend_receive(void) {
    cr_awaitable result = pending_receive;
    pending_receive = (cr_awaitable){NULL, NULL};
    return result;
}

static cr_net_receive_awaitable_state *awaitable_state(
    receive_slot *slot
) {
    return (cr_net_receive_awaitable_state *)(void *)slot->awaitable.bytes;
}

static cr_net_receive_operation *operation_state(receive_slot *slot) {
    return (cr_net_receive_operation *)(void *)slot->operation.bytes;
}

static cr_native_socket_handle memory_socket(uintptr_t value) {
    return (cr_native_socket_handle){
        CR_NATIVE_SOCKET_MEMORY,
        UINT32_C(0),
        value
    };
}

static void record_trace(
    void *context,
    cr_backend_memory_trace_event event,
    const cr_net_receive_operation *operation,
    uint64_t generation
) {
    unsigned int *counts = (unsigned int *)context;
    (void)operation;
    assert(event < UINT32_C(10));
    assert(generation != UINT64_C(0) ||
        event == CR_BACKEND_MEMORY_TRACE_INTERRUPT_CONSUMED ||
        event == CR_BACKEND_MEMORY_TRACE_SHUTDOWN);
    counts[event]++;
}

static void clear_trace(void) {
    memset(trace_counts, 0, sizeof(trace_counts));
}

static void observe_root(
    void *context,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    observation *result = (observation *)context;
    result->calls++;
    result->status = status;
    if (status == CR_POLL_READY) {
        assert(value != NULL);
        assert(error == NULL);
        result->value = *(const unsigned long long *)value;
    } else {
        assert(status == CR_POLL_CANCELED);
        assert(value == NULL);
        assert(error == NULL);
    }
}

static void initialize_receive(
    cr_backend *backend,
    const cr_net_extension_desc *net,
    receive_slot *slot,
    uintptr_t socket_value
) {
    cr_error error = {0, NULL};
    cr_storage_layout state_layout =
        cr_net_receive_awaitable_state_layout();

    assert(pending_receive.state == NULL);
    assert(pending_receive.vtable == NULL);
    assert(cr_storage_layout_is_valid(&state_layout));
    assert(state_layout.size <= sizeof(slot->awaitable));
    assert(state_layout.alignment <= _Alignof(opaque_storage));
    assert(net->receive_operation_layout.size <= sizeof(slot->operation));
    assert(
        net->receive_operation_layout.alignment <= _Alignof(opaque_storage)
    );
    assert(cr_net_receive_awaitable_initialize(
        awaitable_state(slot),
        sizeof(slot->awaitable),
        backend,
        net,
        operation_state(slot),
        sizeof(slot->operation),
        memory_socket(socket_value),
        slot->buffer,
        sizeof(slot->buffer),
        &pending_receive,
        &error
    ));
}

static cr_executor_task *spawn_root(
    cr_executor *executor,
    observation *result
) {
    cr_error error = {0, NULL};
    cr_backend_root_task *task = cr_backend_root_create(&error);
    cr_executor_task *ticket = NULL;
    cr_awaitable root;

    assert(task != NULL);
    root = cr_backend_root_into_awaitable(task);
    assert(cr_executor_spawn(
        executor,
        &root,
        observe_root,
        result,
        &error,
        &ticket
    ));
    assert(root.state == NULL && root.vtable == NULL);
    assert(cr_executor_run_ready(executor) == 1u);
    assert(result->calls == 0u);
    return ticket;
}

static void run_completion(
    cr_backend *backend,
    const cr_net_extension_desc *net,
    cr_executor *executor
) {
    cr_backend_pump_result pump;
    cr_net_error net_error;
    receive_slot slot = {0};
    observation result = {0, CR_POLL_PENDING, 0u};
    cr_executor_task *ticket;

    clear_trace();
    initialize_receive(backend, net, &slot, (uintptr_t)41u);
    ticket = spawn_root(executor, &result);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_INITIALIZED] == 1u);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_SUBMITTED] == 1u);
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);
    assert(pump.events_dispatched == UINT32_C(0));
    assert(cr_backend_memory_complete_ready(
        backend,
        operation_state(&slot),
        "wasm",
        UINT64_C(4),
        &net_error
    ));
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_PROGRESS);
    assert(pump.events_dispatched == UINT32_C(1));
    assert(cr_executor_run_ready(executor) == 1u);
    assert(result.calls == 1u);
    assert(result.status == CR_POLL_READY);
    assert(result.value == 4u);
    assert(memcmp(slot.buffer, "wasm", 4u) == 0);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_TERMINAL_QUEUED] == 1u);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_TERMINAL_CALLBACK] == 1u);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_QUIESCENT] == 1u);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_DESTROYED] == 1u);
    cr_executor_task_release(ticket);
    puts("complete bytes=4 buffer=wasm");
}

static void run_interrupt(cr_backend *backend) {
    cr_backend_error backend_error;
    cr_backend_pump_result pump;

    clear_trace();
    assert(cr_backend_interrupt(backend, &backend_error));
    assert(cr_backend_interrupt(backend, &backend_error));
    assert(cr_backend_pump(backend, UINT64_MAX, UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_INTERRUPTED);
    assert(pump.events_dispatched == UINT32_C(1));
    assert(
        trace_counts[CR_BACKEND_MEMORY_TRACE_INTERRUPT_CONSUMED] == 1u
    );
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_TIMEOUT);
    puts("interrupt events=1");
}

static void run_cancellation(
    cr_backend *backend,
    const cr_net_extension_desc *net,
    cr_executor *executor
) {
    cr_backend_pump_result pump;
    cr_error error = {0, NULL};
    receive_slot slot = {0};
    observation result = {0, CR_POLL_PENDING, 0u};
    cr_executor_task *ticket;
    const cr_net_receive_completion *completion;

    clear_trace();
    initialize_receive(backend, net, &slot, (uintptr_t)42u);
    ticket = spawn_root(executor, &result);
    assert(cr_net_receive_awaitable_cancel(awaitable_state(&slot), &error));
    assert(cr_net_receive_awaitable_cancel(awaitable_state(&slot), &error));
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_CANCEL_REQUESTED] == 1u);
    assert(cr_backend_pump(backend, UINT64_C(0), UINT32_C(1), &pump));
    assert(pump.reason == CR_BACKEND_PUMP_PROGRESS);
    assert(pump.events_dispatched == UINT32_C(1));
    assert(cr_executor_run_ready(executor) == 1u);
    assert(result.calls == 1u);
    assert(result.status == CR_POLL_CANCELED);
    completion = cr_net_receive_awaitable_completion(
        awaitable_state(&slot)
    );
    assert(completion != NULL);
    assert(completion->terminal_kind == CR_NET_RECEIVE_CANCELED);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_TERMINAL_CALLBACK] == 1u);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_QUIESCENT] == 1u);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_DESTROYED] == 1u);
    cr_executor_task_release(ticket);
    puts("cancel terminal=canceled");
}

static void run_task_drop(
    cr_backend *backend,
    const cr_net_extension_desc *net,
    cr_executor *executor
) {
    cr_net_error net_error;
    receive_slot slot = {0};
    observation result = {0, CR_POLL_PENDING, 0u};
    cr_executor_task *ticket;

    clear_trace();
    initialize_receive(backend, net, &slot, (uintptr_t)43u);
    ticket = spawn_root(executor, &result);
    cr_executor_cancel(ticket);
    cr_executor_cancel(ticket);
    assert(result.calls == 1u);
    assert(result.status == CR_POLL_CANCELED);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_CANCEL_REQUESTED] == 1u);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_QUIESCENT] == 1u);
    assert(trace_counts[CR_BACKEND_MEMORY_TRACE_DESTROYED] == 1u);
    assert(!cr_backend_memory_complete_ready(
        backend,
        operation_state(&slot),
        "late",
        UINT64_C(4),
        &net_error
    ));
    cr_executor_task_release(ticket);
    puts("drop quiescent=1 cleanup=1");
}

int main(void) {
    const cr_extension_id net_id = CR_NET_RECEIVE_EXTENSION_ID_INIT;
    cr_backend *backend = NULL;
    cr_backend_error backend_error;
    const cr_backend_extension_desc *base;
    const cr_net_extension_desc *net;
    cr_error executor_error = {0, NULL};
    cr_executor *executor;

    assert(cr_backend_create(
        &cr_backend_memory_provider_desc,
        &backend,
        &backend_error
    ));
    base = cr_backend_query_extension(
        backend,
        net_id,
        CR_NET_ABI_VERSION,
        &backend_error
    );
    assert(base != NULL);
    net = (const cr_net_extension_desc *)(const void *)base;
    assert(cr_net_extension_desc_is_compatible(net));
    assert(cr_backend_memory_set_trace(
        backend,
        record_trace,
        trace_counts,
        &backend_error
    ));
    executor = cr_executor_create_single(&executor_error);
    assert(executor != NULL);

    run_completion(backend, net, executor);
    run_interrupt(backend);
    run_cancellation(backend, net, executor);
    run_task_drop(backend, net, executor);

    cr_executor_destroy(executor);
    assert(cr_backend_destroy(backend, &backend_error));
    return 0;
}
"#;

const EXPECTED_TRANSCRIPT: &str = "\
complete bytes=4 buffer=wasm
interrupt events=1
cancel terminal=canceled
drop quiescent=1 cleanup=1
";

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

fn required() -> bool {
    env::var("CRC_REQUIRE_WASM").is_ok_and(|value| value == "1")
}

fn clang_name() -> &'static str {
    if cfg!(windows) { "clang.exe" } else { "clang" }
}

fn discover_toolchain() -> Option<(PathBuf, PathBuf)> {
    let Some(wasi_root) = env::var_os("WASI_SDK_PATH").map(PathBuf::from) else {
        if required() {
            panic!("CRC_REQUIRE_WASM=1 requires WASI_SDK_PATH");
        }
        eprintln!("skipping pinned Wasm sub-gate: WASI_SDK_PATH is not set");
        return None;
    };
    let clang = wasi_root.join("bin").join(clang_name());
    if !clang.is_file() {
        if required() {
            panic!("WASI SDK Clang is missing: {}", clang.display());
        }
        eprintln!(
            "skipping pinned Wasm sub-gate: {} is missing",
            clang.display()
        );
        return None;
    }
    let wasm_tools = PathBuf::from(if cfg!(windows) {
        "wasm-tools.exe"
    } else {
        "wasm-tools"
    });
    let version = run(Command::new(&wasm_tools).arg("--version"));
    if !version.status.success() {
        if required() {
            panic!("wasm-tools is required: {}", output_text(&version));
        }
        eprintln!("skipping pinned Wasm sub-gate: wasm-tools is unavailable");
        return None;
    }
    Some((wasi_root, wasm_tools))
}

fn assert_pinned_versions(wasi_root: &Path, wasm_tools: &Path) {
    let expected_wasi = WASI_SDK_VERSION.trim();
    let expected_tools = WASM_TOOLS_VERSION.trim();
    assert!(!expected_wasi.is_empty(), "WASI SDK version pin is empty");
    assert!(
        !expected_tools.is_empty(),
        "wasm-tools version pin is empty"
    );

    let installed_wasi =
        fs::read_to_string(wasi_root.join("VERSION")).expect("read WASI SDK VERSION");
    assert_eq!(
        installed_wasi.lines().next(),
        Some(expected_wasi),
        "WASI SDK doesn't match tools/wasi-sdk.version"
    );
    let output = run(Command::new(wasm_tools).arg("--version"));
    assert!(output.status.success(), "{}", output_text(&output));
    let installed_tools = String::from_utf8_lossy(&output.stdout);
    assert!(
        installed_tools.starts_with(&format!("wasm-tools {expected_tools} ")),
        "wasm-tools doesn't match tools/wasm-tools.version: {installed_tools}"
    );
}

fn create_generated_project(root: &Path) -> PathBuf {
    let project = root.join("wasm-backend");
    let created = run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .args(["create", "wasm-backend"])
        .current_dir(root));
    assert!(
        created.status.success(),
        "project creation: {}",
        output_text(&created)
    );

    let config_path = project.join("crc.toml");
    let config = fs::read_to_string(&config_path).expect("generated crc.toml");
    let config = config
        .replacen("target = \"host\"", "target = \"wasm32-wasi\"", 1)
        .replacen(
            "optimization = \"speed\"",
            "optimization = \"aggressive\"",
            1,
        )
        .replacen("executor = \"manual\"", "executor = \"single-thread\"", 1)
        .replacen("backends = []", "backends = [\"memory-conformance\"]", 1);
    assert!(config.contains("target = \"wasm32-wasi\""));
    assert!(config.contains("optimization = \"aggressive\""));
    assert!(config.contains("executor = \"single-thread\""));
    assert!(config.contains("backends = [\"memory-conformance\"]"));
    assert!(config.contains("computed_goto = false"));
    fs::write(&config_path, config).expect("write Wasm Backend config");
    fs::write(project.join("crc/include/main.hr"), PROJECT_HEADER).expect("write project header");
    fs::write(project.join("crc/src/main.cr"), PROJECT_SOURCE).expect("write CR source");
    fs::write(project.join("src/main.c"), APPLICATION_SOURCE).expect("write application source");

    let built = run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));
    assert!(
        built.status.success(),
        "project build: {}",
        output_text(&built)
    );
    project
}

fn generated_sources(project: &Path) -> Vec<PathBuf> {
    let mut sources = walkdir::WalkDir::new(project.join("crc/dist"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| path.extension() == Some(OsStr::new("c")))
        .collect::<Vec<_>>();
    sources.sort();
    sources
}

fn relative_sources(project: &Path, sources: &[PathBuf]) -> Vec<String> {
    let dist = project.join("crc/dist");
    sources
        .iter()
        .map(|path| {
            path.strip_prefix(&dist)
                .expect("generated source is inside dist")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

fn assert_published_artifacts(project: &Path, sources: &[PathBuf]) {
    let dist = project.join("crc/dist");
    assert_eq!(
        relative_sources(project, sources),
        vec![
            "main.c",
            "runtime/cr_backend_common.c",
            "runtime/cr_backend_memory.c",
            "runtime/cr_executor_common.c",
            "runtime/cr_executor_single.c",
            "runtime/cr_executor_threaded_stub.c",
            "runtime/cr_net_recv.c",
        ]
    );
    for required in [
        "include/cr_runtime.h",
        "include/cr_waker.h",
        "include/cr_executor.h",
        "include/cr_backend.h",
        "include/cr_net.h",
        "runtime/cr_executor_internal.h",
        "runtime/cr_backend_internal.h",
    ] {
        assert!(dist.join(required).is_file(), "missing {required}");
    }
    for forbidden in [
        "runtime/cr_backend_iocp.c",
        "runtime/cr_backend_epoll.c",
        "runtime/cr_backend_kqueue.c",
        "runtime/cr_executor_threaded_windows.c",
        "runtime/cr_executor_threaded_posix.c",
        "crc-generated-dependencies.cmake",
    ] {
        assert!(!dist.join(forbidden).exists(), "unexpected {forbidden}");
    }

    let manifest = fs::read_to_string(dist.join("crc-artifacts.json")).expect("artifact manifest");
    for artifact in [
        "include/cr_backend.h",
        "include/cr_net.h",
        "runtime/cr_backend_common.c",
        "runtime/cr_backend_memory.c",
        "runtime/cr_net_recv.c",
    ] {
        assert_eq!(manifest.matches(artifact).count(), 1, "{artifact}");
    }
    assert!(manifest.contains("\"kind\": \"backend-awaitable-source\""));
    assert!(!manifest.contains("\"dependencies\""));

    let meson = fs::read_to_string(dist.join("meson.build")).expect("Meson manifest");
    for source in relative_sources(project, sources) {
        assert_eq!(meson.matches(&format!("'{source}'")).count(), 1, "{source}");
    }
    assert!(meson.ends_with("cr_generated_dependencies = []\n"));
}

fn assert_optimization_and_portability(project: &Path, sources: &[PathBuf]) {
    let generated =
        fs::read_to_string(project.join("crc/dist/main.c")).expect("generated coroutine source");
    let root_poll = generated
        .find("backend_root_poll(")
        .and_then(|start| {
            generated[start..]
                .find("backend_root_drop(")
                .map(|end| &generated[start..start + end])
        })
        .expect("generated Backend root poll body");
    assert!(root_poll.contains("backend_child_poll(&ctx->cr_child_"));
    assert!(!root_poll.contains("backend_child_into_awaitable("));
    assert!(!root_poll.contains("backend_child_as_awaitable("));
    assert!(generated.contains(".vtable->poll("));
    assert!(generated.contains("struct cr_aggressive_layout_task"));
    assert!(generated.contains("union {"));
    assert!(generated.contains("long long"));

    let executor_text = sources
        .iter()
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("cr_executor_"))
        })
        .map(|path| fs::read_to_string(path).expect("executor source"))
        .collect::<String>()
        .to_ascii_lowercase();
    for forbidden in [
        "stdatomic",
        "pthread_",
        "windows.h",
        "interlocked",
        "createthread",
    ] {
        assert!(!executor_text.contains(forbidden), "{forbidden}");
    }
    let memory = fs::read_to_string(project.join("crc/dist/runtime/cr_backend_memory.c"))
        .expect("memory Provider source")
        .to_ascii_lowercase();
    assert!(memory.contains("atomic_"));
    assert!(!memory.contains("pthread_"));
    assert!(!memory.contains("windows.h"));
}

fn available_native_compilers() -> Vec<&'static str> {
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

fn compile_and_run_native(project: &Path, sources: &[PathBuf]) {
    let compilers = available_native_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        let executable = project.join(if cfg!(windows) {
            format!("wasm-backend-{compiler}.exe")
        } else {
            format!("wasm-backend-{compiler}")
        });
        let mut command = Command::new(compiler);
        command.args(["-std=c11", "-Wall", "-Wextra", "-Werror"]);
        command.args(sources);
        command
            .arg(project.join("src/main.c"))
            .arg("-I")
            .arg(project.join("crc/dist/include"))
            .arg("-I")
            .arg(project.join("crc/dist/runtime"))
            .arg("-o")
            .arg(&executable);
        let compiled = run(&mut command);
        assert!(
            compiled.status.success(),
            "native {compiler} compile: {}",
            output_text(&compiled)
        );
        let executed = run(&mut Command::new(&executable));
        assert!(
            executed.status.success(),
            "native {compiler} execute: {}",
            output_text(&executed)
        );
        let transcript = String::from_utf8_lossy(&executed.stdout).replace("\r\n", "\n");
        assert_eq!(transcript, EXPECTED_TRANSCRIPT, "native {compiler}");
    }
}

fn compile_and_validate_wasm(
    project: &Path,
    sources: &[PathBuf],
    wasi_root: &Path,
    wasm_tools: &Path,
    temporary: &Path,
) {
    let clang = wasi_root.join("bin").join(clang_name());
    let sysroot = wasi_root.join("share/wasi-sysroot");
    assert!(
        sysroot.is_dir(),
        "WASI sysroot is missing: {}",
        sysroot.display()
    );
    let mut all_sources = sources.to_vec();
    all_sources.push(project.join("src/main.c"));
    let mut objects = Vec::with_capacity(all_sources.len());

    for (index, source) in all_sources.iter().enumerate() {
        let object = temporary.join(format!("wasm-backend-{index}.o"));
        let compiled = run(Command::new(&clang)
            .arg("--target=wasm32-wasi")
            .arg(format!("--sysroot={}", sysroot.display()))
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
            .arg("-I")
            .arg(project.join("crc/dist/include"))
            .arg("-I")
            .arg(project.join("crc/dist/runtime"))
            .arg("-c")
            .arg(source)
            .arg("-o")
            .arg(&object));
        assert!(
            compiled.status.success(),
            "WASI compile {}: {}",
            source.display(),
            output_text(&compiled)
        );
        assert!(object.is_file(), "missing object for {}", source.display());
        objects.push(object);
    }

    let module = temporary.join("wasm-backend.wasm");
    let mut link = Command::new(&clang);
    link.arg("--target=wasm32-wasi")
        .arg(format!("--sysroot={}", sysroot.display()))
        .args(&objects)
        .arg("-o")
        .arg(&module);
    let linked = run(&mut link);
    assert!(
        linked.status.success(),
        "WASI link: {}",
        output_text(&linked)
    );
    assert!(module.is_file(), "WASI module is missing");

    let validated = run(Command::new(wasm_tools).arg("validate").arg(&module));
    assert!(
        validated.status.success(),
        "wasm-tools validate: {}",
        output_text(&validated)
    );
    let printed = run(Command::new(wasm_tools).arg("print").arg(&module));
    assert!(
        printed.status.success(),
        "wasm-tools print: {}",
        output_text(&printed)
    );
    let wat = String::from_utf8_lossy(&printed.stdout).to_ascii_lowercase();
    let semantic_wat = wat
        .lines()
        .filter(|line| !line.trim_start().starts_with("(@custom"))
        .collect::<Vec<_>>()
        .join("\n");
    let target_features = wat
        .lines()
        .filter(|line| line.contains("@custom \"target_features\""))
        .collect::<Vec<_>>();
    for forbidden in ["atomics", "shared", "thread"] {
        assert!(
            target_features
                .iter()
                .all(|feature| !feature.contains(forbidden)),
            "forbidden Wasm target feature {forbidden}: {target_features:#?}"
        );
    }
    let imports = semantic_wat
        .lines()
        .filter(|line| line.contains("(import"))
        .collect::<Vec<_>>();
    assert!(
        imports
            .iter()
            .all(|import| !import.contains("sock_") && !import.contains("thread")),
        "forbidden socket or thread import: {imports:#?}"
    );
    for memory in semantic_wat.lines().filter(|line| line.contains("(memory")) {
        assert!(
            !memory.contains(" shared"),
            "forbidden shared Wasm memory: {memory}"
        );
    }
    for forbidden in [
        "atomic.",
        ".atomic",
        "cr_backend_iocp",
        "cr_backend_epoll",
        "cr_backend_kqueue",
        "winsock",
    ] {
        let matching_lines = semantic_wat
            .lines()
            .filter(|line| line.contains(forbidden))
            .collect::<Vec<_>>();
        assert!(
            matching_lines.is_empty(),
            "forbidden Wasm feature {forbidden}: {matching_lines:#?}"
        );
    }
}

#[test]
fn generated_memory_backend_project_is_portable_to_wasm() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let project = create_generated_project(directory.path());
    let sources = generated_sources(&project);

    assert_published_artifacts(&project, &sources);
    assert_optimization_and_portability(&project, &sources);
    compile_and_run_native(&project, &sources);

    let Some((wasi_root, wasm_tools)) = discover_toolchain() else {
        return;
    };
    assert_pinned_versions(&wasi_root, &wasm_tools);
    compile_and_validate_wasm(
        &project,
        &sources,
        &wasi_root,
        &wasm_tools,
        directory.path(),
    );
}
