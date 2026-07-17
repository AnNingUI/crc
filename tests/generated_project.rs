use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn run(command: &mut Command) -> Output {
    let output = command.output().expect("command starts");
    assert!(
        output.status.success(),
        "command failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn run_failure(command: &mut Command) -> Output {
    let output = command.output().expect("command starts");
    assert!(output.status.code().is_some_and(|code| code != 0));
    output
}

fn create_and_build_project() -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary directory");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .args(["create", "demo"])
        .current_dir(directory.path()));

    let project = directory.path().join("demo");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));

    for relative in [
        "CMakeLists.txt",
        "meson.build",
        "crc.toml",
        "crc/src/main.cr",
        "crc/include/main.hr",
        "crc/dist/main.c",
        "crc/dist/include/main.h",
        "crc/dist/include/cr_runtime.h",
        "crc/dist/include/cr_waker.h",
        "crc/dist/meson.build",
        "src/main.c",
    ] {
        assert!(project.join(relative).is_file(), "missing {relative}");
    }
    let config = fs::read_to_string(project.join("crc.toml")).expect("project config");
    assert!(config.contains("[runtime]"));
    assert!(config.contains("executor = \"manual\""));
    assert!(config.contains("backends = []"));
    assert!(!project.join("crc/include/cr_runtime.h").exists());
    assert!(!project.join("crc/dist/include/cr_executor.h").exists());
    assert!(!project.join("crc/dist/runtime").exists());
    let meson_manifest =
        fs::read_to_string(project.join("crc/dist/meson.build")).expect("Meson manifest");
    assert!(!meson_manifest.contains("cr_executor"));
    assert!(meson_manifest.contains("cr_generated_dependencies = []"));
    let artifact_manifest =
        fs::read_to_string(project.join("crc/dist/crc-artifacts.json")).expect("artifact manifest");
    assert!(!artifact_manifest.contains("\"kind\": \"executor-"));
    assert!(!artifact_manifest.contains("\"dependencies\""));
    directory
}

fn create_and_build_project_with_executor(executor: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().expect("temporary directory");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .args(["create", "executor-demo"])
        .current_dir(directory.path()));

    let project = directory.path().join("executor-demo");
    let config_path = project.join("crc.toml");
    let config = fs::read_to_string(&config_path).expect("project config");
    fs::write(
        &config_path,
        config.replacen(
            "executor = \"manual\"",
            &format!("executor = \"{executor}\""),
            1,
        ),
    )
    .expect("executor selection");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));
    directory
}

fn available_c_compiler() -> &'static str {
    ["clang", "gcc"]
        .into_iter()
        .find(|compiler| {
            Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("Clang or GCC is required")
}

fn executable_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

fn native_backend_source() -> &'static str {
    if cfg!(windows) {
        "cr_backend_iocp.c"
    } else if cfg!(target_os = "linux") {
        "cr_backend_epoll.c"
    } else if cfg!(target_os = "macos") {
        "cr_backend_kqueue.c"
    } else {
        panic!("native-net generated-project test requires Windows, Linux, or macOS");
    }
}

fn run_generated_executable(path: &Path) {
    let output = run(&mut Command::new(path));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("demo yielded 5"), "{stdout}");
    assert!(stdout.contains("demo completed with 9"), "{stdout}");
}

fn find_executable(root: &Path, name: &str) -> PathBuf {
    let file_name = executable_name(name);
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| entry.file_type().is_file() && entry.file_name() == OsStr::new(&file_name))
        .map(walkdir::DirEntry::into_path)
        .unwrap_or_else(|| panic!("missing executable {file_name} under {}", root.display()))
}

#[test]
fn created_project_compiles_and_runs_with_native_c() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    let executable = project.join(executable_name("native-demo"));
    run(Command::new(available_c_compiler())
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg("src/main.c")
        .arg("crc/dist/main.c")
        .args(["-I", "crc/dist/include", "-I", "include", "-o"])
        .arg(&executable)
        .current_dir(&project));
    run_generated_executable(&executable);
}

