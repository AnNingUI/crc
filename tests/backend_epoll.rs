use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::backend_runtime::{BackendArtifact, epoll_artifacts, native_net_artifacts_for_target};
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

fn write_linux_fixture_tree(root: &Path) {
    for artifact in epoll_artifacts() {
        write_artifact(root, &artifact);
    }
    for name in ["epoll.c", "linux_helpers.h", "linux_hooks.h"] {
        fs::copy(fixture(name), root.join(name)).expect("native fixture");
    }
}

#[test]
fn epoll_artifacts_can_be_exported_for_an_external_linux_gate() {
    let Some(directory) = env::var_os("CRC_EPOLL_EXPORT_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    fs::create_dir_all(&directory).expect("external Linux gate directory");
    write_linux_fixture_tree(&directory);
}

#[test]
fn epoll_artifact_is_linux_only_and_keeps_readiness_private() {
    let artifacts = epoll_artifacts();
    assert_eq!(
        artifacts.last().map(|artifact| artifact.path),
        Some("runtime/cr_backend_epoll.c")
    );
    let source = artifacts.last().expect("epoll source").contents;
    for required in [
        "epoll_create1",
        "EPOLLONESHOT",
        "EPOLL_CTL_MOD",
        "eventfd",
        "CR_BACKEND_EPOLL_FILTER_EVENT_TOKEN",
        "operation->event_token",
    ] {
        assert!(source.contains(required), "{required}");
    }
    assert!(!source.contains("close(operation->socket_fd)"));
    assert!(native_net_artifacts_for_target(&TargetConfig::LinuxGnu).is_some());
    assert!(native_net_artifacts_for_target(&TargetConfig::LinuxMusl).is_some());
    for target in [
        TargetConfig::WindowsMsvc,
        TargetConfig::WindowsGnu,
        TargetConfig::Macos,
        TargetConfig::Wasm32Wasi,
        TargetConfig::Custom("unknown-vendor".to_owned()),
    ] {
        let artifacts = native_net_artifacts_for_target(&target);
        assert!(artifacts.as_ref().is_none_or(|artifacts| {
            artifacts
                .iter()
                .all(|artifact| artifact.path != "runtime/cr_backend_epoll.c")
        }));
    }
}

#[test]
fn epoll_provider_runs_real_loopback_conformance_with_linux_compilers() {
    if !cfg!(target_os = "linux") {
        eprintln!("skipping epoll execution gate on a non-Linux host");
        return;
    }
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Linux Clang or GCC is required");
    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_linux_fixture_tree(directory.path());
        let executable = format!("backend-epoll-{compiler}");
        let source_paths = epoll_artifacts()
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
            "linux_hooks.h",
            "epoll.c",
        ]);
        command.args(source_paths);
        command
            .args([
                "-I", "include", "-I", "runtime", "-I", ".", "-pthread", "-o",
            ])
            .arg(&executable)
            .current_dir(directory.path());
        let compiled = run(&mut command);
        assert!(
            compiled.status.success(),
            "{compiler} epoll compile: {}",
            output_text(&compiled)
        );
        let executed = run(&mut Command::new(directory.path().join(executable)));
        assert!(
            executed.status.success(),
            "{compiler} epoll execute: {}",
            output_text(&executed)
        );
    }
}
