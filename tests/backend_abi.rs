use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::backend_abi::{
    CR_BACKEND_CORE_ID_HIGH, CR_BACKEND_CORE_ID_LOW, CR_BACKEND_EXPERIMENTAL_ABI_VERSION,
    CR_NET_EXPERIMENTAL_ABI_VERSION, CR_NET_RECEIVE_EXTENSION_ID_HIGH,
    CR_NET_RECEIVE_EXTENSION_ID_LOW, backend_header, net_header,
};

const WASI_SDK_VERSION: &str = include_str!("../tools/wasi-sdk.version");
const FIXTURE_NAMES: [&str; 2] = ["layout.c", "protocol.c"];

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

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/backend/abi")
        .join(name)
}

fn write_headers(directory: &Path) {
    fs::write(directory.join("cr_backend.h"), backend_header()).expect("backend header");
    fs::write(directory.join("cr_net.h"), net_header()).expect("net header");
}

fn required_wasm() -> bool {
    env::var("CRC_REQUIRE_WASM").is_ok_and(|value| value == "1")
}

fn wasi_root() -> Option<PathBuf> {
    match env::var_os("WASI_SDK_PATH").map(PathBuf::from) {
        Some(root) => Some(root),
        None if required_wasm() => panic!("CRC_REQUIRE_WASM=1 requires WASI_SDK_PATH"),
        None => {
            eprintln!("skipping backend ABI WASI gate: WASI_SDK_PATH is not set");
            None
        }
    }
}

#[test]
fn experimental_identity_constants_match_the_public_headers() {
    assert_eq!(CR_BACKEND_EXPERIMENTAL_ABI_VERSION, 1);
    assert_eq!(CR_NET_EXPERIMENTAL_ABI_VERSION, 1);
    assert_eq!(CR_BACKEND_CORE_ID_HIGH, 0x4352_5f42_4143_4b45);
    assert_eq!(CR_BACKEND_CORE_ID_LOW, 0x4e44_5f43_4f52_4531);
    assert_eq!(CR_NET_RECEIVE_EXTENSION_ID_HIGH, 0x4352_5f4e_4554_5f52);
    assert_eq!(CR_NET_RECEIVE_EXTENSION_ID_LOW, 0x4543_4549_5645_5f31);
    assert_ne!(
        (CR_BACKEND_CORE_ID_HIGH, CR_BACKEND_CORE_ID_LOW),
        (
            CR_NET_RECEIVE_EXTENSION_ID_HIGH,
            CR_NET_RECEIVE_EXTENSION_ID_LOW
        )
    );
}

#[test]
fn headers_keep_runtime_objects_and_native_apis_out_of_the_public_boundary() {
    let combined = format!("{}{}", backend_header(), net_header()).to_ascii_lowercase();
    for forbidden in [
        "cr_task",
        "cr_executor",
        "reactor",
        "eventsource",
        "windows.h",
        "winsock2.h",
        "sys/epoll.h",
        "sys/event.h",
        "pthread",
    ] {
        assert!(!combined.contains(forbidden), "unexpected `{forbidden}`");
    }
    assert!(combined.contains("typedef struct cr_backend cr_backend"));
    assert!(combined.contains("typedef struct cr_net_receive_operation"));
    assert!(!combined.contains("struct cr_backend {"));
    assert!(!combined.contains("struct cr_net_receive_operation {"));
}

#[test]
fn experimental_records_compile_and_run_with_native_c11_compilers() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        for fixture_name in FIXTURE_NAMES {
            let directory = tempfile::tempdir().expect("temporary directory");
            write_headers(directory.path());
            let executable = if cfg!(windows) {
                format!("{fixture_name}.exe")
            } else {
                fixture_name.trim_end_matches(".c").to_owned()
            };
            let compiled = run(Command::new(compiler)
                .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
                .arg(directory.path())
                .arg(fixture(fixture_name))
                .arg("-o")
                .arg(&executable)
                .current_dir(directory.path()));
            assert!(
                compiled.status.success(),
                "{compiler} {fixture_name}: {}",
                output_text(&compiled)
            );
            let execution = run(&mut Command::new(directory.path().join(&executable)));
            assert!(
                execution.status.success(),
                "{compiler} {fixture_name}: {}",
                output_text(&execution)
            );
        }
    }
}

#[test]
fn experimental_records_compile_for_pinned_wasm32_wasi() {
    let Some(wasi_root) = wasi_root() else {
        return;
    };
    let installed = fs::read_to_string(wasi_root.join("VERSION")).expect("WASI VERSION file");
    assert_eq!(installed.lines().next(), Some(WASI_SDK_VERSION.trim()));
    let clang = wasi_root
        .join("bin")
        .join(if cfg!(windows) { "clang.exe" } else { "clang" });
    assert!(clang.is_file(), "missing WASI Clang: {}", clang.display());

    for fixture_name in FIXTURE_NAMES {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_headers(directory.path());
        let compiled = run(Command::new(&clang)
            .arg("--target=wasm32-wasi")
            .arg(format!(
                "--sysroot={}",
                wasi_root.join("share/wasi-sysroot").display()
            ))
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-I"])
            .arg(directory.path())
            .arg("-c")
            .arg(fixture(fixture_name))
            .arg("-o")
            .arg(format!("{fixture_name}.o"))
            .current_dir(directory.path()));
        assert!(
            compiled.status.success(),
            "{fixture_name}: {}",
            output_text(&compiled)
        );
    }
}
