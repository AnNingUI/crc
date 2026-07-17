//! CR (Coroutine Runtime) Compiler
//!
//! 将 `.cr` 文件 (C语言 + __async/__await/__yield/__defer) 编译为标准 C 代码 (goto 状态机)

pub mod await_plan;
pub mod backend_abi;
pub mod backend_runtime;
pub mod c_declaration_env;
pub mod c_emitter;
pub mod c_static_plan;
pub mod cli;
pub mod config;
pub mod context_layout;
pub mod control_flow;
pub mod coroutine;
pub mod coroutine_opt;
pub mod executor_runtime;
pub mod header_emitter;
pub mod incremental;
pub mod liveness;
pub mod preprocessor;
pub mod runtime_abi;
pub mod scope_exit;
pub mod semantic;
pub mod slot_liveness;
pub mod symbol_index;
pub mod syntax;
pub mod target_layout;
pub mod template;
pub mod waker_abi;
pub mod watch;

use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::c_emitter::{CBackend, CEmitterConfig};
use crate::coroutine_opt::CfgPassReport;
use crate::syntax::{Diagnostic, DiagnosticSeverity};

/// 编译器主入口
pub struct Compiler {
    config: config::Config,
}

#[derive(Debug, Clone)]
struct PipelineOptions {
    target: config::TargetConfig,
    optimization: config::OptimizationLevel,
}

/// Generated C and verified coroutine-CFG optimization evidence.
#[derive(Debug, Clone)]
pub struct CompilationOutput {
    pub source: String,
    pub optimization: CoroutineOptimizationMetrics,
}

/// Aggregate metrics for the CFG that actually reached coroutine lowering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoroutineOptimizationMetrics {
    pub level: config::OptimizationLevel,
    pub passes: Vec<CfgPassReport>,
    pub input_blocks: usize,
    pub output_blocks: usize,
    pub resume_states: usize,
}

struct LoweredIndexedSource {
    coroutines: coroutine::CoroutineUnit,
    optimization: CoroutineOptimizationMetrics,
}

impl Compiler {
    pub fn new(config: config::Config) -> Self {
        Self { config }
    }

    /// 编译单个 .cr 文件
    pub fn compile_file(&self, input: &Path, output: &Path) -> Result<()> {
        let source = std::fs::read_to_string(input)?;
        let output_code = self.compile_source(&source, input)?;
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(output, output_code)?;
        Ok(())
    }

    /// 编译源代码字符串
    pub fn compile_source(&self, source: &str, file_path: &Path) -> Result<String> {
        Ok(self.compile_source_with_report(source, file_path)?.source)
    }

