use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::backend_runtime::{BackendArtifact, iocp_artifacts, native_net_artifacts_for_target};
use crc_lib::config::TargetConfig;

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
        .join("tests/fixtures/backend/native")
        .join(name)
}

fn write_artifact(root: &Path, artifact: &BackendArtifact) {
    let path = root.join(artifact.path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("artifact directory");
    }
    fs::write(path, artifact.contents).expect("backend artifact");
}

#[test]
fn iocp_artifact_is_windows_only_and_keeps_native_details_private() {
    let artifacts = iocp_artifacts();
    assert_eq!(
        artifacts.last().map(|artifact| artifact.path),
        Some("runtime/cr_backend_iocp.c")
    );
    let source = artifacts.last().expect("IOCP source").contents;
    for required in [
        "CreateIoCompletionPort",
        "GetQueuedCompletionStatus",
        "CancelIoEx",
        "PostQueuedCompletionStatus",
        "OVERLAPPED overlapped;",
        "CR_BACKEND_IOCP_SUBMIT_OBSERVED",
    ] {
        assert!(source.contains(required), "{required}");
    }
    assert!(!source.contains("closesocket"));
    assert!(native_net_artifacts_for_target(&TargetConfig::WindowsMsvc).is_some());
    assert!(native_net_artifacts_for_target(&TargetConfig::WindowsGnu).is_some());
    for target in [
        TargetConfig::LinuxGnu,
        TargetConfig::LinuxMusl,
        TargetConfig::Macos,
        TargetConfig::Wasm32Wasi,
        TargetConfig::Custom("unknown-vendor".to_owned()),
    ] {
        let artifacts = native_net_artifacts_for_target(&target);
        assert!(artifacts.as_ref().is_none_or(|artifacts| {
            artifacts
                .iter()
                .all(|artifact| artifact.path != "runtime/cr_backend_iocp.c")
        }));
    }
}

#[test]
fn iocp_provider_runs_real_loopback_conformance_with_windows_compilers() {
    if !cfg!(windows) {
        eprintln!("skipping IOCP execution gate on a non-Windows host");
        return;
    }
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Windows Clang or GCC is required");
    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        for artifact in iocp_artifacts() {
            write_artifact(directory.path(), &artifact);
        }
        for name in ["iocp.c", "windows_helpers.h", "windows_hooks.h"] {
            fs::copy(fixture(name), directory.path().join(name)).expect("native fixture");
        }
        let executable = format!("backend-iocp-{compiler}.exe");
        let source_paths = iocp_artifacts()
            .into_iter()
            .filter(|artifact| artifact.is_source)
            .map(|artifact| artifact.path)
            .collect::<Vec<_>>();
        let mut command = Command::new(compiler);
        command.args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-include",
            "windows_hooks.h",
            "iocp.c",
        ]);
        command.args(source_paths);
        command
            .args(["-I", "include", "-I", "runtime", "-I", ".", "-o"])
            .arg(&executable)
            .arg("-lws2_32")
            .current_dir(directory.path());
        let compiled = run(&mut command);
        assert!(
            compiled.status.success(),
            "{compiler} IOCP compile: {}",
            output_text(&compiled)
        );
        let executed = run(&mut Command::new(directory.path().join(executable)));
        assert!(
            executed.status.success(),
            "{compiler} IOCP execute: {}",
            output_text(&executed)
        );
    }
}