#[test]
fn created_project_builds_and_runs_with_cmake() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    run(Command::new("cmake")
        .args(["-S", ".", "-B", "build-cmake"])
        .current_dir(&project));
    run(Command::new("cmake")
        .args(["--build", "build-cmake"])
        .current_dir(&project));
    run_generated_executable(&find_executable(&project.join("build-cmake"), "demo"));
}

#[test]
fn created_project_builds_and_runs_with_meson() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    run(Command::new("meson")
        .args(["setup", "build-meson", "."])
        .current_dir(&project));
    run(Command::new("meson")
        .args(["compile", "-C", "build-meson"])
        .current_dir(&project));
    run_generated_executable(&find_executable(&project.join("build-meson"), "demo"));
}

#[test]
fn single_thread_project_packages_and_builds_each_runtime_source_once() {
    let directory = create_and_build_project_with_executor("single-thread");
    let project = directory.path().join("executor-demo");
    let runtime_sources = [
        "runtime/cr_executor_common.c",
        "runtime/cr_executor_single.c",
        "runtime/cr_executor_threaded_stub.c",
    ];

    for relative in [
        "include/cr_executor.h",
        "runtime/cr_executor_internal.h",
        runtime_sources[0],
        runtime_sources[1],
        runtime_sources[2],
    ] {
        assert!(
            project.join("crc/dist").join(relative).is_file(),
            "missing {relative}"
        );
    }
    assert!(
        !project
            .join("crc/dist/include/cr_executor_internal.h")
            .exists()
    );

    let meson_manifest =
        fs::read_to_string(project.join("crc/dist/meson.build")).expect("Meson manifest");
    assert_eq!(
        meson_manifest,
        "cr_generated_sources = files(\n  'main.c',\n  'runtime/cr_executor_common.c',\n  'runtime/cr_executor_single.c',\n  'runtime/cr_executor_threaded_stub.c',\n)\ncr_generated_dependencies = []\n"
    );
    for source in runtime_sources {
        assert_eq!(
            meson_manifest.matches(source).count(),
            1,
            "{source}: {meson_manifest}"
        );
    }
    let artifact_manifest =
        fs::read_to_string(project.join("crc/dist/crc-artifacts.json")).expect("artifact manifest");
    assert!(artifact_manifest.contains("\"output\": \"include/cr_executor.h\""));
    let artifact_manifest: serde_json::Value =
        serde_json::from_str(&artifact_manifest).expect("valid artifact manifest");
    let executor_records = artifact_manifest["artifacts"]
        .as_array()
        .expect("artifact records")
        .iter()
        .filter_map(|record| {
            let kind = record["kind"].as_str()?;
            kind.starts_with("executor-")
                .then(|| (record["output"].as_str().expect("artifact output"), kind))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        executor_records,
        vec![
            ("include/cr_executor.h", "executor-header"),
            ("runtime/cr_executor_internal.h", "executor-internal"),
            ("runtime/cr_executor_common.c", "executor-source"),
            ("runtime/cr_executor_single.c", "executor-source"),
            ("runtime/cr_executor_threaded_stub.c", "executor-source"),
        ]
    );

    let native_executable = project.join(executable_name("single-native"));
    run(Command::new(available_c_compiler())
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg("src/main.c")
        .arg("crc/dist/main.c")
        .args(runtime_sources.map(|source| format!("crc/dist/{source}")))
        .args([
            "-I",
            "crc/dist/include",
            "-I",
            "crc/dist/runtime",
            "-I",
            "include",
            "-o",
        ])
        .arg(&native_executable)
        .current_dir(&project));
    run_generated_executable(&native_executable);

    run(Command::new("cmake")
        .args(["-S", ".", "-B", "build-single-cmake"])
        .current_dir(&project));
    run(Command::new("cmake")
        .args(["--build", "build-single-cmake"])
        .current_dir(&project));
    run_generated_executable(&find_executable(
        &project.join("build-single-cmake"),
        "executor-demo",
    ));

    run(Command::new("meson")
        .args(["setup", "build-single-meson", "."])
        .current_dir(&project));
    run(Command::new("meson")
        .args(["compile", "-C", "build-single-meson"])
        .current_dir(&project));
    run_generated_executable(&find_executable(
        &project.join("build-single-meson"),
        "executor-demo",
    ));
}

#[test]
fn native_threaded_project_selects_only_the_host_backend() {
    let directory = create_and_build_project_with_executor("native-threaded");
    let project = directory.path().join("executor-demo");
    let runtime = project.join("crc/dist/runtime");
    let expected = if cfg!(windows) {
        "cr_executor_threaded_windows.c"
    } else if cfg!(unix) {
        "cr_executor_threaded_posix.c"
    } else {
        "cr_executor_threaded_stub.c"
    };
    assert!(runtime.join(expected).is_file());
    for excluded in [
        "cr_executor_threaded_windows.c",
        "cr_executor_threaded_posix.c",
        "cr_executor_threaded_stub.c",
    ] {
        if excluded != expected {
            assert!(!runtime.join(excluded).exists(), "unexpected {excluded}");
        }
    }

    let meson_manifest =
        fs::read_to_string(project.join("crc/dist/meson.build")).expect("Meson manifest");
    let dependency_manifest = if cfg!(unix) {
        "cr_generated_dependencies = [dependency('threads')]\n"
    } else {
        "cr_generated_dependencies = []\n"
    };
    assert_eq!(
        meson_manifest,
        format!(
            "cr_generated_sources = files(\n  'main.c',\n  'runtime/cr_executor_common.c',\n  'runtime/cr_executor_single.c',\n  'runtime/{expected}',\n)\n{dependency_manifest}"
        )
    );
    let artifact_manifest =
        fs::read_to_string(project.join("crc/dist/crc-artifacts.json")).expect("artifact manifest");
    assert_eq!(artifact_manifest.matches(expected).count(), 1);
    let artifact_manifest: serde_json::Value =
        serde_json::from_str(&artifact_manifest).expect("valid artifact manifest");
    let executor_outputs = artifact_manifest["artifacts"]
        .as_array()
        .expect("artifact records")
        .iter()
        .filter(|record| {
            record["kind"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("executor-"))
        })
        .map(|record| record["output"].as_str().expect("artifact output"))
        .collect::<Vec<_>>();
    let threaded_output = format!("runtime/{expected}");
    assert_eq!(
        executor_outputs,
        vec![
            "include/cr_executor.h",
            "runtime/cr_executor_internal.h",
            "runtime/cr_executor_common.c",
            "runtime/cr_executor_single.c",
            threaded_output.as_str(),
        ]
    );

    let runtime_sources = ["cr_executor_common.c", "cr_executor_single.c", expected];
    let native_executable = project.join(executable_name("threaded-native"));
    let mut native = Command::new(available_c_compiler());
    native
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg("src/main.c")
        .arg("crc/dist/main.c")
        .args(runtime_sources.map(|source| format!("crc/dist/runtime/{source}")))
        .args([
            "-I",
            "crc/dist/include",
            "-I",
            "crc/dist/runtime",
            "-I",
            "include",
        ]);
    if cfg!(unix) {
        native.arg("-pthread");
    }
    native
        .arg("-o")
        .arg(&native_executable)
        .current_dir(&project);
    run(&mut native);
    run_generated_executable(&native_executable);

    run(Command::new("cmake")
        .args(["-S", ".", "-B", "build-threaded-cmake"])
        .current_dir(&project));
    run(Command::new("cmake")
        .args(["--build", "build-threaded-cmake"])
        .current_dir(&project));
    run_generated_executable(&find_executable(
        &project.join("build-threaded-cmake"),
        "executor-demo",
    ));

    run(Command::new("meson")
        .args(["setup", "build-threaded-meson", "."])
        .current_dir(&project));
    run(Command::new("meson")
        .args(["compile", "-C", "build-threaded-meson"])
        .current_dir(&project));
    run_generated_executable(&find_executable(
        &project.join("build-threaded-meson"),
        "executor-demo",
    ));
}

#[test]
fn executor_selection_transition_is_atomic_and_removes_stale_artifacts() {
    let directory = create_and_build_project_with_executor("single-thread");
    let project = directory.path().join("executor-demo");
    let manifest_path = project.join("crc/dist/crc-artifacts.json");
    let previous_manifest = fs::read_to_string(&manifest_path).expect("executor manifest");
    let source_path = project.join("crc/src/main.cr");
    let valid_source = fs::read_to_string(&source_path).expect("valid source");
    let config_path = project.join("crc.toml");
    let manual_config = fs::read_to_string(&config_path)
        .expect("executor config")
        .replacen("executor = \"single-thread\"", "executor = \"manual\"", 1);
    fs::write(&config_path, manual_config).expect("manual executor selection");
    fs::write(
        &source_path,
        "__async int sequence(void) { return __await; }\n",
    )
    .expect("invalid source");

    let failure = run_failure(
        Command::new(env!("CARGO_BIN_EXE_crc"))
            .arg("build")
            .current_dir(&project),
    );
    assert!(!String::from_utf8_lossy(&failure.stderr).is_empty());
    assert_eq!(
        fs::read_to_string(&manifest_path).expect("preserved executor manifest"),
        previous_manifest
    );
    assert!(project.join("crc/dist/include/cr_executor.h").is_file());
    assert!(
        project
            .join("crc/dist/runtime/cr_executor_common.c")
            .is_file()
    );

    fs::write(&source_path, valid_source).expect("restore valid source");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));
    assert!(!project.join("crc/dist/include/cr_executor.h").exists());
    assert!(!project.join("crc/dist/runtime").exists());
    let manual_manifest = fs::read_to_string(&manifest_path).expect("manual manifest");
    assert!(!manual_manifest.contains("executor-"));
    let meson_manifest =
        fs::read_to_string(project.join("crc/dist/meson.build")).expect("manual Meson manifest");
    assert!(!meson_manifest.contains("cr_executor"));
    assert!(meson_manifest.contains("cr_generated_dependencies = []"));
}

