use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use crc_lib::backend_runtime::{BackendArtifact, memory_artifacts, memory_net_awaitable_artifacts};
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
        .join("tests/fixtures/backend/memory")
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

fn write_runtime(root: &Path) {
    for artifact in memory_artifacts() {
        write_artifact(root, &artifact);
    }
    for name in ["lifecycle.c", "owner.c", "owner_hooks.h"] {
        fs::copy(fixture(name), root.join(name)).expect("memory fixture");
    }
}

fn source_paths() -> Vec<&'static str> {
    memory_artifacts()
        .into_iter()
        .filter(|artifact| artifact.is_source)
        .map(|artifact| artifact.path)
        .collect()
}

fn awaitable_source_paths() -> Vec<&'static str> {
    memory_net_awaitable_artifacts()
        .into_iter()
        .filter(|artifact| artifact.is_source)
        .map(|artifact| artifact.path)
        .collect()
}

fn required_wasm() -> bool {
    env::var("CRC_REQUIRE_WASM").is_ok_and(|value| value == "1")
}

fn discover_wasm_tools() -> Option<(PathBuf, PathBuf)> {
    let Some(wasi_root) = env::var_os("WASI_SDK_PATH").map(PathBuf::from) else {
        if required_wasm() {
            panic!("CRC_REQUIRE_WASM=1 requires WASI_SDK_PATH");
        }
        eprintln!("skipping memory provider WASI gate: WASI_SDK_PATH is not set");
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
        eprintln!("skipping memory provider WASI gate: wasm-tools is unavailable");
        return None;
    }
    Some((wasi_root, wasm_tools))
}

fn compile_native_fixture(compiler: &str, fixture_name: &str, owner_hook: bool) {
    let directory = tempfile::tempdir().expect("temporary directory");
    write_runtime(directory.path());
    let executable = if cfg!(windows) {
        format!(
            "memory-{}-{compiler}.exe",
            fixture_name.trim_end_matches(".c")
        )
    } else {
        format!("memory-{}-{compiler}", fixture_name.trim_end_matches(".c"))
    };
    let mut command = Command::new(compiler);
    command.args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-fno-inline"]);
    if owner_hook {
        command.args([
            "-include",
            "owner_hooks.h",
            "-DCR_BACKEND_CURRENT_OWNER_TOKEN=test_owner_token",
        ]);
    }
    command.arg(fixture_name);
    command.args(source_paths());
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
    let executed = run(&mut Command::new(directory.path().join(&executable)));
    assert!(
        executed.status.success(),
        "{compiler} {fixture_name}: {}",
        output_text(&executed)
    );
}

#[test]
fn memory_provider_runs_lifecycle_and_owner_contracts_with_native_c11() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        compile_native_fixture(compiler, "lifecycle.c", false);
        compile_native_fixture(compiler, "owner.c", true);
    }
}

#[test]
fn memory_provider_lifecycle_races_and_allocation_contracts() {
    let compilers = available_compilers();
    assert!(!compilers.is_empty(), "Clang or GCC is required");
    for compiler in compilers {
        let directory = tempfile::tempdir().expect("temporary directory");
        for artifact in memory_artifacts() {
            write_artifact(directory.path(), &artifact);
        }
        fs::copy(
            lifecycle_fixture("provider_lifecycle.c"),
            directory.path().join("provider_lifecycle.c"),
        )
        .expect("provider lifecycle fixture");
        let executable = if cfg!(windows) {
            format!("provider-lifecycle-{compiler}.exe")
        } else {
            format!("provider-lifecycle-{compiler}")
        };
        let mut command = Command::new(compiler);
        command.args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "provider_lifecycle.c",
        ]);
        command.args(source_paths());
        command
            .args(["-I", "include", "-I", "runtime", "-o"])
            .arg(&executable)
            .current_dir(directory.path());
        let compiled = run(&mut command);
        assert!(
            compiled.status.success(),
            "{compiler} provider lifecycle: {}",
            output_text(&compiled)
        );
        let executed = run(&mut Command::new(directory.path().join(&executable)));
        assert!(
            executed.status.success(),
            "{compiler} provider lifecycle: {}",
            output_text(&executed)
        );

        let directory = tempfile::tempdir().expect("temporary directory");
        for artifact in memory_net_awaitable_artifacts() {
            write_artifact(directory.path(), &artifact);
        }
        fs::write(
            directory.path().join("include/cr_runtime.h"),
            runtime_header(),
        )
        .expect("runtime header");
        fs::write(directory.path().join("include/cr_waker.h"), waker_header())
            .expect("Waker header");
        for name in ["allocation.c", "allocator_hooks.h"] {
            fs::copy(lifecycle_fixture(name), directory.path().join(name))
                .expect("allocator fixture");
        }
        let executable = if cfg!(windows) {
            format!("backend-allocation-{compiler}.exe")
        } else {
            format!("backend-allocation-{compiler}")
        };
        let mut command = Command::new(compiler);
        command.args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-include",
            "allocator_hooks.h",
            "-DCR_BACKEND_CALLOC=test_backend_calloc",
            "-DCR_BACKEND_FREE=test_backend_free",
            "-DCR_BACKEND_MEMORY_CALLOC=test_provider_calloc",
            "-DCR_BACKEND_MEMORY_FREE=test_provider_free",
            "-DCR_BACKEND_TRACKING_CALLOC=test_tracking_calloc",
            "-DCR_BACKEND_TRACKING_FREE=test_tracking_free",
            "-DCR_BACKEND_AWAITABLE_CALLOC=test_awaitable_calloc",
            "-DCR_BACKEND_AWAITABLE_FREE=test_awaitable_free",
            "-DCR_BACKEND_OPERATION_CALLOC=test_operation_calloc",
            "-DCR_BACKEND_OPERATION_FREE=test_operation_free",
            "allocation.c",
        ]);
        command.args(awaitable_source_paths());
        command
            .args(["-I", "include", "-I", "runtime", "-o"])
            .arg(&executable)
            .current_dir(directory.path());
        let compiled = run(&mut command);
        assert!(
            compiled.status.success(),
            "{compiler} allocation lifecycle: {}",
            output_text(&compiled)
        );
        let executed = run(&mut Command::new(directory.path().join(&executable)));
        assert!(
            executed.status.success(),
            "{compiler} allocation lifecycle: {}",
            output_text(&executed)
        );
    }
}

