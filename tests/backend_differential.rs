use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::backend_runtime::{
    BackendArtifact, epoll_artifacts, iocp_artifacts, kqueue_artifacts, memory_artifacts,
};

const EXPECTED_TRANSCRIPT: &[&str] = &[
    "CRDIFF cancel terminal=3 bytes=0 category=0 callbacks=1 wakes=0 quiescent=1 reusable=1 pump=1 events=1",
    "CRDIFF eof terminal=1 bytes=0 category=0 callbacks=1 wakes=0 quiescent=1 reusable=1 pump=1 events=1",
    "CRDIFF error terminal=2 bytes=0 category=6 callbacks=1 wakes=0 quiescent=1 reusable=1 pump=1 events=1",
    "CRDIFF interrupt terminal=0 bytes=0 category=0 callbacks=0 wakes=0 quiescent=1 reusable=1 pump=3 events=1",
    "CRDIFF shutdown terminal=3 bytes=0 category=0 callbacks=1 wakes=0 quiescent=1 reusable=0 pump=1 events=1",
    "CRDIFF success terminal=1 bytes=5 category=0 callbacks=1 wakes=0 quiescent=1 reusable=1 pump=1 events=1",
    "CRDIFF timeout terminal=0 bytes=0 category=0 callbacks=0 wakes=0 quiescent=1 reusable=1 pump=2 events=0",
];

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

fn fixture(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/backend")
        .join(path)
}

fn write_artifact(root: &Path, artifact: &BackendArtifact) {
    let path = root.join(artifact.path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("artifact directory");
    }
    fs::write(path, artifact.contents).expect("backend artifact");
}

fn executable_name(provider: &str, compiler: &str) -> String {
    if cfg!(windows) {
        format!("backend-differential-{provider}-{compiler}.exe")
    } else {
        format!("backend-differential-{provider}-{compiler}")
    }
}

fn assert_transcript(provider: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{provider} differential fixture: {}",
        output_text(output)
    );
    let mut actual = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("CRDIFF "))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    actual.sort();
    assert_eq!(
        actual,
        EXPECTED_TRANSCRIPT,
        "{provider} normalized differential transcript\n{}",
        output_text(output)
    );
}

fn run_memory_fixture(compiler: &str) {
    let directory = tempfile::tempdir().expect("temporary directory");
    for artifact in memory_artifacts() {
        write_artifact(directory.path(), &artifact);
    }
    fs::copy(
        fixture("memory/lifecycle.c"),
        directory.path().join("differential.c"),
    )
    .expect("memory differential fixture");
    fs::copy(
        fixture("differential/transcript.h"),
        directory.path().join("transcript.h"),
    )
    .expect("differential transcript header");
    let executable = executable_name("memory", compiler);
    let source_paths = memory_artifacts()
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
        "-DCR_BACKEND_DIFFERENTIAL=1",
        "differential.c",
    ]);
    command.args(source_paths);
    command
        .args(["-I", "include", "-I", "runtime", "-I", ".", "-o"])
        .arg(&executable)
        .current_dir(directory.path());
    let compiled = run(&mut command);
    assert!(
        compiled.status.success(),
        "memory {compiler} compile: {}",
        output_text(&compiled)
    );
    let executed = run(&mut Command::new(directory.path().join(executable)));
    assert_transcript("memory", &executed);
}

struct NativeFixture {
    provider: &'static str,
    artifacts: Vec<BackendArtifact>,
    fixture_files: &'static [&'static str],
    forced_include: &'static str,
    link_args: &'static [&'static str],
}

fn host_native_fixture() -> Option<NativeFixture> {
    if cfg!(windows) {
        Some(NativeFixture {
            provider: "iocp",
            artifacts: iocp_artifacts(),
            fixture_files: &["iocp.c", "windows_helpers.h", "windows_hooks.h"],
            forced_include: "windows_hooks.h",
            link_args: &["-lws2_32"],
        })
    } else if cfg!(target_os = "linux") {
        Some(NativeFixture {
            provider: "epoll",
            artifacts: epoll_artifacts(),
            fixture_files: &["epoll.c", "linux_helpers.h", "linux_hooks.h"],
            forced_include: "linux_hooks.h",
            link_args: &["-pthread"],
        })
    } else if cfg!(target_os = "macos") {
        Some(NativeFixture {
            provider: "kqueue",
            artifacts: kqueue_artifacts(),
            fixture_files: &["kqueue.c", "macos_helpers.h", "macos_hooks.h"],
            forced_include: "macos_hooks.h",
            link_args: &["-pthread"],
        })
    } else {
        None
    }
}

fn run_native_fixture(compiler: &str, fixture_config: &NativeFixture) {
    let directory = tempfile::tempdir().expect("temporary directory");
    for artifact in &fixture_config.artifacts {
        write_artifact(directory.path(), artifact);
    }
    for name in fixture_config.fixture_files {
        fs::copy(
            fixture(&format!("native/{name}")),
            directory.path().join(name),
        )
        .expect("native differential fixture");
    }
    fs::copy(
        fixture("differential/transcript.h"),
        directory.path().join("transcript.h"),
    )
    .expect("differential transcript header");
    let executable = executable_name(fixture_config.provider, compiler);
    let source_paths = fixture_config
        .artifacts
        .iter()
        .filter(|artifact| artifact.is_source)
        .map(|artifact| artifact.path)
        .collect::<Vec<_>>();
    let source = format!("{}.c", fixture_config.provider);
    let mut command = Command::new(compiler);
    command.args([
        "-std=c11",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-DCR_BACKEND_DIFFERENTIAL=1",
        "-include",
        fixture_config.forced_include,
        &source,
    ]);
    command.args(source_paths);
    command
        .args(["-I", "include", "-I", "runtime", "-I", "."])
        .args(fixture_config.link_args)
        .args(["-o"])
        .arg(&executable)
        .current_dir(directory.path());
    let compiled = run(&mut command);
    assert!(
        compiled.status.success(),
        "{} {compiler} compile: {}",
        fixture_config.provider,
        output_text(&compiled)
    );
    let executed = run(&mut Command::new(directory.path().join(executable)));
    assert_transcript(fixture_config.provider, &executed);
}

#[test]
fn portable_memory_provider_matches_the_canonical_transcript() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        run_memory_fixture(compiler);
    }
}

#[test]
fn host_native_provider_matches_the_canonical_transcript() {
    let Some(fixture_config) = host_native_fixture() else {
        eprintln!("skipping native differential gate on an unsupported host");
        return;
    };
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        run_native_fixture(compiler, &fixture_config);
    }
}
