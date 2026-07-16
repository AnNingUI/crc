use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::Compiler;
use crc_lib::config::{Config, TargetConfig};
use crc_lib::runtime_abi::runtime_header;
use crc_lib::waker_abi::waker_header;

const SOURCE: &str = include_str!("fixtures/waker/registration.cr");
const WASI_SDK_VERSION: &str = include_str!("../tools/wasi-sdk.version");

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

fn compile_source(target: TargetConfig) -> String {
    let mut config = Config::default();
    config.build.target = target;
    Compiler::new(config)
        .compile_source(SOURCE, Path::new("waker-registration.cr"))
        .expect("Waker registration fixture compiles")
}

fn write_fixture(directory: &Path, generated: &str) {
    fs::write(directory.join("cr_runtime.h"), runtime_header()).expect("runtime header");
    fs::write(directory.join("cr_waker.h"), waker_header()).expect("Waker header");
    fs::write(directory.join("registration.c"), generated).expect("generated source");
}

fn required_wasm() -> bool {
    env::var("CRC_REQUIRE_WASM").is_ok_and(|value| value == "1")
}

fn wasi_root() -> Option<PathBuf> {
    match env::var_os("WASI_SDK_PATH").map(PathBuf::from) {
        Some(root) => Some(root),
        None if required_wasm() => panic!("CRC_REQUIRE_WASM=1 requires WASI_SDK_PATH"),
        None => {
            eprintln!("skipping Waker registration WASI gate: WASI_SDK_PATH is not set");
            None
        }
    }
}

#[test]
fn registration_protocol_is_deterministic_on_native_compilers() {
    let generated = compile_source(TargetConfig::Host);
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");

    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_fixture(directory.path(), &generated);
        let executable = if cfg!(windows) {
            "waker-registration.exe"
        } else {
            "waker-registration"
        };
        let compilation = run(Command::new(compiler)
            .args([
                "-std=c11",
                "-Wall",
                "-Wextra",
                "-Werror",
                "-fno-inline",
                "registration.c",
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
fn registration_protocol_compiles_for_wasm32_wasi() {
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

    let generated = compile_source(TargetConfig::Wasm32Wasi);
    let directory = tempfile::tempdir().expect("temporary directory");
    write_fixture(directory.path(), &generated);
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
            "registration.c",
            "-o",
            "registration.o",
        ])
        .current_dir(directory.path()));
    assert!(
        compilation.status.success(),
        "{}",
        output_text(&compilation)
    );
    assert!(directory.path().join("registration.o").is_file());
}
