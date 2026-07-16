use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::config::TargetConfig;
use crc_lib::target_layout::{LayoutKnowledge, TargetLayoutModel};

const WASI_SDK_VERSION: &str = include_str!("../tools/wasi-sdk.version");
const WASM_TOOLS_VERSION: &str = include_str!("../tools/wasm-tools.version");

const EXECUTOR_HEADER_SOURCE: &str = r#"
#ifndef CR_WASM_CONTRACT_MAIN_H
#define CR_WASM_CONTRACT_MAIN_H

__async int executor_root(void);
__async int size_layout(void);
cr_awaitable executor_event(int value);
int executor_event_polls(void);
int executor_event_drops(void);

#endif
"#;

const EXECUTOR_CR_SOURCE: &str = r#"
#include "main.hr"

static __async int executor_child(int value) {
    return __await executor_event(value);
}

__async int executor_root(void) {
    int first = __await executor_event(20);
    int second = __await executor_child(22);
    return first + second;
}

static void consume_layout_value(int value) {
    (void)value;
}

__async int size_layout(void) {
    char first[64] = {1};
    __yield 1;
    consume_layout_value(first[0]);
    long long second[8] = {2};
    __yield 2;
    return (int)second[0];
}
"#;

const EXECUTOR_APP_SOURCE: &str = r#"
#include "main.h"
#include "cr_executor.h"

#include <assert.h>
#include <stdlib.h>

typedef struct executor_event_state {
    int value;
    int polls;
    cr_waker retained_waker;
} executor_event_state;

static int event_poll_count;
static int event_drop_count;

static cr_poll_status executor_event_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    executor_event_state *state = (executor_event_state *)raw;
    assert(poll_context != NULL);
    assert(poll_context->abi_version == CR_POLL_CONTEXT_ABI_VERSION);
    assert(poll_context->struct_size >= CR_POLL_CONTEXT_V1_MIN_SIZE);
    assert(poll_context->available_capabilities == CR_POLL_CAP_WAKER);
    assert(cr_waker_is_valid(poll_context->waker));
    state->polls++;
    event_poll_count++;
    if (state->polls == 1) {
        assert(cr_waker_clone(poll_context->waker, &state->retained_waker));
        cr_waker_wake(&state->retained_waker);
        cr_waker_wake(&state->retained_waker);
        return CR_POLL_PENDING;
    }
    *(int *)out_value = state->value;
    return CR_POLL_READY;
}

static void executor_event_drop(void *raw) {
    executor_event_state *state = (executor_event_state *)raw;
    event_drop_count++;
    cr_waker_drop(&state->retained_waker);
    free(state);
}

static const cr_awaitable_vtable executor_event_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    CR_POLL_CAP_WAKER,
    executor_event_poll,
    NULL,
    executor_event_drop,
    sizeof(int),
    _Alignof(int)
};

cr_awaitable executor_event(int value) {
    executor_event_state *state = calloc(1u, sizeof(*state));
    assert(state != NULL);
    state->value = value;
    return (cr_awaitable){state, &executor_event_vtable};
}

int executor_event_polls(void) {
    return event_poll_count;
}

int executor_event_drops(void) {
    return event_drop_count;
}

typedef struct executor_observation {
    int calls;
    cr_poll_status status;
    int value;
} executor_observation;

