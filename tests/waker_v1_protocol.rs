use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::runtime_abi::runtime_header;
use crc_lib::waker_abi::waker_header;

const WASI_SDK_VERSION: &str = include_str!("../tools/wasi-sdk.version");

const PROTOCOL_SOURCE: &str = r#"
#include "cr_waker.h"

#include <assert.h>

typedef struct protocol_state {
    int originals;
    int references;
    int clone_calls;
    int wake_calls;
    int drop_calls;
    int allocation_calls;
    bool clone_returns_null;
} protocol_state;

static void *protocol_clone_state(void *raw) {
    protocol_state *state = (protocol_state *)raw;
    state->clone_calls++;
    if (state->clone_returns_null) return NULL;
    state->references++;
    return state;
}

static void protocol_wake_by_ref(void *raw) {
    protocol_state *state = (protocol_state *)raw;
    state->wake_calls++;
}

static void protocol_drop_state(void *raw) {
    protocol_state *state = (protocol_state *)raw;
    assert(state->references > 0);
    state->drop_calls++;
    state->references--;
}

static cr_waker_vtable protocol_vtable(void) {
    cr_waker_vtable vtable = {
        CR_WAKER_VTABLE_ABI_VERSION,
        sizeof(cr_waker_vtable),
        0u,
        protocol_clone_state,
        protocol_wake_by_ref,
        protocol_drop_state
    };
    return vtable;
}

static void assert_null_waker(const cr_waker *waker) {
    assert(waker->state == NULL);
    assert(waker->vtable == NULL);
}

static void expect_invalid(
    cr_waker candidate,
    protocol_state *state
) {
    cr_waker output = {state, candidate.vtable};
    int clones_before = state->clone_calls;
    int wakes_before = state->wake_calls;
    int drops_before = state->drop_calls;

    assert(!cr_waker_is_valid(&candidate));
    assert(!cr_waker_clone(&candidate, &output));
    assert_null_waker(&output);
    assert(state->clone_calls == clones_before);

    cr_waker_wake(&candidate);
    assert(state->wake_calls == wakes_before);

    cr_waker_drop(&candidate);
    assert_null_waker(&candidate);
    assert(state->drop_calls == drops_before);
}

static void test_null_and_malformed_handles(void) {
    protocol_state state = {1, 1, 0, 0, 0, 0, false};
    cr_waker_vtable valid = protocol_vtable();
    cr_waker output = {&state, &valid};

    assert(!cr_waker_is_valid(NULL));
    assert(!cr_waker_clone(NULL, &output));
    assert_null_waker(&output);
    assert(!cr_waker_clone(&(cr_waker){&state, &valid}, NULL));
    assert(state.clone_calls == 0);
    cr_waker_wake(NULL);
    cr_waker_drop(NULL);

    expect_invalid((cr_waker){NULL, &valid}, &state);
    expect_invalid((cr_waker){&state, NULL}, &state);

    cr_waker_vtable malformed = valid;
    malformed.abi_version = 0u;
    expect_invalid((cr_waker){&state, &malformed}, &state);

    malformed = valid;
    malformed.struct_size = CR_WAKER_VTABLE_V1_MIN_SIZE - 1u;
    expect_invalid((cr_waker){&state, &malformed}, &state);

    malformed = valid;
    malformed.clone_state = NULL;
    expect_invalid((cr_waker){&state, &malformed}, &state);

    malformed = valid;
    malformed.wake_by_ref = NULL;
    expect_invalid((cr_waker){&state, &malformed}, &state);

    malformed = valid;
    malformed.drop_state = NULL;
    expect_invalid((cr_waker){&state, &malformed}, &state);

    assert(state.originals == 1);
    assert(state.references == 1);
    assert(state.clone_calls == 0);
    assert(state.wake_calls == 0);
    assert(state.drop_calls == 0);
    assert(state.allocation_calls == 0);
}

static void test_clone_wake_and_drop_accounting(void) {
    protocol_state state = {1, 1, 0, 0, 0, 0, false};
    cr_waker_vtable vtable = protocol_vtable();
    cr_waker original = {&state, &vtable};
    cr_waker first = {NULL, NULL};
    cr_waker second = {NULL, NULL};

    assert(cr_waker_is_valid(&original));
    assert(cr_waker_clone(&original, &first));
    assert(cr_waker_clone(&original, &second));
    assert(first.state == &state && first.vtable == &vtable);
    assert(second.state == &state && second.vtable == &vtable);
    assert(state.clone_calls == 2);
    assert(state.references == 3);
    assert(state.allocation_calls == 0);

    cr_waker_wake(&original);
    cr_waker_wake(&first);
    cr_waker_wake(&first);
    assert(cr_waker_is_valid(&original));
    assert(cr_waker_is_valid(&first));
    assert(state.wake_calls == 3);
    assert(state.drop_calls == 0);

    cr_waker_drop(&first);
    assert_null_waker(&first);
    assert(state.drop_calls == 1);
    assert(state.references == 2);
    cr_waker_drop(&first);
    assert(state.drop_calls == 1);

    cr_waker_drop(&second);
    cr_waker_drop(&original);
    assert_null_waker(&second);
    assert_null_waker(&original);
    assert(state.originals == 1);
    assert(state.clone_calls == 2);
    assert(state.wake_calls == 3);
    assert(state.drop_calls == 3);
    assert(state.references == 0);
    assert(state.allocation_calls == 0);
}

