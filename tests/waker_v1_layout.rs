use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::runtime_abi::runtime_header;
use crc_lib::waker_abi::{CR_WAKER_VTABLE_ABI_VERSION, waker_header};

const WASI_SDK_VERSION: &str = include_str!("../tools/wasi-sdk.version");
const STAGE4_RUNTIME_HEADER_LENGTH: usize = 4472;
const STAGE4_RUNTIME_HEADER_FNV1A64: u64 = 0x70dc_916f_2d8e_e4f0;

const LAYOUT_SOURCE: &str = r#"
#include "cr_waker.h"

static void *clone_state(void *state) { return state; }
static void wake_by_ref(void *state) { (void)state; }
static void drop_state(void *state) { (void)state; }

static const cr_waker_vtable test_vtable = {
    CR_WAKER_VTABLE_ABI_VERSION,
    sizeof(cr_waker_vtable),
    CR_WAKER_FLAG_CROSS_THREAD,
    clone_state,
    wake_by_ref,
    drop_state
};

typedef struct future_waker_vtable {
    cr_waker_vtable v1;
    uint64_t appended_field;
} future_waker_vtable;

_Static_assert(CR_RUNTIME_ABI_VERSION == 3u, "core runtime ABI");
_Static_assert(CR_WAKER_VTABLE_ABI_VERSION == 1u, "waker ABI version");
_Static_assert(
    sizeof(cr_waker) == 2u * sizeof(void *),
    "waker must contain two machine words"
);
_Static_assert(offsetof(cr_waker, state) == 0u, "waker state offset");
_Static_assert(
    offsetof(cr_waker, vtable) == sizeof(void *),
    "waker vtable offset"
);
_Static_assert(
    CR_WAKER_VTABLE_V1_MIN_SIZE ==
        offsetof(cr_waker_vtable, drop_state) +
        sizeof(((cr_waker_vtable *)0)->drop_state),
    "waker v1 prefix ends after drop_state"
);
_Static_assert(
    CR_WAKER_VTABLE_V1_MIN_SIZE <= sizeof(cr_waker_vtable),
    "waker v1 prefix fits the complete structure"
);
_Static_assert(
    offsetof(future_waker_vtable, v1) == 0u,
    "future version retains v1 at offset zero"
);
_Static_assert(
    offsetof(future_waker_vtable, appended_field) == sizeof(cr_waker_vtable),
    "future version appends after the v1 prefix"
);
_Static_assert(
    CR_POLL_CONTEXT_V1_MIN_SIZE ==
        offsetof(cr_poll_context, waker) +
        sizeof(((cr_poll_context *)0)->waker),
    "core poll context prefix remains unchanged"
);
_Static_assert(
    CR_POLL_CONTEXT_V1_MIN_SIZE <= sizeof(cr_poll_context),
    "core poll context prefix fits the complete structure"
);
_Static_assert(CR_ERROR_INVALID_WAKER_ABI == 1110, "invalid waker code");
_Static_assert(CR_ERROR_WAKER_CLONE_FAILED == 1111, "clone failure code");

int main(void) {
    int state = 0;
    cr_waker waker = {&state, &test_vtable};
    return waker.state == &state &&
        waker.vtable->abi_version >= CR_WAKER_VTABLE_ABI_VERSION ? 0 : 1;
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

fn write_headers(directory: &Path) {
    fs::write(directory.join("cr_runtime.h"), runtime_header()).expect("runtime header");
    fs::write(directory.join("cr_waker.h"), waker_header()).expect("waker header");
    fs::write(directory.join("layout.c"), LAYOUT_SOURCE).expect("layout source");
}

fn required_wasm() -> bool {
    env::var("CRC_REQUIRE_WASM").is_ok_and(|value| value == "1")
}

fn wasi_root() -> Option<PathBuf> {
    match env::var_os("WASI_SDK_PATH").map(PathBuf::from) {
        Some(root) => Some(root),
        None if required_wasm() => panic!("CRC_REQUIRE_WASM=1 requires WASI_SDK_PATH"),
        None => {
            eprintln!("skipping Waker WASI layout gate: WASI_SDK_PATH is not set");
            None
        }
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[test]
fn core_runtime_header_remains_byte_identical_to_stage4() {
    let header = runtime_header();
    assert_eq!(header.len(), STAGE4_RUNTIME_HEADER_LENGTH);
    assert_eq!(fnv1a64(header.as_bytes()), STAGE4_RUNTIME_HEADER_FNV1A64);
}

#[test]
fn waker_v1_layout_is_valid_for_native_compilers() {
    assert_eq!(CR_WAKER_VTABLE_ABI_VERSION, 1);
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_headers(directory.path());
        let executable = if cfg!(windows) {
            "waker-layout.exe"
        } else {
            "waker-layout"
        };
        let compiled = run(Command::new(compiler)
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "layout.c", "-o"])
            .arg(executable)
            .current_dir(directory.path()));
        assert!(
            compiled.status.success(),
            "{compiler}: {}",
            output_text(&compiled)
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
fn waker_v1_layout_compiles_for_wasm32_wasi() {
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
    write_headers(directory.path());
    let compiled = run(Command::new(clang)
        .arg("--target=wasm32-wasi")
        .arg(format!(
            "--sysroot={}",
            wasi_root.join("share/wasi-sysroot").display()
        ))
        .args([
            "-std=c11", "-Wall", "-Wextra", "-Werror", "-c", "layout.c", "-o",
        ])
        .arg("layout.o")
        .current_dir(directory.path()));
    assert!(compiled.status.success(), "{}", output_text(&compiled));
    assert!(directory.path().join("layout.o").is_file());
}
