use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::backend_runtime::{BackendArtifact, memory_net_awaitable_artifacts};
use crc_lib::executor_runtime::portable_artifacts;
use crc_lib::runtime_abi::runtime_header;
use crc_lib::waker_abi::waker_header;

const WASI_SDK_VERSION: &str = include_str!("../tools/wasi-sdk.version");
const WASM_TOOLS_VERSION: &str = include_str!("../tools/wasm-tools.version");

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
        .join("tests/fixtures/backend/net")
        .join(name)
}

fn lifecycle_fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/backend/lifecycle")
        .join(name)
}

fn write_artifact(root: &Path, artifact: &BackendArtifact) {
    let path = root.join(artifact.path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("artifact directory");
    }
    fs::write(path, artifact.contents).expect("backend artifact");
}

fn write_runtime(root: &Path, with_executor: bool) {
    for artifact in memory_net_awaitable_artifacts() {
        write_artifact(root, &artifact);
    }
    fs::write(root.join("include/cr_runtime.h"), runtime_header()).expect("runtime header");
    fs::write(root.join("include/cr_waker.h"), waker_header()).expect("Waker header");
    if with_executor {
        for artifact in portable_artifacts() {
            let path = root.join(artifact.path);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("executor artifact directory");
            }
            fs::write(path, artifact.contents).expect("executor artifact");
        }
    }
}

fn backend_source_paths() -> Vec<&'static str> {
    memory_net_awaitable_artifacts()
        .into_iter()
        .filter(|artifact| artifact.is_source)
        .map(|artifact| artifact.path)
        .collect()
}

fn executor_source_paths() -> Vec<&'static str> {
    portable_artifacts()
        .iter()
        .filter(|artifact| artifact.is_source)
        .map(|artifact| artifact.path)
        .collect()
}

fn compile_native(compiler: &str, fixture_name: &str, with_executor: bool) {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_runtime(directory.path(), with_executor);
    fs::copy(fixture(fixture_name), directory.path().join(fixture_name))
        .expect("awaitable fixture");
    let executable = if cfg!(windows) {
        format!("net-{}-{compiler}.exe", fixture_name.trim_end_matches(".c"))
    } else {
        format!("net-{}-{compiler}", fixture_name.trim_end_matches(".c"))
    };
    let mut command = Command::new(compiler);
    command.args(["-std=c11", "-Wall", "-Wextra", "-Werror", fixture_name]);
    command.args(backend_source_paths());
    if with_executor {
        command.args(executor_source_paths());
    }
    command
        .args(["-I", "include", "-I", "runtime", "-o"])
        .arg(&executable)
        .current_dir(directory.path());
    let compiled = run(&mut command);
    assert!(
        compiled.status.success(),
        "{compiler} {fixture_name}: {}",
        output_text(&compiled)
    );
    let executed = run(&mut Command::new(directory.path().join(executable)));
    assert!(
        executed.status.success(),
        "{compiler} {fixture_name}: {}",
        output_text(&executed)
    );
}

fn required_wasm() -> bool {
    env::var("CRC_REQUIRE_WASM").is_ok_and(|value| value == "1")
}

fn discover_wasm_tools() -> Option<(PathBuf, PathBuf)> {
    let Some(wasi_root) = env::var_os("WASI_SDK_PATH").map(PathBuf::from) else {
        if required_wasm() {
            panic!("CRC_REQUIRE_WASM=1 requires WASI_SDK_PATH");
        }
        eprintln!("skipping net awaitable WASI gate: WASI_SDK_PATH is not set");
        return None;
    };
    let wasm_tools = PathBuf::from(if cfg!(windows) {
        "wasm-tools.exe"
    } else {
        "wasm-tools"
    });
    let version = run(Command::new(&wasm_tools).arg("--version"));
    if !version.status.success() {
        if required_wasm() {
            panic!("wasm-tools is required: {}", output_text(&version));
        }
        eprintln!("skipping net awaitable WASI gate: wasm-tools is unavailable");
        return None;
    }
    Some((wasi_root, wasm_tools))
}

#[test]
fn reference_receive_awaitable_runs_manual_and_executor_paths_natively() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        compile_native(compiler, "manual.c", false);
        compile_native(compiler, "executor.c", true);
    }
}

