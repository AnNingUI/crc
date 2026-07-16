//! Content-fingerprinted incremental project builds.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildStatus {
    Unchanged,
    Rebuilt,
}

pub struct IncrementalCompiler {
    config: Config,
    compiler: crate::Compiler,
    last_fingerprint: Option<u64>,
}

impl IncrementalCompiler {
    pub fn new(config: Config) -> Self {
        Self {
            compiler: crate::Compiler::new(config.clone()),
            config,
            last_fingerprint: None,
        }
    }

    pub fn update_config(&mut self, config: Config) {
        self.compiler = crate::Compiler::new(config.clone());
        self.config = config;
    }

    pub fn build(&mut self, project_root: &Path) -> Result<BuildStatus> {
        let fingerprint = project_fingerprint(project_root, &self.config)?;
        if self.last_fingerprint == Some(fingerprint) {
            return Ok(BuildStatus::Unchanged);
        }
        self.compiler.build_project(project_root)?;
        self.last_fingerprint = Some(fingerprint);
        Ok(BuildStatus::Rebuilt)
    }
}

fn project_fingerprint(project_root: &Path, config: &Config) -> Result<u64> {
    let mut paths = Vec::new();
    collect_files(
        &project_root.join(&config.project.crate_dir),
        &["cr", "hr", "h"],
        &mut paths,
    )?;
    collect_files(
        &project_root.join(&config.project.include_dir),
        &["hr", "h"],
        &mut paths,
    )?;
    collect_files(&project_root.join("include"), &["h"], &mut paths)?;
    for include in &config.preprocessor.include_paths {
        collect_files(&project_root.join(include), &["hr", "h"], &mut paths)?;
    }
    for include in &config.preprocessor.forced_includes {
        let include = project_root.join(include);
        if include.is_file() {
            paths.push(include);
        }
    }
    let config_path = project_root.join("crc.toml");
    if config_path.is_file() {
        paths.push(config_path);
    }
    paths.sort();
    paths.dedup();

    let mut hasher = DefaultHasher::new();
    env!("CARGO_PKG_VERSION").hash(&mut hasher);
    crate::runtime_abi::CR_RUNTIME_ABI_VERSION.hash(&mut hasher);
    toml::to_string(config)?.hash(&mut hasher);
    for path in paths {
        path.strip_prefix(project_root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/")
            .hash(&mut hasher);
        std::fs::read(&path)?.hash(&mut hasher);
    }
    Ok(hasher.finish())
}

fn collect_files(directory: &Path, extensions: &[&str], paths: &mut Vec<PathBuf>) -> Result<()> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in walkdir::WalkDir::new(directory) {
        let entry = entry?;
        if entry.file_type().is_file()
            && entry.path().extension().is_some_and(|extension| {
                extension
                    .to_str()
                    .is_some_and(|extension| extensions.contains(&extension))
            })
        {
            paths.push(entry.into_path());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::config::{BackendSelection, ExecutorSelection};

    use super::*;

    #[test]
    fn skips_a_content_identical_project_build() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path();
        fs::create_dir_all(root.join("crc/src")).expect("source directory");
        fs::create_dir_all(root.join("crc/include")).expect("header directory");
        fs::write(
            root.join("crc/src/main.cr"),
            "int value(void) { return 1; }\n",
        )
        .expect("source");
        let config = Config::default();
        let mut compiler = IncrementalCompiler::new(config);

        assert_eq!(
            compiler.build(root).expect("initial build"),
            BuildStatus::Rebuilt
        );
        let generated = root.join("crc/dist/main.c");
        let modified = fs::metadata(&generated)
            .and_then(|metadata| metadata.modified())
            .expect("generated timestamp");
        assert_eq!(
            compiler.build(root).expect("no-op build"),
            BuildStatus::Unchanged
        );
        assert_eq!(
            fs::metadata(generated)
                .and_then(|metadata| metadata.modified())
                .expect("unchanged timestamp"),
            modified
        );
    }

    #[test]
    fn executor_selection_change_rebuilds_and_removes_stale_runtime_artifacts() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path();
        fs::create_dir_all(root.join("crc/src")).expect("source directory");
        fs::create_dir_all(root.join("crc/include")).expect("header directory");
        fs::write(
            root.join("crc/src/main.cr"),
            "int value(void) { return 1; }\n",
        )
        .expect("source");

        let mut single_thread = Config::default();
        single_thread.runtime.executor = ExecutorSelection::SingleThread;
        let mut compiler = IncrementalCompiler::new(single_thread.clone());
        assert_eq!(
            compiler.build(root).expect("single-thread build"),
            BuildStatus::Rebuilt
        );
        assert!(root.join("crc/dist/include/cr_executor.h").is_file());
        assert!(
            root.join("crc/dist/runtime/cr_executor_threaded_stub.c")
                .is_file()
        );

        let mut manual = single_thread;
        manual.runtime.executor = ExecutorSelection::Manual;
        compiler.update_config(manual);
        assert_eq!(
            compiler.build(root).expect("manual rebuild"),
            BuildStatus::Rebuilt
        );
        assert!(!root.join("crc/dist/include/cr_executor.h").exists());
        assert!(!root.join("crc/dist/runtime").exists());
        let manifest =
            fs::read_to_string(root.join("crc/dist/crc-artifacts.json")).expect("manifest");
        assert!(!manifest.contains("executor-"));
        assert_eq!(
            compiler.build(root).expect("unchanged manual build"),
            BuildStatus::Unchanged
        );
    }

    #[test]
    fn backend_selection_change_is_part_of_the_project_fingerprint() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let root = directory.path();
        fs::create_dir_all(root.join("crc/src")).expect("source directory");
        fs::create_dir_all(root.join("crc/include")).expect("header directory");
        fs::write(
            root.join("crc/src/main.cr"),
            "int value(void) { return 1; }\n",
        )
        .expect("source");

        let unselected = Config::default();
        let unselected_fingerprint =
            project_fingerprint(root, &unselected).expect("unselected fingerprint");
        let mut selected = unselected;
        selected.runtime.backends = vec![BackendSelection::MemoryConformance];
        let selected_fingerprint =
            project_fingerprint(root, &selected).expect("selected fingerprint");
        assert_ne!(selected_fingerprint, unselected_fingerprint);
    }
}
