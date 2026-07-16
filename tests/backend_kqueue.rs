use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::backend_runtime::{
    BackendArtifact, kqueue_artifacts, native_net_artifacts_for_target,
};
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

fn write_macos_fixture_tree(root: &Path) {
    for artifact in kqueue_artifacts() {
        write_artifact(root, &artifact);
    }
    for name in ["kqueue.c", "macos_helpers.h", "macos_hooks.h"] {
        fs::copy(fixture(name), root.join(name)).expect("native fixture");
    }
}

fn append_compile_args(command: &mut Command, root: &Path, output: &str) {
    let source_paths = kqueue_artifacts()
        .into_iter()
        .filter(|artifact| artifact.is_source)
        .map(|artifact| artifact.path)
        .collect::<Vec<_>>();
    command.args([
        "-std=c11",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-include",
        "macos_hooks.h",
        "kqueue.c",
    ]);
    command.args(source_paths);
    command
        .args([
            "-I", "include", "-I", "runtime", "-I", ".", "-pthread", "-o",
        ])
        .arg(output)
        .current_dir(root);
}

fn compile_command(compiler: &str, root: &Path, output: &str) -> Command {
    let mut command = Command::new(compiler);
    append_compile_args(&mut command, root, output);
    command
}

#[test]
fn kqueue_artifact_is_macos_only_and_keeps_event_details_private() {
    let artifacts = kqueue_artifacts();
    assert_eq!(
        artifacts.last().map(|artifact| artifact.path),
        Some("runtime/cr_backend_kqueue.c")
    );
    let source = artifacts.last().expect("kqueue source").contents;
    for required in [
        "kqueue()",
        "EVFILT_READ",
        "EV_DISPATCH",
        "EVFILT_USER",
        "NOTE_TRIGGER",
        "EV_EOF",
        "operation->event_token",
    ] {
        assert!(source.contains(required), "{required}");
    }
    assert!(!source.contains("close(operation->socket_fd)"));
    let macos =
        native_net_artifacts_for_target(&TargetConfig::Macos).expect("macOS kqueue artifacts");
    assert!(
        macos
            .iter()
            .any(|artifact| artifact.path == "runtime/cr_backend_kqueue.c")
    );
    for target in [
        TargetConfig::WindowsMsvc,
        TargetConfig::WindowsGnu,
        TargetConfig::LinuxGnu,
        TargetConfig::LinuxMusl,
        TargetConfig::Wasm32Wasi,
        TargetConfig::Custom("unknown-vendor".to_owned()),
    ] {
        let artifacts = native_net_artifacts_for_target(&target);
        assert!(artifacts.as_ref().is_none_or(|artifacts| {
            artifacts
                .iter()
                .all(|artifact| artifact.path != "runtime/cr_backend_kqueue.c")
        }));
    }
}

#[test]
fn kqueue_artifacts_can_be_exported_for_an_external_macos_gate() {
    let Some(directory) = env::var_os("CRC_KQUEUE_EXPORT_DIR") else {
        return;
    };
    let directory = PathBuf::from(directory);
    fs::create_dir_all(&directory).expect("external macOS gate directory");
    write_macos_fixture_tree(&directory);
}

#[test]
fn kqueue_provider_cross_compiles_for_intel_and_apple_silicon() {
    let version = match Command::new("zig").arg("version").output() {
        Ok(version) => version,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skipping macOS cross gate because Zig is unavailable");
            return;
        }
        Err(error) => panic!("Zig version command starts: {error}"),
    };
    if !version.status.success() {
        eprintln!("skipping macOS cross gate because Zig is unavailable");
        return;
    }
    for target in ["x86_64-macos", "aarch64-macos"] {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_macos_fixture_tree(directory.path());
        let output = format!("backend-kqueue-{target}");
        let mut command = Command::new("zig");
        command.args(["cc", "-target", target]);
        append_compile_args(&mut command, directory.path(), &output);
        let compiled = run(&mut command);
        assert!(
            compiled.status.success(),
            "Zig {target} kqueue compile: {}",
            output_text(&compiled)
        );
        assert!(directory.path().join(output).is_file());
    }
}

#[test]
fn kqueue_provider_runs_real_loopback_conformance_with_macos_clang() {
    if !cfg!(target_os = "macos") {
        eprintln!("skipping kqueue execution gate on a non-macOS host");
        return;
    }
    let version = run(Command::new("clang").arg("--version"));
    assert!(version.status.success(), "macOS Clang is required");
    let directory = tempfile::tempdir().expect("temporary directory");
    write_macos_fixture_tree(directory.path());
    let executable = "backend-kqueue-clang";
    let compiled = run(&mut compile_command("clang", directory.path(), executable));
    assert!(
        compiled.status.success(),
        "macOS Clang kqueue compile: {}",
        output_text(&compiled)
    );
    let executed = run(&mut Command::new(directory.path().join(executable)));
    assert!(
        executed.status.success(),
        "macOS Clang kqueue execute: {}",
        output_text(&executed)
    );
}
