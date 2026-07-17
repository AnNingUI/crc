use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::backend_abi::{
    CR_BACKEND_ABI_VERSION, CR_BACKEND_CORE_ID_HIGH, CR_BACKEND_CORE_ID_LOW,
    CR_BACKEND_EXPERIMENTAL_ABI_VERSION, CR_NET_ABI_VERSION, CR_NET_EXPERIMENTAL_ABI_VERSION,
    CR_NET_RECEIVE_EXTENSION_ID_HIGH, CR_NET_RECEIVE_EXTENSION_ID_LOW, backend_header, net_header,
};

const WASI_SDK_VERSION: &str = include_str!("../tools/wasi-sdk.version");
const FIXTURE_NAMES: [&str; 2] = ["layout.c", "protocol.c"];
const STABLE_PREFIX_DIGESTS: &str = include_str!("fixtures/backend/stable/prefixes.fnv64");

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

fn stable_region<'a>(header: &'a str, begin: &str, end: &str) -> &'a str {
    let (_, after_begin) = header.split_once(begin).expect("stable begin marker");
    let (region, _) = after_begin.split_once(end).expect("stable end marker");
    region
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn expected_digest(name: &str) -> u64 {
    STABLE_PREFIX_DIGESTS
        .lines()
        .find_map(|line| line.split_once('=').filter(|(key, _)| *key == name))
        .map(|(_, value)| u64::from_str_radix(value, 16).expect("hex digest"))
        .expect("stable prefix digest")
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
fn stable_identity_constants_match_the_public_headers() {
    assert_eq!(CR_BACKEND_ABI_VERSION, 1);
    assert_eq!(CR_NET_ABI_VERSION, 1);
    assert_eq!(CR_BACKEND_EXPERIMENTAL_ABI_VERSION, CR_BACKEND_ABI_VERSION);
    assert_eq!(CR_NET_EXPERIMENTAL_ABI_VERSION, CR_NET_ABI_VERSION);
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
fn stable_v1_prefix_bytes_match_the_frozen_digest_fixture() {
    let backend = stable_region(
        backend_header(),
        "/* CR_STABLE_BACKEND_V1_BEGIN */",
        "/* CR_STABLE_BACKEND_V1_END */",
    );
    let net = stable_region(
        net_header(),
        "/* CR_STABLE_NET_V1_BEGIN */",
        "/* CR_STABLE_NET_V1_END */",
    );
    let backend_digest = fnv1a64(backend.as_bytes());
    let net_digest = fnv1a64(net.as_bytes());
    assert_eq!(
        backend_digest,
        expected_digest("cr_backend_v1"),
        "update only after an approved Backend v1 ABI revision; actual={backend_digest:016x}"
    );
    assert_eq!(
        net_digest,
        expected_digest("cr_net_v1"),
        "update only after an approved net v1 ABI revision; actual={net_digest:016x}"
    );
    assert!(!net.contains("cr_net_receive_awaitable_state"));
    assert!(!net.contains("cr_awaitable"));
}

#[test]
fn stable_records_compile_and_run_with_native_c11_compilers() {
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
fn stable_records_compile_for_pinned_wasm32_wasi() {
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