#[test]
fn check_and_failed_build_preserve_the_published_artifacts() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    let generated = project.join("crc/dist/main.c");
    let previous = fs::read_to_string(&generated).expect("published source");

    fs::write(
        project.join("crc/src/main.cr"),
        "#include \"main.hr\"\n\n__async int sequence(void) {\n    __yield 6;\n    return 10;\n}\n",
    )
    .expect("valid edit");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("check")
        .current_dir(project.join("src")));
    assert_eq!(
        fs::read_to_string(&generated).expect("unchanged generated source"),
        previous
    );

    fs::write(
        project.join("crc/src/main.cr"),
        "__async int sequence(void) { return __await; }\n",
    )
    .expect("invalid edit");
    let failure = run_failure(
        Command::new(env!("CARGO_BIN_EXE_crc"))
            .arg("build")
            .current_dir(&project),
    );
    assert!(!String::from_utf8_lossy(&failure.stderr).is_empty());
    assert_eq!(
        fs::read_to_string(&generated).expect("previous generated source"),
        previous
    );
}

#[test]
fn check_validates_executor_target_without_publishing_artifacts() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    let generated = project.join("crc/dist/main.c");
    let manifest = project.join("crc/dist/crc-artifacts.json");
    let previous_source = fs::read_to_string(&generated).expect("published source");
    let previous_manifest = fs::read_to_string(&manifest).expect("published manifest");
    let config_path = project.join("crc.toml");
    let config = fs::read_to_string(&config_path).expect("project config");
    let portable = config
        .replacen("target = \"host\"", "target = \"wasm32-wasi\"", 1)
        .replacen("executor = \"manual\"", "executor = \"single-thread\"", 1);
    fs::write(&config_path, &portable).expect("write portable executor config");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("check")
        .current_dir(project.join("src")));
    assert_eq!(
        fs::read_to_string(&generated).expect("source after portable check"),
        previous_source
    );
    assert_eq!(
        fs::read_to_string(&manifest).expect("manifest after portable check"),
        previous_manifest
    );

    let unsupported = portable.replacen(
        "executor = \"single-thread\"",
        "executor = \"native-threaded\"",
        1,
    );
    fs::write(&config_path, unsupported).expect("write unsupported executor config");
    let failure = run_failure(
        Command::new(env!("CARGO_BIN_EXE_crc"))
            .arg("check")
            .current_dir(project.join("src")),
    );
    let stderr = String::from_utf8_lossy(&failure.stderr);
    assert!(stderr.contains("native-threaded"), "{stderr}");
    assert!(stderr.contains("wasm32-wasi"), "{stderr}");
    assert_eq!(
        fs::read_to_string(&generated).expect("source after rejected check"),
        previous_source
    );
    assert_eq!(
        fs::read_to_string(&manifest).expect("manifest after rejected check"),
        previous_manifest
    );
    assert!(!project.join("crc/dist/include/cr_executor.h").exists());
    assert!(!project.join("crc/dist/runtime").exists());
}