    /// Compile source and return verified coroutine optimization metrics.
    pub fn compile_source_with_report(
        &self,
        source: &str,
        file_path: &Path,
    ) -> Result<CompilationOutput> {
        let mut parser = syntax::SyntaxParser::new()?;
        let syntax = parser.parse(file_path.to_path_buf(), source)?;
        let symbol_build =
            symbol_index::build_local_async_symbol_index(&syntax, &self.config.codegen.prefix);
        reject_errors(&symbol_build.diagnostics)?;
        let project_path = file_path
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| file_path.to_path_buf());
        self.compile_indexed_source(source, &syntax, &project_path, &symbol_build.index)
    }

    fn compile_indexed_source(
        &self,
        source: &str,
        syntax: &syntax::SyntaxUnit,
        project_path: &Path,
        symbols: &symbol_index::AsyncSymbolIndex,
    ) -> Result<CompilationOutput> {
        let options = self.pipeline_options();
        let mut lowered = self.lower_indexed_source(syntax, project_path, symbols, &options)?;
        let planning = plan_coroutine_storage(&mut lowered.coroutines, project_path, symbols);
        lowered.coroutines.diagnostics.extend(planning.diagnostics);
        let declaration_environment = c_declaration_env::build_c_declaration_environment(syntax);
        let source = self.emit_lowered_source(
            source,
            &lowered.coroutines,
            project_path,
            symbols,
            &declaration_environment,
            &options,
        )?;
        Ok(CompilationOutput {
            source,
            optimization: lowered.optimization,
        })
    }

    fn pipeline_options(&self) -> PipelineOptions {
        PipelineOptions {
            target: self.config.build.target.clone(),
            optimization: self.config.build.optimization,
        }
    }

    fn lower_indexed_source(
        &self,
        syntax: &syntax::SyntaxUnit,
        project_path: &Path,
        symbols: &symbol_index::AsyncSymbolIndex,
        options: &PipelineOptions,
    ) -> Result<LoweredIndexedSource> {
        let hir = semantic::build_hir_with_symbol_index(syntax, symbols, project_path);
        let cfg = control_flow::build_cfg(&hir);
        let cfg = scope_exit::lower_scope_exits(&cfg);
        let input_blocks = cfg
            .functions
            .iter()
            .map(|function| function.blocks.len())
            .sum();
        let optimization = coroutine_opt::optimize_coroutine_cfg(&cfg, options.optimization);
        reject_errors(&optimization.diagnostics)?;
        let Some(cfg) = optimization.unit else {
            anyhow::bail!("verified CFG optimization produced no candidate");
        };
        let output_blocks = cfg
            .functions
            .iter()
            .map(|function| function.blocks.len())
            .sum();
        let mut coroutines = coroutine::lower_coroutines(&cfg, &self.config.codegen.prefix);
        for function in coroutines
            .functions
            .iter_mut()
            .filter(|function| function.cfg.is_async)
        {
            if let Some(resolved) = symbols.resolve(project_path, &function.cfg.name) {
                let stem = &resolved.symbol.public_stem;
                function.task_type = format!("{stem}_task");
                function.init_name = format!("{stem}_init");
                function.poll_name = format!("{stem}_poll");
                function.drop_name = format!("{stem}_drop");
            }
        }
        let resume_states = coroutines
            .functions
            .iter()
            .filter(|function| function.cfg.is_async)
            .map(|function| function.states.len())
            .sum();
        Ok(LoweredIndexedSource {
            coroutines,
            optimization: CoroutineOptimizationMetrics {
                level: options.optimization,
                passes: optimization.reports,
                input_blocks,
                output_blocks,
                resume_states,
            },
        })
    }

    fn emit_lowered_source(
        &self,
        source: &str,
        coroutines: &coroutine::CoroutineUnit,
        project_path: &Path,
        symbols: &symbol_index::AsyncSymbolIndex,
        declaration_environment: &c_declaration_env::CDeclarationEnvironment,
        options: &PipelineOptions,
    ) -> Result<String> {
        let liveness = liveness::analyze_liveness(coroutines);
        let static_plan = c_static_plan::build_c_static_await_plan(
            &liveness,
            project_path,
            symbols,
            declaration_environment,
        );
        reject_errors(&static_plan.diagnostics)?;
        let slot_liveness = slot_liveness::analyze_slot_liveness(coroutines);
        let layout = context_layout::build_context_layout(
            &liveness,
            Some(&static_plan),
            &slot_liveness,
            options.optimization,
            &options.target,
            declaration_environment.has_packing_barrier(),
        );
        reject_errors(&layout.diagnostics)?;
        let layout_verification = context_layout::verify_context_layout(
            &liveness,
            Some(&static_plan),
            &slot_liveness,
            &layout,
        );
        reject_errors(&layout_verification)?;
        let backend = if self.config.codegen.computed_goto {
            CBackend::ComputedGoto
        } else {
            CBackend::Portable
        };
        let emission = c_emitter::emit_translation_unit(
            source,
            &liveness,
            Some(&static_plan),
            &slot_liveness,
            &layout,
            &CEmitterConfig {
                prefix: self.config.codegen.prefix.clone(),
                context_name: self.config.codegen.context_name.clone(),
                backend,
                target: options.target.clone(),
                optimization: options.optimization,
            },
        );
        reject_errors(&emission.diagnostics)?;
        Ok(emission.source)
    }

    /// Compile one CR header into a public C header.
    pub fn compile_header_source(&self, source: &str, file_path: &Path) -> Result<String> {
        let mut parser = syntax::SyntaxParser::new()?;
        let syntax = parser.parse(file_path.to_path_buf(), source)?;
        let emission = header_emitter::emit_header(&syntax, &self.config.codegen.prefix);
        reject_errors(&emission.diagnostics)?;
        Ok(emission.source)
    }

    /// Compile one `.hr` file into a `.h` file.
    pub fn compile_header_file(&self, input: &Path, output: &Path) -> Result<()> {
        let source = std::fs::read_to_string(input)?;
        let output_code = self.compile_header_source(&source, input)?;
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(output, output_code)?;
        Ok(())
    }

    /// 批量编译项目
    pub fn build_project(&self, project_root: &Path) -> Result<()> {
        let dist_dir = resolve_project_path(
            project_root,
            &self.config.project.dist_dir,
            "project.dist_dir",
        )?;
        let artifacts = self.collect_project_artifacts(project_root)?;
        publish_artifacts(&dist_dir, &artifacts)?;

        for artifact in artifacts.iter().filter(|artifact| artifact.input.is_some()) {
            println!(
                "generated: {} -> {}",
                artifact
                    .input
                    .as_deref()
                    .unwrap_or_else(|| Path::new("<compiler>"))
                    .display(),
                dist_dir.join(&artifact.output).display()
            );
        }
        Ok(())
    }

    /// Validate every project input without writing generated artifacts.
    pub fn check_project(&self, project_root: &Path) -> Result<()> {
        self.collect_project_artifacts(project_root).map(|_| ())
    }

    /// Remove only the compiler-owned distribution directory.
    pub fn clean_project(&self, project_root: &Path) -> Result<Option<PathBuf>> {
        let dist_dir = resolve_project_path(
            project_root,
            &self.config.project.dist_dir,
            "project.dist_dir",
        )?;
        if dist_dir.exists() {
            std::fs::remove_dir_all(&dist_dir)?;
            Ok(Some(dist_dir))
        } else {
            Ok(None)
        }
    }

    fn collect_project_artifacts(&self, project_root: &Path) -> Result<Vec<Artifact>> {
        let runtime_selection = self.config.validated_runtime_selection()?;
        let crate_dir = resolve_project_path(
            project_root,
            &self.config.project.crate_dir,
            "project.crate_dir",
        )?;
        if !crate_dir.exists() {
            anyhow::bail!(
                "CR source directory does not exist: {}",
                crate_dir.display()
            );
        }

        let mut artifacts = vec![
            Artifact {
                input: None,
                output: PathBuf::from("include/cr_runtime.h"),
                contents: runtime_abi::runtime_header().to_owned(),
                kind: "runtime",
            },
            Artifact {
                input: None,
                output: PathBuf::from("include/cr_waker.h"),
                contents: waker_abi::waker_header().to_owned(),
                kind: "runtime-extension",
            },
        ];
        let mut runtime_sources = Vec::new();
        let mut build_dependencies = Vec::new();
        let mut build_compile_options = Vec::new();
        let executor_artifacts = match runtime_selection.executor {
            config::ExecutorSelection::Manual => Vec::new(),
            config::ExecutorSelection::SingleThread => {
                executor_runtime::portable_artifacts().to_vec()
            }
            config::ExecutorSelection::NativeThreaded => {
                executor_runtime::native_threaded_artifacts(&self.config.build.target)
            }
        };
        if runtime_selection.executor == config::ExecutorSelection::NativeThreaded
            && target_uses_posix_threads(&self.config.build.target)
        {
            insert_build_dependency(&mut build_dependencies, BuildDependency::PosixThreads);
        }
        if !executor_artifacts.is_empty() {
            for executor_artifact in &executor_artifacts {
                let output = PathBuf::from(executor_artifact.path);
                if executor_artifact.is_source {
                    runtime_sources.push(output.clone());
                }
                artifacts.push(Artifact {
                    input: None,
                    output,
                    contents: executor_artifact.contents.to_owned(),
                    kind: executor_artifact.kind,
                });
            }
        }
        for selection in runtime_selection.backends {
            let mut selected = match selection {
                config::BackendSelection::MemoryConformance => {
                    if target_uses_winsock(&self.config.build.target) {
                        insert_build_compile_option(
                            &mut build_compile_options,
                            BuildCompileOption::MsvcC11Atomics,
                        );
                    }
                    backend_runtime::memory_artifacts()
                }
                config::BackendSelection::NativeNet => {
                    let artifacts =
                        backend_runtime::native_net_artifacts_for_target(&self.config.build.target)
                            .context("validated native-net target has no reference Provider")?;
                    if target_uses_winsock(&self.config.build.target) {
                        insert_build_dependency(&mut build_dependencies, BuildDependency::WinSock);
                    }
                    artifacts
                }
            };
            selected.push(backend_runtime::net_awaitable_artifact());
            for backend_artifact in selected {
                let output = PathBuf::from(backend_artifact.path);
                let is_source = backend_artifact.is_source;
                let inserted = push_unique_artifact(
                    &mut artifacts,
                    Artifact {
                        input: None,
                        output: output.clone(),
                        contents: backend_artifact.contents.to_owned(),
                        kind: backend_artifact.kind,
                    },
                )?;
                if inserted && is_source {
                    runtime_sources.push(output);
                }
            }
        }

        let header_dir = resolve_project_path(
            project_root,
            &self.config.project.include_dir,
            "project.include_dir",
        )?;
        let mut headers = Vec::new();
        if header_dir.exists() {
            headers = walkdir::WalkDir::new(&header_dir)
                .into_iter()
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "hr")
                })
                .map(|entry| entry.into_path())
                .collect();
            headers.sort();
        }

        // 查找所有 .cr 文件
        let mut cr_files: Vec<_> = walkdir::WalkDir::new(&crate_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "cr"))
            .map(|entry| entry.into_path())
            .collect();
        cr_files.sort();

        let mut parser = syntax::SyntaxParser::new()?;
        let mut indexed_units = Vec::with_capacity(headers.len() + cr_files.len());
        for input_path in headers.iter().chain(&cr_files) {
            preprocessor::validate_translation_unit(project_root, input_path, &self.config)?;
            let source = std::fs::read_to_string(input_path)?;
            let unit = parser.parse(input_path.clone(), source)?;
            indexed_units.push((project_relative(project_root, input_path), unit));
        }
        let symbol_inputs: Vec<_> = indexed_units
            .iter()
            .map(|(project_path, unit)| symbol_index::AsyncSymbolInput { project_path, unit })
            .collect();
        let symbol_build =
            symbol_index::build_async_symbol_index(&symbol_inputs, &self.config.codegen.prefix);
        reject_errors(&symbol_build.diagnostics)?;

        for header in &headers {
            let relative = header.strip_prefix(&header_dir)?;
            let source = std::fs::read_to_string(header)?;
            artifacts.push(Artifact {
                input: Some(project_relative(project_root, header)),
                output: PathBuf::from("include").join(relative).with_extension("h"),
                contents: self.compile_header_source(&source, header)?,
                kind: "header",
            });
        }

        let mut source_units = Vec::with_capacity(cr_files.len());
        let options = self.pipeline_options();
        for input_path in &cr_files {
            let relative = input_path.strip_prefix(&crate_dir)?;
            let output_path = relative.with_extension("c");
            let source = std::fs::read_to_string(input_path)?;
            let mut parser = syntax::SyntaxParser::new()?;
            let syntax = parser.parse(input_path.clone(), source.as_str())?;
            let project_path = project_relative(project_root, input_path);
            let coroutines = self
                .lower_indexed_source(&syntax, &project_path, &symbol_build.index, &options)?
                .coroutines;
            let declaration_environment =
                c_declaration_env::build_c_declaration_environment(&syntax);
            source_units.push(ProjectSourceUnit {
                project_path,
                output_path,
                source,
                coroutines,
                declaration_environment,
            });
        }

        let mut planning_functions = Vec::new();
        for source_unit in &mut source_units {
            let translation_unit =
                symbol_index::TranslationUnitId(path_for_manifest(&source_unit.project_path));
            for function in source_unit
                .coroutines
                .functions
                .iter_mut()
                .filter(|function| function.cfg.is_async)
            {
                let caller = symbol_build
                    .index
                    .resolve(&source_unit.project_path, &function.cfg.name)
                    .expect("lowered async function must exist in the project symbol index")
                    .symbol
                    .id;
                planning_functions.push(await_plan::AwaitStorageFunction {
                    caller,
                    translation_unit: translation_unit.clone(),
                    source_start: function.cfg.span.start_byte,
                    plan: &mut function.await_plan,
                });
            }
        }
        let planning = await_plan::plan_await_storage(&mut planning_functions, &symbol_build.index);
        reject_errors(&planning.diagnostics)?;
        drop(planning_functions);

        let mut generated_sources = Vec::with_capacity(source_units.len() + runtime_sources.len());
        for source_unit in source_units {
            let contents = self.emit_lowered_source(
                &source_unit.source,
                &source_unit.coroutines,
                &source_unit.project_path,
                &symbol_build.index,
                &source_unit.declaration_environment,
                &options,
            )?;
            artifacts.push(Artifact {
                input: Some(source_unit.project_path),
                output: source_unit.output_path.clone(),
                contents,
                kind: "source",
            });
            generated_sources.push(source_unit.output_path);
        }
        generated_sources.extend(runtime_sources);
        artifacts.push(Artifact {
            input: None,
            output: PathBuf::from("meson.build"),
            contents: meson_source_manifest(
                &generated_sources,
                &build_dependencies,
                &build_compile_options,
            ),
            kind: "build-manifest",
        });
        if let Some(contents) =
            cmake_dependency_manifest(&build_dependencies, &build_compile_options)
        {
            artifacts.push(Artifact {
                input: None,
                output: PathBuf::from("crc-generated-dependencies.cmake"),
                contents,
                kind: "build-manifest",
            });
        }

        let manifest = ArtifactManifest {
            compiler_version: env!("CARGO_PKG_VERSION"),
            runtime_abi_version: runtime_abi::CR_RUNTIME_ABI_VERSION,
            dependencies: build_dependencies
                .iter()
                .map(|dependency| dependency.as_str())
                .collect(),
            compile_options: build_compile_options
                .iter()
                .map(|option| option.as_str())
                .collect(),
            artifacts: artifacts
                .iter()
                .map(|artifact| ArtifactRecord {
                    input: artifact.input.as_deref().map(path_for_manifest),
                    output: path_for_manifest(&artifact.output),
                    kind: artifact.kind,
                })
                .collect(),
        };
        artifacts.push(Artifact {
            input: None,
            output: PathBuf::from("crc-artifacts.json"),
            contents: serde_json::to_string_pretty(&manifest)? + "\n",
            kind: "artifact-manifest",
        });

        Ok(artifacts)
    }
}

