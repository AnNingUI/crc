use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use crc_lib::config::TargetConfig;
use crc_lib::executor_runtime::native_threaded_artifacts;
use crc_lib::runtime_abi::runtime_header;
use crc_lib::waker_abi::waker_header;

const FIXTURE_SOURCE: &str = include_str!("fixtures/waker/executor_threaded.c");
const HOOK_HEADER: &str = include_str!("fixtures/waker/executor_threaded_hooks.h");

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

fn write_runtime(directory: &Path) -> Vec<String> {
    fs::create_dir_all(directory.join("include")).expect("include directory");
    fs::create_dir_all(directory.join("runtime")).expect("runtime directory");
    fs::write(directory.join("include/cr_runtime.h"), runtime_header()).expect("runtime header");
    fs::write(directory.join("include/cr_waker.h"), waker_header()).expect("Waker header");
    let artifacts = native_threaded_artifacts(&TargetConfig::Host);
    let mut sources = Vec::new();
    for artifact in artifacts {
        let path = directory.join(artifact.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("artifact parent");
        }
        fs::write(&path, artifact.contents).expect("threaded artifact");
        if artifact.is_source {
            sources.push(artifact.path.to_owned());
        }
    }
    fs::write(directory.join("threaded.c"), FIXTURE_SOURCE).expect("threaded fixture");
    fs::write(directory.join("threaded_hooks.h"), HOOK_HEADER).expect("threaded hooks");
    sources
}

#[test]
fn native_threaded_executor_proves_wake_visibility_and_owner_rules() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");

    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        let sources = write_runtime(directory.path());
        let executable = if cfg!(windows) {
            "reference-threaded.exe"
        } else {
            "reference-threaded"
        };
        let mut command = Command::new(compiler);
        command.args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-fno-inline",
            "-include",
            "threaded_hooks.h",
            "-DCR_EXECUTOR_WAIT_HOOK=test_executor_wait_hook",
            "threaded.c",
        ]);
        command.args(&sources);
        if !cfg!(windows) {
            command.arg("-pthread");
        }
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
fn threaded_artifact_selection_is_platform_specific_and_wasi_free() {
    let windows = native_threaded_artifacts(&TargetConfig::WindowsMsvc);
    assert!(
        windows
            .iter()
            .any(|artifact| artifact.path.ends_with("threaded_windows.c"))
    );
    assert!(
        !windows
            .iter()
            .any(|artifact| artifact.path.ends_with("threaded_posix.c"))
    );

    let linux = native_threaded_artifacts(&TargetConfig::LinuxGnu);
    assert!(
        linux
            .iter()
            .any(|artifact| artifact.path.ends_with("threaded_posix.c"))
    );
    assert!(
        !linux
            .iter()
            .any(|artifact| artifact.path.ends_with("threaded_windows.c"))
    );

    let wasi = native_threaded_artifacts(&TargetConfig::Wasm32Wasi);
    assert!(
        wasi.iter()
            .any(|artifact| artifact.path.ends_with("threaded_stub.c"))
    );
    assert!(!wasi.iter().any(|artifact| {
        artifact.path.ends_with("threaded_windows.c") || artifact.path.ends_with("threaded_posix.c")
    }));
}