static void observe_executor_root(
    void *raw,
    cr_poll_status status,
    const void *value,
    const cr_error *error
) {
    executor_observation *observation = (executor_observation *)raw;
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
    cr_executor_task *ticket = NULL;
    cr_executor_root_task *task;
    cr_awaitable root;
    executor_observation observation = {0, CR_POLL_PENDING, 0};

    assert(executor != NULL);
    task = cr_executor_root_create(&error);
    assert(task != NULL);
    root = cr_executor_root_into_awaitable(task);
    assert(cr_executor_spawn(
        executor,
        &root,
        observe_executor_root,
        &observation,
        &error,
        &ticket
    ));
    assert(root.state == NULL && root.vtable == NULL);
    assert(cr_executor_run_ready(executor) == 3u);
    assert(observation.calls == 1);
    assert(observation.status == CR_POLL_READY);
    assert(observation.value == 42);
    assert(executor_event_polls() == 4);
    assert(executor_event_drops() == 2);
    cr_executor_task_release(ticket);
    cr_executor_destroy(executor);
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

fn required() -> bool {
    env::var("CRC_REQUIRE_WASM").is_ok_and(|value| value == "1")
}

fn discover_toolchain() -> Option<(PathBuf, PathBuf)> {
    let Some(wasi_root) = env::var_os("WASI_SDK_PATH").map(PathBuf::from) else {
        if required() {
            panic!("CRC_REQUIRE_WASM=1 requires WASI_SDK_PATH");
        }
        eprintln!("skipping Wasm gate: WASI_SDK_PATH is not set");
        return None;
    };
    let clang = wasi_root
        .join("bin")
        .join(if cfg!(windows) { "clang.exe" } else { "clang" });
    if !clang.is_file() {
        if required() {
            panic!("WASI SDK Clang is missing: {}", clang.display());
        }
        eprintln!("skipping Wasm gate: {} is missing", clang.display());
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
        eprintln!("skipping Wasm gate: wasm-tools is not installed");
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
        fs::read_to_string(wasi_root.join("VERSION")).expect("WASI SDK VERSION file is readable");
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
    let project = root.join("wasm-contract");
    let created = run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .args(["--root", root.to_str().expect("UTF-8 root"), "create"])
        .arg("wasm-contract")
        .args(["--dir", "wasm-contract"]));
    assert!(created.status.success(), "{}", output_text(&created));
    let config_path = project.join("crc.toml");
    let config = fs::read_to_string(&config_path).expect("generated crc.toml");
    assert!(
        config.contains("computed_goto = false"),
        "Wasm fixture must use the portable backend"
    );
    let config = config.replacen("target = \"host\"", "target = \"wasm32-wasi\"", 1);
    let config = config.replacen(
        "optimization = \"speed\"",
        "optimization = \"aggressive\"",
        1,
    );
    let config = config.replacen("executor = \"manual\"", "executor = \"single-thread\"", 1);
    fs::write(&config_path, &config).expect("write wasm32-wasi project config");
    assert!(
        config.contains("target = \"wasm32-wasi\""),
        "Wasm fixture must plan for wasm32-wasi before C compilation"
    );
    assert!(
        config.contains("optimization = \"aggressive\""),
        "Wasm fixture must exercise Aggressive context planning"
    );
    assert!(
        config.contains("executor = \"single-thread\""),
        "Wasm fixture must package the portable executor"
    );
    fs::write(project.join("crc/include/main.hr"), EXECUTOR_HEADER_SOURCE)
        .expect("write executor header fixture");
    fs::write(project.join("crc/src/main.cr"), EXECUTOR_CR_SOURCE)
        .expect("write executor CR fixture");
    fs::write(project.join("src/main.c"), EXECUTOR_APP_SOURCE)
        .expect("write executor application fixture");
    let built = run(Command::new(env!("CARGO_BIN_EXE_crc")).args([
        "--root",
        project.to_str().expect("UTF-8 project"),
        "build",
    ]));
    assert!(built.status.success(), "{}", output_text(&built));
    project
}

fn generated_sources(project: &Path) -> Vec<PathBuf> {
    let mut sources: Vec<_> = walkdir::WalkDir::new(project.join("crc/dist"))
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(walkdir::DirEntry::into_path)
        .filter(|path| path.extension() == Some(OsStr::new("c")))
        .collect();
    sources.sort();
    assert!(!sources.is_empty(), "generated project has no C sources");
    sources
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

fn compile_and_run_native(project: &Path, generated: &[PathBuf]) {
    let compilers = available_native_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        let executable = project.join(if cfg!(windows) {
            format!("wasm-contract-{compiler}.exe")
        } else {
            format!("wasm-contract-{compiler}")
        });
        let mut command = Command::new(compiler);
        command.args(["-std=c11", "-Wall", "-Wextra", "-Werror"]);
        command.args(generated);
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
            "native {compiler}: {}",
            output_text(&compiled)
        );
        let executed = run(&mut Command::new(&executable));
        assert!(
            executed.status.success(),
            "native {compiler}: {}",
            output_text(&executed)
        );
    }
}