#[test]
fn reference_receive_awaitable_lifecycle_drop_and_quiescence_contracts() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_runtime(directory.path(), false);
        fs::copy(
            lifecycle_fixture("awaitable_lifecycle.c"),
            directory.path().join("awaitable_lifecycle.c"),
        )
        .expect("awaitable lifecycle fixture");
        let executable = if cfg!(windows) {
            format!("awaitable-lifecycle-{compiler}.exe")
        } else {
            format!("awaitable-lifecycle-{compiler}")
        };
        let mut command = Command::new(compiler);
        command.args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "awaitable_lifecycle.c",
        ]);
        command.args(backend_source_paths());
        command
            .args(["-I", "include", "-I", "runtime", "-o"])
            .arg(&executable)
            .current_dir(directory.path());
        let compiled = run(&mut command);
        assert!(
            compiled.status.success(),
            "{compiler}: {}",
            output_text(&compiled)
        );
        let executed = run(&mut Command::new(directory.path().join(executable)));
        assert!(
            executed.status.success(),
            "{compiler}: {}",
            output_text(&executed)
        );
    }
}

#[test]
fn reference_receive_awaitable_compiles_and_links_for_wasm32_wasi() {
    let Some((wasi_root, wasm_tools)) = discover_wasm_tools() else {
        return;
    };
    let installed_wasi = fs::read_to_string(wasi_root.join("VERSION")).expect("WASI VERSION file");
    assert_eq!(installed_wasi.lines().next(), Some(WASI_SDK_VERSION.trim()));
    let version = run(Command::new(&wasm_tools).arg("--version"));
    assert!(
        String::from_utf8_lossy(&version.stdout)
            .starts_with(&format!("wasm-tools {} ", WASM_TOOLS_VERSION.trim()))
    );
    let clang = wasi_root
        .join("bin")
        .join(if cfg!(windows) { "clang.exe" } else { "clang" });

    for (fixture_name, with_executor) in [("manual.c", false), ("executor.c", true)] {
        let directory = tempfile::tempdir().expect("temporary directory");
        write_runtime(directory.path(), with_executor);
        fs::copy(fixture(fixture_name), directory.path().join(fixture_name))
            .expect("awaitable fixture");
        let output = format!("{}.wasm", fixture_name.trim_end_matches(".c"));
        let mut command = Command::new(&clang);
        command
            .arg("--target=wasm32-wasi")
            .arg(format!(
                "--sysroot={}",
                wasi_root.join("share/wasi-sysroot").display()
            ))
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", fixture_name]);
        command.args(backend_source_paths());
        if with_executor {
            command.args(executor_source_paths());
        }
        command
            .args(["-I", "include", "-I", "runtime", "-o"])
            .arg(&output)
            .current_dir(directory.path());
        let compiled = run(&mut command);
        assert!(
            compiled.status.success(),
            "{fixture_name}: {}",
            output_text(&compiled)
        );
        let validated = run(Command::new(&wasm_tools)
            .args(["validate", &output])
            .current_dir(directory.path()));
        assert!(validated.status.success(), "{}", output_text(&validated));
        let printed = run(Command::new(&wasm_tools)
            .args(["print", &output])
            .current_dir(directory.path()));
        assert!(printed.status.success(), "{}", output_text(&printed));
        let wat = String::from_utf8_lossy(&printed.stdout).to_ascii_lowercase();
        assert!(
            wat.lines()
                .filter(|line| line.contains("(memory"))
                .all(|line| !line.contains(" shared")),
            "shared memory found in {output}"
        );
        assert!(
            !wat.contains("atomic."),
            "atomic instruction found in {output}"
        );
        assert!(
            !wat.contains(".atomic"),
            "atomic instruction found in {output}"
        );
    }
}

#[test]
fn provider_stays_waker_free_and_awaitable_remains_an_unpublished_adapter() {
    let artifacts = memory_net_awaitable_artifacts();
    let provider = artifacts
        .iter()
        .find(|artifact| artifact.path == "runtime/cr_backend_memory.c")
        .expect("memory provider artifact")
        .contents
        .to_ascii_lowercase();
    let awaitable = artifacts
        .iter()
        .find(|artifact| artifact.path == "runtime/cr_net_recv.c")
        .expect("reference awaitable artifact")
        .contents;
    assert!(!provider.contains("waker"));
    assert!(awaitable.contains("cr_waker_clone"));
    assert!(awaitable.contains("cr_waker_wake"));
    assert!(awaitable.contains("UINT64_C(0),\n    cr_net_awaitable_poll"));

    let mut config = crc_lib::config::Config::default();
    config.runtime.backends = vec![crc_lib::config::BackendSelection::MemoryConformance];
    let directory = tempfile::tempdir().expect("temporary directory");
    let root = directory.path();
    fs::create_dir_all(root.join("crc/src")).expect("source directory");
    fs::create_dir_all(root.join("crc/include")).expect("header directory");
    fs::write(
        root.join("crc/src/main.cr"),
        "int value(void) { return 1; }\n",
    )
    .expect("source");
    crc_lib::Compiler::new(config)
        .build_project(root)
        .expect("selected project builds without publishing Task 4 adapter");
    assert!(!root.join("crc/dist/runtime/cr_net_recv.c").exists());
}