#[test]
fn check_validates_backend_selection_without_publishing_artifacts() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    let generated = project.join("crc/dist/main.c");
    let manifest = project.join("crc/dist/crc-artifacts.json");
    let previous_source = fs::read_to_string(&generated).expect("published source");
    let previous_manifest = fs::read_to_string(&manifest).expect("published manifest");
    let config_path = project.join("crc.toml");
    let config = fs::read_to_string(&config_path).expect("project config");
    let portable = config
        .replacen("target = \"host\"", "target = \"wasm32-wasi\"", 1)
        .replacen("backends = []", "backends = [\"memory-conformance\"]", 1);
    fs::write(&config_path, &portable).expect("write portable backend config");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("check")
        .current_dir(project.join("src")));
    assert_eq!(
        fs::read_to_string(&generated).expect("source after portable check"),
        previous_source
    );
    assert_eq!(
        fs::read_to_string(&manifest).expect("manifest after portable check"),
        previous_manifest
    );

    let unsupported = portable.replacen(
        "backends = [\"memory-conformance\"]",
        "backends = [\"native-net\"]",
        1,
    );
    fs::write(&config_path, &unsupported).expect("write unsupported backend config");
    let failure = run_failure(
        Command::new(env!("CARGO_BIN_EXE_crc"))
            .arg("check")
            .current_dir(project.join("src")),
    );
    let stderr = String::from_utf8_lossy(&failure.stderr);
    assert!(stderr.contains("native-net"), "{stderr}");
    assert!(stderr.contains("wasm32-wasi"), "{stderr}");
    assert_eq!(
        fs::read_to_string(&generated).expect("source after rejected check"),
        previous_source
    );
    assert_eq!(
        fs::read_to_string(&manifest).expect("manifest after rejected check"),
        previous_manifest
    );

    let duplicate = config.replacen(
        "backends = []",
        "backends = [\"memory-conformance\", \"memory-conformance\"]",
        1,
    );
    fs::write(&config_path, duplicate).expect("write duplicate backend config");
    let failure = run_failure(
        Command::new(env!("CARGO_BIN_EXE_crc"))
            .arg("build")
            .current_dir(&project),
    );
    let stderr = String::from_utf8_lossy(&failure.stderr);
    assert!(stderr.contains("duplicate"), "{stderr}");
    assert!(stderr.contains("memory-conformance"), "{stderr}");
    assert_eq!(
        fs::read_to_string(&generated).expect("source after rejected build"),
        previous_source
    );
    assert_eq!(
        fs::read_to_string(&manifest).expect("manifest after rejected build"),
        previous_manifest
    );
    assert!(!project.join("crc/dist/include/cr_backend.h").exists());
    assert!(!project.join("crc/dist/include/cr_net.h").exists());
    assert!(!project.join("crc/dist/runtime").exists());
}