struct Artifact {
    input: Option<PathBuf>,
    output: PathBuf,
    contents: String,
    kind: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BuildDependency {
    PosixThreads,
    WinSock,
}

impl BuildDependency {
    const fn as_str(self) -> &'static str {
        match self {
            Self::PosixThreads => "posix-threads",
            Self::WinSock => "winsock",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BuildCompileOption {
    MsvcC11Atomics,
}

impl BuildCompileOption {
    const fn as_str(self) -> &'static str {
        match self {
            Self::MsvcC11Atomics => "msvc-c11-atomics",
        }
    }
}

struct ProjectSourceUnit {
    project_path: PathBuf,
    output_path: PathBuf,
    source: String,
    coroutines: coroutine::CoroutineUnit,
    declaration_environment: c_declaration_env::CDeclarationEnvironment,
}

#[derive(Serialize)]
struct ArtifactManifest<'a> {
    compiler_version: &'a str,
    runtime_abi_version: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    dependencies: Vec<&'static str>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    compile_options: Vec<&'static str>,
    artifacts: Vec<ArtifactRecord>,
}

#[derive(Serialize)]
struct ArtifactRecord {
    input: Option<String>,
    output: String,
    kind: &'static str,
}

fn resolve_project_path(project_root: &Path, configured: &Path, name: &str) -> Result<PathBuf> {
    if configured.is_absolute()
        || configured.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!(
            "{name} must stay within the project root: {}",
            configured.display()
        );
    }
    Ok(project_root.join(configured))
}

