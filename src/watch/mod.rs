//! File watching and transactional development rebuilds.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

use anyhow::Result;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher as NotifyWatcher};

use crate::config::Config;
use crate::incremental::{BuildStatus, IncrementalCompiler};

pub struct FileWatcher {
    config: Config,
    watcher: Option<RecommendedWatcher>,
    debounce_ms: u64,
}

impl FileWatcher {
    pub fn new(config: Config) -> Self {
        let debounce_ms = config.watch.debounce_ms;
        Self {
            config,
            watcher: None,
            debounce_ms,
        }
    }

    pub fn start(&mut self, project_root: PathBuf) -> Result<()> {
        let mut incremental = IncrementalCompiler::new(self.config.clone());
        incremental.build(&project_root)?;

        let (tx, rx) = channel();
        let mut watcher = notify::recommended_watcher(tx)?;
        watch_if_present(
            &mut watcher,
            &project_root.join(&self.config.project.crate_dir),
            RecursiveMode::Recursive,
        )?;
        watch_if_present(
            &mut watcher,
            &project_root.join(&self.config.project.include_dir),
            RecursiveMode::Recursive,
        )?;
        watch_if_present(
            &mut watcher,
            &project_root.join(&self.config.project.src_dir),
            RecursiveMode::Recursive,
        )?;
        watch_if_present(
            &mut watcher,
            &project_root.join("include"),
            RecursiveMode::Recursive,
        )?;
        for include in &self.config.preprocessor.include_paths {
            if include == &self.config.project.include_dir || include == Path::new("include") {
                continue;
            }
            watch_if_present(
                &mut watcher,
                &project_root.join(include),
                RecursiveMode::Recursive,
            )?;
        }
        for include in &self.config.preprocessor.forced_includes {
            watch_if_present(
                &mut watcher,
                &project_root.join(include),
                RecursiveMode::NonRecursive,
            )?;
        }
        watch_if_present(
            &mut watcher,
            &project_root.join("crc.toml"),
            RecursiveMode::NonRecursive,
        )?;

        self.watcher = Some(watcher);
        run_event_loop(
            rx,
            project_root,
            self.config.clone(),
            self.debounce_ms,
            incremental,
        );
        Ok(())
    }

    pub fn stop(&mut self) {
        self.watcher = None;
    }
}

fn watch_if_present(
    watcher: &mut RecommendedWatcher,
    path: &Path,
    mode: RecursiveMode,
) -> Result<()> {
    if path.exists() {
        watcher.watch(path, mode)?;
    }
    Ok(())
}

fn is_relevant_event(event: &Event) -> bool {
    matches!(
        event.kind,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
    )
}

fn should_watch(path: &Path, config: &Config) -> bool {
    let normalized = path.to_string_lossy().replace('\\', "/");
    if config.watch.ignore_patterns.iter().any(|pattern| {
        let component = pattern.trim_start_matches("**/").trim_end_matches("/**");
        !component.is_empty()
            && normalized
                .split('/')
                .any(|path_component| path_component == component)
    }) {
        return false;
    }
    path.file_name().is_some_and(|name| name == "crc.toml")
        || path
            .extension()
            .is_some_and(|extension| matches!(extension.to_str(), Some("cr" | "hr" | "h")))
}

fn process_changes(
    changes: &mut HashSet<PathBuf>,
    config: &mut Config,
    compiler: &mut IncrementalCompiler,
    project_root: &Path,
) {
    if changes.is_empty() {
        return;
    }
    let mut changed: Vec<_> = changes.drain().collect();
    changed.sort();
    println!("detected {} changed file(s)", changed.len());
    for path in &changed {
        println!("  {}", path.display());
    }

    if changed
        .iter()
        .any(|path| path.file_name().is_some_and(|name| name == "crc.toml"))
    {
        match Config::load_from_file(&project_root.join("crc.toml")) {
            Ok(updated) => {
                *config = updated.clone();
                compiler.update_config(updated);
            }
            Err(error) => {
                eprintln!(
                    "development rebuild failed; previous artifacts remain active:\n{error:#}"
                );
                return;
            }
        }
    }

    match compiler.build(project_root) {
        Ok(BuildStatus::Rebuilt) => println!("development rebuild completed"),
        Ok(BuildStatus::Unchanged) => println!("no content changes; rebuild skipped"),
        Err(error) => {
            eprintln!("development rebuild failed; previous artifacts remain active:\n{error:#}")
        }
    }
}

fn run_event_loop(
    rx: std::sync::mpsc::Receiver<notify::Result<Event>>,
    project_root: PathBuf,
    mut config: Config,
    debounce_ms: u64,
    mut compiler: IncrementalCompiler,
) {
    std::thread::spawn(move || {
        let debounce = Duration::from_millis(debounce_ms.max(1));
        let mut changes = HashSet::new();
        loop {
            match rx.recv_timeout(debounce) {
                Ok(Ok(event)) if is_relevant_event(&event) => {
                    changes.extend(
                        event
                            .paths
                            .into_iter()
                            .filter(|path| should_watch(path, &config)),
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => eprintln!("watch error: {error}"),
                Err(RecvTimeoutError::Timeout) => {
                    process_changes(&mut changes, &mut config, &mut compiler, &project_root);
                }
                Err(RecvTimeoutError::Disconnected) => {
                    process_changes(&mut changes, &mut config, &mut compiler, &project_root);
                    break;
                }
            }
        }
    });
}

pub type Watcher = FileWatcher;

pub fn new_watcher(config: Config) -> Result<FileWatcher> {
    Ok(FileWatcher::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watches_sources_headers_and_configuration_cross_platform() {
        let config = Config::default();
        assert!(should_watch(Path::new("crc/src/main.cr"), &config));
        assert!(should_watch(Path::new("crc/include/api.hr"), &config));
        assert!(should_watch(Path::new("include/api.h"), &config));
        assert!(should_watch(Path::new("crc.toml"), &config));
        assert!(!should_watch(Path::new("target/generated.cr"), &config));
        assert!(!should_watch(Path::new("crc/dist/main.c"), &config));
    }
}