#[test]
fn backend_selection_publishes_exact_deduplicated_provider_family() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    let dist = project.join("crc/dist");
    let snapshot = |root: &Path| {
        let mut files: Vec<_> = walkdir::WalkDir::new(root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| {
                let path = entry.into_path();
                let relative = path.strip_prefix(root).expect("artifact is below root");
                (
                    relative.to_path_buf(),
                    fs::read(&path).expect("artifact contents"),
                )
            })
            .collect();
        files.sort_by(|left, right| left.0.cmp(&right.0));
        files
    };
    let stage5 = snapshot(&dist);
    assert!(!dist.join("include/cr_backend.h").exists());
    assert!(!dist.join("include/cr_net.h").exists());
    assert!(!dist.join("runtime").exists());
    let config_path = project.join("crc.toml");
    let config = fs::read_to_string(&config_path).expect("project config");
    fs::write(
        &config_path,
        config.replacen(
            "backends = []",
            "backends = [\"memory-conformance\", \"native-net\"]",
            1,
        ),
    )
    .expect("write selected backend config");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));
    assert_ne!(snapshot(&dist), stage5);
    for relative in [
        "include/cr_backend.h",
        "include/cr_net.h",
        "runtime/cr_backend_internal.h",
        "runtime/cr_backend_common.c",
        "runtime/cr_backend_memory.c",
        "runtime/cr_net_recv.c",
    ] {
        assert!(dist.join(relative).is_file(), "missing {relative}");
    }
    assert!(dist.join("runtime").join(native_backend_source()).is_file());
    for unselected in [
        "cr_backend_iocp.c",
        "cr_backend_epoll.c",
        "cr_backend_kqueue.c",
    ] {
        if unselected != native_backend_source() {
            assert!(!dist.join("runtime").join(unselected).exists());
        }
    }
    let manifest = fs::read_to_string(dist.join("crc-artifacts.json")).expect("backend manifest");
    for shared in [
        "include/cr_backend.h",
        "include/cr_net.h",
        "runtime/cr_backend_internal.h",
        "runtime/cr_backend_common.c",
        "runtime/cr_net_recv.c",
    ] {
        assert_eq!(manifest.matches(shared).count(), 1, "{shared}: {manifest}");
    }
    assert!(manifest.contains("\"kind\": \"backend-awaitable-source\""));
    let meson = fs::read_to_string(dist.join("meson.build")).expect("Backend Meson manifest");
    if cfg!(windows) {
        assert!(manifest.contains("\"winsock\""));
        assert!(meson.contains("find_library('ws2_32'"));
        assert!(dist.join("crc-generated-dependencies.cmake").is_file());
    } else {
        assert!(!manifest.contains("\"winsock\""));
        assert!(!meson.contains("ws2_32"));
        assert!(!dist.join("crc-generated-dependencies.cmake").exists());
    }
}