fn assert_wasi_layout_model(wasi_root: &Path, project: &Path, directory: &Path) {
    let model = match TargetLayoutModel::for_target(&TargetConfig::Wasm32Wasi) {
        LayoutKnowledge::Exact(model) => model,
        LayoutKnowledge::Unknown(reason) => panic!("WASI model is unknown: {reason:?}"),
    };
    let known = std::collections::BTreeMap::new();
    let fields: Vec<_> = [
        "uint8_t",
        "long",
        "void *",
        "uint16_t[3][4]",
        "int (*)(void)",
        "cr_error",
        "cr_cleanup_stack",
        "cr_awaitable",
    ]
    .into_iter()
    .map(|c_type| {
        model
            .type_layout(c_type, &known)
            .exact()
            .copied()
            .unwrap_or_else(|| panic!("exact WASI layout for {c_type}"))
    })
    .collect();
    let layout = model
        .struct_layout(fields)
        .exact()
        .cloned()
        .expect("exact WASI aggregate layout");
    let probe = directory.join("wasi-layout-probe.c");
    fs::write(
        &probe,
        format!(
            r#"#include "cr_runtime.h"
#include "cr_waker.h"
#include <stddef.h>
typedef int (*probe_callback)(void);
typedef struct layout_probe {{
    uint8_t byte;
    long count;
    void *pointer;
    uint16_t samples[3][4];
    probe_callback callback;
    cr_error error;
    cr_cleanup_stack cleanups;
    cr_awaitable awaitable;
}} layout_probe;
_Static_assert(sizeof(cr_waker) == 2u * sizeof(void *), "WASI waker size");
_Static_assert(
    CR_WAKER_VTABLE_V1_MIN_SIZE <= sizeof(cr_waker_vtable),
    "WASI waker v1 prefix fits the structure"
);
_Static_assert(sizeof(layout_probe) == {}u, "WASI size model");
_Static_assert(_Alignof(layout_probe) == {}u, "WASI alignment model");
_Static_assert(offsetof(layout_probe, byte) == {}u, "WASI byte offset");
_Static_assert(offsetof(layout_probe, count) == {}u, "WASI long offset");
_Static_assert(offsetof(layout_probe, pointer) == {}u, "WASI pointer offset");
_Static_assert(offsetof(layout_probe, samples) == {}u, "WASI array offset");
_Static_assert(offsetof(layout_probe, callback) == {}u, "WASI callback offset");
_Static_assert(offsetof(layout_probe, error) == {}u, "WASI error offset");
_Static_assert(offsetof(layout_probe, cleanups) == {}u, "WASI cleanup offset");
_Static_assert(offsetof(layout_probe, awaitable) == {}u, "WASI awaitable offset");
"#,
            layout.size,
            layout.align,
            layout.offsets[0],
            layout.offsets[1],
            layout.offsets[2],
            layout.offsets[3],
            layout.offsets[4],
            layout.offsets[5],
            layout.offsets[6],
            layout.offsets[7],
        ),
    )
    .expect("WASI layout probe source");
    let object = directory.join("wasi-layout-probe.o");
    let compiled = run(Command::new(wasi_root.join("bin").join(if cfg!(windows) {
        "clang.exe"
    } else {
        "clang"
    }))
    .arg("--target=wasm32-wasi")
    .arg(format!(
        "--sysroot={}",
        wasi_root.join("share/wasi-sysroot").display()
    ))
    .arg("-std=c11")
    .arg("-Wall")
    .arg("-Wextra")
    .arg("-Werror")
    .arg("-I")
    .arg(project.join("crc/dist/include"))
    .arg("-c")
    .arg(&probe)
    .arg("-o")
    .arg(&object));
    assert!(compiled.status.success(), "{}", output_text(&compiled));
    assert!(object.is_file(), "WASI layout probe object is missing");
}