typedef struct future_waker_vtable {
    cr_waker_vtable v1;
    uint64_t appended_field;
} future_waker_vtable;

static void test_future_version_and_unknown_flags(void) {
    protocol_state state = {1, 1, 0, 0, 0, 0, false};
    future_waker_vtable future = {
        {
            CR_WAKER_VTABLE_ABI_VERSION + 1u,
            sizeof(future_waker_vtable),
            CR_WAKER_FLAG_CROSS_THREAD | (UINT64_C(1) << 63),
            protocol_clone_state,
            protocol_wake_by_ref,
            protocol_drop_state
        },
        UINT64_C(0x12345678)
    };
    cr_waker original = {&state, &future.v1};
    cr_waker clone = {NULL, NULL};

    assert(cr_waker_is_valid(&original));
    assert(
        (original.vtable->provided_flags & CR_WAKER_FLAG_CROSS_THREAD) != 0u
    );
    assert(cr_waker_clone(&original, &clone));
    cr_waker_wake(&clone);
    cr_waker_drop(&clone);
    cr_waker_drop(&original);

    assert(state.originals == 1);
    assert(state.clone_calls == 1);
    assert(state.wake_calls == 1);
    assert(state.drop_calls == 2);
    assert(state.references == 0);
    assert(state.allocation_calls == 0);
}

static void test_null_clone_result_is_protocol_failure(void) {
    protocol_state state = {1, 1, 0, 0, 0, 0, true};
    cr_waker_vtable vtable = protocol_vtable();
    cr_waker original = {&state, &vtable};
    cr_waker output = {&state, &vtable};

    assert(cr_waker_is_valid(&original));
    assert(!cr_waker_clone(&original, &output));
    assert_null_waker(&output);
    assert(state.clone_calls == 1);
    assert(state.references == 1);
    assert(state.wake_calls == 0);
    assert(state.allocation_calls == 0);

    cr_waker_drop(&original);
    assert_null_waker(&original);
    assert(state.drop_calls == 1);
    assert(state.references == 0);
}

int main(void) {
    test_null_and_malformed_handles();
    test_clone_wake_and_drop_accounting();
    test_future_version_and_unknown_flags();
    test_null_clone_result_is_protocol_failure();
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

fn write_fixture(directory: &Path) {
    fs::write(directory.join("cr_runtime.h"), runtime_header()).expect("runtime header");
    fs::write(directory.join("cr_waker.h"), waker_header()).expect("waker header");
    fs::write(directory.join("protocol.c"), PROTOCOL_SOURCE).expect("protocol source");
}

fn required_wasm() -> bool {
    env::var("CRC_REQUIRE_WASM").is_ok_and(|value| value == "1")
}

fn wasi_root() -> Option<PathBuf> {
    match env::var_os("WASI_SDK_PATH").map(PathBuf::from) {
        Some(root) => Some(root),
        None if required_wasm() => panic!("CRC_REQUIRE_WASM=1 requires WASI_SDK_PATH"),
        None => {
            eprintln!("skipping Waker WASI protocol gate: WASI_SDK_PATH is not set");
            None
        }
    }
}

#[test]
fn waker_v1_helpers_enforce_the_protocol_without_runtime_linkage() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");

    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_fixture(directory.path());
        let executable = if cfg!(windows) {
            "waker-protocol.exe"
        } else {
            "waker-protocol"
        };
        let compilation = run(Command::new(compiler)
            .args([
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-fno-inline",
                "protocol.c",
                "-o",
            ])
            .arg(executable)
            .current_dir(directory.path()));
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
fn waker_v1_helpers_compile_for_wasm32_wasi() {
    let Some(wasi_root) = wasi_root() else {
        return;
    };
    let installed = fs::read_to_string(wasi_root.join("VERSION")).expect("WASI VERSION file");
    assert_eq!(installed.lines().next(), Some(WASI_SDK_VERSION.trim()));
    let clang = wasi_root
        .join("bin")
        .join(if cfg!(windows) { "clang.exe" } else { "clang" });
    assert!(
        clang.is_file(),
        "WASI SDK Clang is missing: {}",
        clang.display()
    );

    let directory = tempfile::tempdir().expect("temporary directory");
    write_fixture(directory.path());
    let compilation = run(Command::new(clang)
        .arg("--target=wasm32-wasi")
        .arg(format!(
            "--sysroot={}",
            wasi_root.join("share/wasi-sysroot").display()
        ))
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fno-inline",
            "-c",
            "protocol.c",
            "-o",
            "protocol.o",
        ])
        .current_dir(directory.path()));
    assert!(
        compilation.status.success(),
        "{}",
        output_text(&compilation)
    );
    assert!(directory.path().join("protocol.o").is_file());
}