#[test]
fn selected_backends_build_and_run_with_direct_c_cmake_and_meson() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    let config_path = project.join("crc.toml");
    let config = fs::read_to_string(&config_path).expect("project config");
    fs::write(
        &config_path,
        config.replacen(
            "backends = []",
            "backends = [\"memory-conformance\", \"native-net\"]",
            1,
        ),
    )
    .expect("write selected backend config");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));

    let dist = project.join("crc/dist");
    let mut generated_sources = walkdir::WalkDir::new(&dist)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "c")
        })
        .map(|entry| entry.into_path())
        .collect::<Vec<_>>();
    generated_sources.sort();
    let direct_executable = project.join(executable_name("backend-direct"));
    let mut direct = Command::new(available_c_compiler());
    direct
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror"])
        .arg("src/main.c")
        .args(&generated_sources)
        .args(["-I", "crc/dist/include", "-I", "include", "-o"])
        .arg(&direct_executable)
        .current_dir(&project);
    if cfg!(windows) {
        direct.arg("-lws2_32");
    }
    run(&mut direct);
    run_generated_executable(&direct_executable);

    run(Command::new("cmake")
        .args(["-S", ".", "-B", "build-backend-cmake"])
        .current_dir(&project));
    run(Command::new("cmake")
        .args(["--build", "build-backend-cmake"])
        .current_dir(&project));
    run_generated_executable(&find_executable(
        &project.join("build-backend-cmake"),
        "demo",
    ));

    run(Command::new("meson")
        .args(["setup", "build-backend-meson", "."])
        .current_dir(&project));
    run(Command::new("meson")
        .args(["compile", "-C", "build-backend-meson"])
        .current_dir(&project));
    run_generated_executable(&find_executable(
        &project.join("build-backend-meson"),
        "demo",
    ));
}