#[test]
fn memory_provider_links_for_wasi_without_shared_memory_or_threads() {
    let Some((wasi_root, wasm_tools)) = discover_wasm_tools() else {
        return;
    };
    let installed_wasi = fs::read_to_string(wasi_root.join("VERSION")).expect("WASI VERSION file");
    assert_eq!(installed_wasi.lines().next(), Some(WASI_SDK_VERSION.trim()));
    let tools_version = run(Command::new(&wasm_tools).arg("--version"));
    assert!(
        tools_version.status.success(),
        "{}",
        output_text(&tools_version)
    );
    assert!(
        String::from_utf8_lossy(&tools_version.stdout)
            .starts_with(&format!("wasm-tools {} ", WASM_TOOLS_VERSION.trim()))
    );
    let clang = wasi_root
        .join("bin")
        .join(if cfg!(windows) { "clang.exe" } else { "clang" });
    assert!(clang.is_file(), "missing WASI Clang: {}", clang.display());

    let directory = tempfile::tempdir().expect("temporary directory");
    write_runtime(directory.path());
    let mut command = Command::new(clang);
    command
        .arg("--target=wasm32-wasi")
        .arg(format!(
            "--sysroot={}",
            wasi_root.join("share/wasi-sysroot").display()
        ))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "lifecycle.c"]);
    command.args(source_paths());
    command
        .args([
            "-I",
            "include",
            "-I",
            "runtime",
            "-o",
            "backend-memory.wasm",
        ])
        .current_dir(directory.path());
    let compiled = run(&mut command);
    assert!(compiled.status.success(), "{}", output_text(&compiled));

    let validated = run(Command::new(&wasm_tools)
        .args(["validate", "backend-memory.wasm"])
        .current_dir(directory.path()));
    assert!(validated.status.success(), "{}", output_text(&validated));
    let printed = run(Command::new(&wasm_tools)
        .args(["print", "backend-memory.wasm"])
        .current_dir(directory.path()));
    assert!(printed.status.success(), "{}", output_text(&printed));
    let wat = String::from_utf8_lossy(&printed.stdout).to_ascii_lowercase();
    assert!(
        !wat.contains(" shared"),
        "shared memory found in Wasm module"
    );
    assert!(
        !wat.contains("atomic."),
        "atomic instruction found in Wasm module"
    );
    assert!(
        !wat.contains(".atomic"),
        "atomic instruction found in Wasm module"
    );
    assert!(
        directory
            .path()
            .join(OsStr::new("backend-memory.wasm"))
            .is_file()
    );
}

#[test]
fn memory_artifact_query_does_not_change_default_project_publication() {
    let artifacts = memory_artifacts();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.path == "include/cr_backend.h")
    );
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
        .expect("selected project builds without publishing Task 3 artifacts");
    assert!(!root.join("crc/dist/include/cr_backend.h").exists());
    assert!(!root.join("crc/dist/include/cr_net.h").exists());
    assert!(!root.join("crc/dist/runtime/cr_backend_memory.c").exists());
}