fn project_relative(project_root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(project_root)
        .unwrap_or(path)
        .to_path_buf()
}

fn path_for_manifest(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn push_unique_artifact(artifacts: &mut Vec<Artifact>, candidate: Artifact) -> Result<bool> {
    if let Some(existing) = artifacts
        .iter()
        .find(|artifact| artifact.output == candidate.output)
    {
        if existing.input == candidate.input
            && existing.contents == candidate.contents
            && existing.kind == candidate.kind
        {
            return Ok(false);
        }
        anyhow::bail!(
            "conflicting generated artifact `{}`",
            path_for_manifest(&candidate.output)
        );
    }
    artifacts.push(candidate);
    Ok(true)
}

fn insert_build_dependency(dependencies: &mut Vec<BuildDependency>, dependency: BuildDependency) {
    if !dependencies.contains(&dependency) {
        dependencies.push(dependency);
        dependencies.sort_unstable();
    }
}

fn insert_build_compile_option(options: &mut Vec<BuildCompileOption>, option: BuildCompileOption) {
    if !options.contains(&option) {
        options.push(option);
        options.sort_unstable();
    }
}

fn target_uses_posix_threads(target: &config::TargetConfig) -> bool {
    matches!(
        target,
        config::TargetConfig::LinuxGnu
            | config::TargetConfig::LinuxMusl
            | config::TargetConfig::Macos
    ) || matches!(target, config::TargetConfig::Host) && cfg!(unix)
}

fn target_uses_winsock(target: &config::TargetConfig) -> bool {
    matches!(
        target,
        config::TargetConfig::WindowsMsvc | config::TargetConfig::WindowsGnu
    ) || matches!(target, config::TargetConfig::Host) && cfg!(windows)
}

fn publish_artifacts(dist_dir: &Path, artifacts: &[Artifact]) -> Result<()> {
    let parent = dist_dir
        .parent()
        .context("the configured dist directory has no parent")?;
    std::fs::create_dir_all(parent)?;
    let name = dist_dir
        .file_name()
        .and_then(|name| name.to_str())
        .context("the configured dist directory has no UTF-8 name")?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = parent.join(format!(
        ".{name}.crc-staging-{}-{nonce}",
        std::process::id()
    ));
    let backup = parent.join(format!(".{name}.crc-backup-{}-{nonce}", std::process::id()));

    std::fs::create_dir(&staging)?;
    let write_result = (|| -> Result<()> {
        for artifact in artifacts {
            let output = staging.join(&artifact.output);
            if let Some(output_parent) = output.parent() {
                std::fs::create_dir_all(output_parent)?;
            }
            std::fs::write(output, &artifact.contents)?;
        }
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }

    let had_previous = dist_dir.exists();
    if had_previous {
        std::fs::rename(dist_dir, &backup).with_context(|| {
            format!(
                "failed to preserve previous artifacts at {}",
                backup.display()
            )
        })?;
    }
    if let Err(error) = std::fs::rename(&staging, dist_dir) {
        if had_previous {
            let _ = std::fs::rename(&backup, dist_dir);
        }
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error).context("failed to publish the complete artifact set");
    }
    if had_previous {
        std::fs::remove_dir_all(&backup)
            .with_context(|| format!("failed to remove artifact backup {}", backup.display()))?;
    }
    Ok(())
}

fn cmake_dependency_manifest(
    dependencies: &[BuildDependency],
    compile_options: &[BuildCompileOption],
) -> Option<String> {
    if dependencies.is_empty() && compile_options.is_empty() {
        return None;
    }
    let mut manifest = String::from("set(CR_GENERATED_DEPENDENCIES)\n");
    for dependency in dependencies {
        match dependency {
            BuildDependency::PosixThreads => manifest.push_str(
                "find_package(Threads REQUIRED)\n\
                 list(APPEND CR_GENERATED_DEPENDENCIES Threads::Threads)\n",
            ),
            BuildDependency::WinSock => {
                manifest.push_str("list(APPEND CR_GENERATED_DEPENDENCIES ws2_32)\n")
            }
        }
    }
    if compile_options.contains(&BuildCompileOption::MsvcC11Atomics) {
        manifest.push_str(
            "if(MSVC)\n\
             target_compile_options(${PROJECT_NAME} PRIVATE /experimental:c11atomics)\n\
             endif()\n",
        );
    }
    Some(manifest)
}

fn meson_source_manifest(
    sources: &[std::path::PathBuf],
    dependencies: &[BuildDependency],
    compile_options: &[BuildCompileOption],
) -> String {
    let mut manifest = if sources.is_empty() {
        "cr_generated_sources = []\n".to_owned()
    } else {
        let mut sources_manifest = String::from("cr_generated_sources = files(\n");
        for source in sources {
            let path = source
                .to_string_lossy()
                .replace('\\', "/")
                .replace('\'', "\\'");
            sources_manifest.push_str("  '");
            sources_manifest.push_str(&path);
            sources_manifest.push_str("',\n");
        }
        sources_manifest.push_str(")\n");
        sources_manifest
    };
    if dependencies.contains(&BuildDependency::WinSock) || !compile_options.is_empty() {
        manifest.push_str("cr_generated_c_compiler = meson.get_compiler('c')\n");
    }
    if dependencies.is_empty() {
        manifest.push_str("cr_generated_dependencies = []\n");
    } else {
        manifest.push_str("cr_generated_dependencies = [\n");
        for dependency in dependencies {
            match dependency {
                BuildDependency::PosixThreads => {
                    manifest.push_str("  dependency('threads'),\n");
                }
                BuildDependency::WinSock => manifest.push_str(
                    "  cr_generated_c_compiler.find_library('ws2_32', required: true),\n",
                ),
            }
        }
        manifest.push_str("]\n");
    }
    if compile_options.contains(&BuildCompileOption::MsvcC11Atomics) {
        manifest.push_str(
            "cr_generated_compile_args = []\n\
             if cr_generated_c_compiler.get_id() == 'msvc'\n\
               cr_generated_compile_args += ['/experimental:c11atomics']\n\
             endif\n",
        );
    }
    manifest
}

fn plan_coroutine_storage(
    unit: &mut coroutine::CoroutineUnit,
    project_path: &Path,
    symbols: &symbol_index::AsyncSymbolIndex,
) -> await_plan::AwaitStoragePlanResult {
    let translation_unit = symbol_index::TranslationUnitId(path_for_manifest(project_path));
    let mut functions: Vec<_> = unit
        .functions
        .iter_mut()
        .filter(|function| function.cfg.is_async)
        .map(|function| {
            let caller = symbols
                .resolve(project_path, &function.cfg.name)
                .expect("lowered async function must exist in the symbol index")
                .symbol
                .id;
            await_plan::AwaitStorageFunction {
                caller,
                translation_unit: translation_unit.clone(),
                source_start: function.cfg.span.start_byte,
                plan: &mut function.await_plan,
            }
        })
        .collect();
    await_plan::plan_await_storage(&mut functions, symbols)
}

fn reject_errors(diagnostics: &[Diagnostic]) -> Result<()> {
    let errors: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
        .map(|diagnostic| {
            format!(
                "{}:{}:{} [{}] {}",
                diagnostic.primary_span.path.display(),
                diagnostic.primary_span.start.row + 1,
                diagnostic.primary_span.start.column + 1,
                diagnostic.code,
                diagnostic.message
            )
        })
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(errors.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::path::PathBuf;

    use crate::config::{
        BackendSelection, Config, ExecutorSelection, OptimizationLevel, TargetConfig,
    };

    use super::{
        BuildCompileOption, BuildDependency, Compiler, cmake_dependency_manifest,
        meson_source_manifest,
    };

    #[test]
    fn compiler_pipeline_retains_target_and_optimization_selection() {
        let mut config = Config::default();
        config.build.target = TargetConfig::Wasm32Wasi;
        config.build.optimization = OptimizationLevel::Size;
        let compiler = Compiler::new(config);

        let options = compiler.pipeline_options();
        assert!(matches!(options.target, TargetConfig::Wasm32Wasi));
        assert!(matches!(options.optimization, OptimizationLevel::Size));
    }

    #[test]
    fn executor_selection_does_not_change_single_source_output() {
        let source = "__async int value(void) { return 7; }\n";
        let compile = |executor| {
            let mut config = Config::default();
            config.runtime.executor = executor;
            Compiler::new(config)
                .compile_source(source, Path::new("selection.cr"))
                .expect("selection fixture compiles")
        };
        let manual = compile(ExecutorSelection::Manual);
        assert_eq!(compile(ExecutorSelection::SingleThread), manual);
        assert_eq!(compile(ExecutorSelection::NativeThreaded), manual);
    }

    #[test]
    fn backend_selection_does_not_change_single_source_output() {
        let source = "__async int value(void) { return 7; }\n";
        let compile = |backends| {
            let mut config = Config::default();
            config.runtime.backends = backends;
            Compiler::new(config)
                .compile_source(source, Path::new("backend-selection.cr"))
                .expect("selection fixture compiles")
        };
        let unselected = compile(Vec::new());
        assert_eq!(
            compile(vec![BackendSelection::MemoryConformance]),
            unselected
        );
        assert_eq!(compile(vec![BackendSelection::NativeNet]), unselected);
    }

    #[test]
    fn meson_manifest_is_deterministic_and_uses_portable_separators() {
        let manifest = meson_source_manifest(
            &[PathBuf::from("nested\\worker.c"), PathBuf::from("main.c")],
            &[],
            &[],
        );
        assert_eq!(
            manifest,
            "cr_generated_sources = files(\n  'nested/worker.c',\n  'main.c',\n)\ncr_generated_dependencies = []\n"
        );
    }

    #[test]
    fn meson_manifest_records_only_the_posix_thread_dependency() {
        let manifest = meson_source_manifest(
            &[PathBuf::from("runtime/worker.c")],
            &[BuildDependency::PosixThreads],
            &[],
        );
        assert_eq!(
            manifest,
            "cr_generated_sources = files(\n  'runtime/worker.c',\n)\ncr_generated_dependencies = [\n  dependency('threads'),\n]\n"
        );
    }

    #[test]
    fn build_manifests_render_winsock_from_the_explicit_dependency_plan() {
        let dependencies = [BuildDependency::WinSock];
        assert_eq!(
            cmake_dependency_manifest(&dependencies, &[]).as_deref(),
            Some(
                "set(CR_GENERATED_DEPENDENCIES)\n\
                 list(APPEND CR_GENERATED_DEPENDENCIES ws2_32)\n"
            )
        );
        assert_eq!(
            meson_source_manifest(&[], &dependencies, &[]),
            concat!(
                "cr_generated_sources = []\n",
                "cr_generated_c_compiler = meson.get_compiler('c')\n",
                "cr_generated_dependencies = [\n",
                "  cr_generated_c_compiler.find_library('ws2_32', required: true),\n",
                "]\n",
            )
        );
    }

    #[test]
    fn build_manifests_enable_c11_atomics_only_for_msvc_memory_projects() {
        let options = [BuildCompileOption::MsvcC11Atomics];
        let cmake = cmake_dependency_manifest(&[], &options).expect("CMake options");
        assert!(cmake.contains("if(MSVC)"));
        assert!(cmake.contains("/experimental:c11atomics"));
        let meson = meson_source_manifest(&[], &[], &options);
        assert!(meson.contains("get_id() == 'msvc'"));
        assert!(meson.contains("/experimental:c11atomics"));
    }
}