#[test]
fn backend_selection_transition_removes_stale_provider_and_dependency_files() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    let config_path = project.join("crc.toml");
    let initial = fs::read_to_string(&config_path).expect("project config");
    fs::write(
        &config_path,
        initial.replacen("backends = []", "backends = [\"memory-conformance\"]", 1),
    )
    .expect("write memory backend config");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));
    let dist = project.join("crc/dist");
    assert!(dist.join("runtime/cr_backend_memory.c").is_file());
    assert!(!dist.join("runtime").join(native_backend_source()).exists());

    let memory = fs::read_to_string(&config_path).expect("memory config");
    fs::write(
        &config_path,
        memory.replacen("memory-conformance", "native-net", 1),
    )
    .expect("write native backend config");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));
    assert!(!dist.join("runtime/cr_backend_memory.c").exists());
    assert!(dist.join("runtime").join(native_backend_source()).is_file());

    let native = fs::read_to_string(&config_path).expect("native config");
    fs::write(
        &config_path,
        native.replacen("backends = [\"native-net\"]", "backends = []", 1),
    )
    .expect("write empty backend config");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));
    assert!(!dist.join("include/cr_backend.h").exists());
    assert!(!dist.join("include/cr_net.h").exists());
    assert!(!dist.join("runtime").exists());
    assert!(!dist.join("crc-generated-dependencies.cmake").exists());
    let manifest =
        fs::read_to_string(dist.join("crc-artifacts.json")).expect("empty backend manifest");
    assert!(!manifest.contains("\"dependencies\""));
}

#[test]
fn successful_build_removes_stale_artifacts_and_records_a_manifest() {
    let directory = create_and_build_project();
    let project = directory.path().join("demo");
    let extra_source = project.join("crc/src/extra.cr");
    fs::write(&extra_source, "int extra(void) { return 1; }\n").expect("extra source");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));
    assert!(project.join("crc/dist/extra.c").is_file());

    fs::remove_file(extra_source).expect("remove source");
    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("build")
        .current_dir(&project));
    assert!(!project.join("crc/dist/extra.c").exists());

    let manifest =
        fs::read_to_string(project.join("crc/dist/crc-artifacts.json")).expect("artifact manifest");
    assert!(manifest.contains("\"runtime_abi_version\": 3"));
    assert!(manifest.contains("\"output\": \"main.c\""));
    assert!(manifest.contains("\"output\": \"include/cr_waker.h\""));
    assert!(manifest.contains("\"kind\": \"runtime-extension\""));
    assert!(!manifest.contains("extra.c"));

    run(Command::new(env!("CARGO_BIN_EXE_crc"))
        .arg("clean")
        .current_dir(project.join("src")));
    assert!(!project.join("crc/dist").exists());
    assert!(project.join("crc/src/main.cr").is_file());
}