#[test]
fn generated_project_compiles_and_validates_as_wasm() {
    let Some((wasi_root, wasm_tools)) = discover_toolchain() else {
        return;
    };
    assert_pinned_versions(&wasi_root, &wasm_tools);

    let directory = tempfile::tempdir().expect("temporary directory");
    let project = create_generated_project(directory.path());
    assert!(project.join("crc/dist/include/cr_waker.h").is_file());
    assert!(project.join("crc/dist/include/cr_executor.h").is_file());
    assert!(
        project
            .join("crc/dist/runtime/cr_executor_common.c")
            .is_file()
    );
    assert!(
        project
            .join("crc/dist/runtime/cr_executor_single.c")
            .is_file()
    );
    assert!(
        project
            .join("crc/dist/runtime/cr_executor_threaded_stub.c")
            .is_file()
    );
    assert!(
        !project
            .join("crc/dist/runtime/cr_executor_threaded_windows.c")
            .exists()
    );
    assert!(
        !project
            .join("crc/dist/runtime/cr_executor_threaded_posix.c")
            .exists()
    );
    assert_wasi_layout_model(&wasi_root, &project, directory.path());
    let module = project.join("crc-demo.wasm");
    let generated = generated_sources(&project);
    let generated_outputs = generated
        .iter()
        .map(|path| {
            path.strip_prefix(project.join("crc/dist"))
                .expect("generated source stays in dist")
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        generated_outputs,
        vec![
            "main.c",
            "runtime/cr_executor_common.c",
            "runtime/cr_executor_single.c",
            "runtime/cr_executor_threaded_stub.c",
        ]
    );
    let generated_text = generated
        .iter()
        .map(|path| fs::read_to_string(path).expect("generated C source"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(generated_text.contains("struct cr_size_layout_task"));
    assert!(generated_text.contains("union {"));
    assert!(generated_text.contains("long long"));
    assert!(generated_text.contains("cr_crc_src_main_cr_executor_child_poll(&ctx->"));
    assert!(generated_text.contains(", poll_context)"));
    assert!(!generated_text.contains("cr_crc_src_main_cr_executor_child_into_awaitable("));

    let runtime_text = generated
        .iter()
        .filter(|path| path.parent() == Some(&project.join("crc/dist/runtime")))
        .map(|path| fs::read_to_string(path).expect("portable runtime source"))
        .collect::<String>()
        .to_ascii_lowercase();
    for forbidden in [
        "stdatomic",
        "pthread_",
        "windows.h",
        "interlocked",
        "critical_section",
        "condition_variable",
        "createthread",
    ] {
        assert!(!runtime_text.contains(forbidden), "{forbidden}");
    }

    let meson_manifest =
        fs::read_to_string(project.join("crc/dist/meson.build")).expect("Meson manifest");
    assert_eq!(
        meson_manifest,
        "cr_generated_sources = files(\n  'main.c',\n  'runtime/cr_executor_common.c',\n  'runtime/cr_executor_single.c',\n  'runtime/cr_executor_threaded_stub.c',\n)\ncr_generated_dependencies = []\n"
    );
    let artifact_manifest =
        fs::read_to_string(project.join("crc/dist/crc-artifacts.json")).expect("artifact manifest");
    assert!(artifact_manifest.contains("\"output\": \"include/cr_executor.h\""));
    assert!(artifact_manifest.contains("\"output\": \"runtime/cr_executor_internal.h\""));
    assert_eq!(
        artifact_manifest
            .matches("\"kind\": \"executor-source\"")
            .count(),
        3
    );
    assert!(!artifact_manifest.contains("threaded_windows"));
    assert!(!artifact_manifest.contains("threaded_posix"));

    compile_and_run_native(&project, &generated);

    let mut clang =
        Command::new(
            wasi_root
                .join("bin")
                .join(if cfg!(windows) { "clang.exe" } else { "clang" }),
        );
    clang
        .arg("--target=wasm32-wasi")
        .arg(format!(
            "--sysroot={}",
            wasi_root.join("share/wasi-sysroot").display()
        ))
        .arg("-std=c11")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("-I")
        .arg(project.join("crc/dist/include"))
        .arg("-I")
        .arg(project.join("crc/dist/runtime"));
    for source in &generated {
        clang.arg(source);
    }
    clang.arg(project.join("src/main.c")).arg("-o").arg(&module);
    let compiled = run(&mut clang);
    assert!(compiled.status.success(), "{}", output_text(&compiled));

    let validated = run(Command::new(wasm_tools).arg("validate").arg(&module));
    assert!(validated.status.success(), "{}", output_text(&validated));
    assert!(module.is_file(), "validated Wasm module is missing");
}
