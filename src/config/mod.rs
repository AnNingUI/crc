//! 配置系统

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub project: ProjectConfig,
    pub build: BuildConfig,
    #[serde(default)]
    pub runtime: RuntimeConfig,
    #[serde(default)]
    pub preprocessor: PreprocessorConfig,
    pub watch: WatchConfig,
    pub codegen: CodegenConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub name: String,
    pub version: String,
    pub crate_dir: PathBuf,   // crc 源码目录
    pub dist_dir: PathBuf,    // 编译输出目录
    pub include_dir: PathBuf, // C 头文件目录
    pub src_dir: PathBuf,     // 主 C 源文件目录
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BuildConfig {
    pub target: TargetConfig,
    pub optimization: OptimizationLevel,
    pub debug_info: bool,
    pub warnings_as_errors: bool,
    pub c_standard: CStandard,
    pub crt_runtime_path: Option<PathBuf>, // CR 运行时库路径
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RuntimeConfig {
    pub executor: ExecutorSelection,
    pub backends: Vec<BackendSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutorSelection {
    #[default]
    Manual,
    SingleThread,
    NativeThreaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendSelection {
    MemoryConformance,
    NativeNet,
}

impl BackendSelection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MemoryConformance => "memory-conformance",
            Self::NativeNet => "native-net",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedRuntimeSelection {
    pub executor: ExecutorSelection,
    pub backends: Vec<BackendSelection>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PreprocessorConfig {
    pub enabled: bool,
    pub command: Option<PathBuf>,
    pub include_paths: Vec<PathBuf>,
    pub defines: Vec<String>,
    pub forced_includes: Vec<PathBuf>,
}

impl Default for PreprocessorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            command: None,
            include_paths: vec![PathBuf::from("crc/include"), PathBuf::from("include")],
            defines: Vec::new(),
            forced_includes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TargetConfig {
    #[default]
    Host,
    WindowsMsvc,
    WindowsGnu,
    LinuxGnu,
    LinuxMusl,
    Macos,
    #[serde(rename = "wasm32-wasi")]
    Wasm32Wasi,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum OptimizationLevel {
    #[default]
    None,
    Size,
    Speed,
    Aggressive,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CStandard {
    C89,
    C99,
    #[default]
    C11,
    C17,
    C23,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WatchConfig {
    pub enabled: bool,
    pub debounce_ms: u64,
    pub ignore_patterns: Vec<String>,
    pub parallel_jobs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CodegenConfig {
    pub prefix: String,
    pub context_name: String,
    pub computed_goto: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            project: ProjectConfig {
                name: "my-cr-project".into(),
                version: "0.1.0".into(),
                crate_dir: PathBuf::from("crc/src"),
                dist_dir: PathBuf::from("crc/dist"),
                include_dir: PathBuf::from("crc/include"),
                src_dir: PathBuf::from("src"),
            },
            build: BuildConfig {
                target: TargetConfig::Host,
                optimization: OptimizationLevel::Speed,
                debug_info: true,
                warnings_as_errors: false,
                c_standard: CStandard::C11,
                crt_runtime_path: None,
            },
            runtime: RuntimeConfig::default(),
            preprocessor: PreprocessorConfig::default(),
            watch: WatchConfig {
                enabled: true,
                debounce_ms: 50,
                ignore_patterns: vec!["**/target/**".into(), "**/.git/**".into()],
                parallel_jobs: num_cpus::get(),
            },
            codegen: CodegenConfig {
                prefix: "cr_".into(),
                context_name: "ctx".into(),
                computed_goto: false,
            },
        }
    }
}

impl Config {
    pub fn load_from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    pub fn save_to_file(&self, path: &Path) -> anyhow::Result<()> {
        let content = toml::to_string_pretty(self)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    pub fn validate_runtime_target(&self) -> anyhow::Result<()> {
        self.validated_runtime_selection().map(|_| ())
    }

    pub(crate) fn validated_runtime_selection(&self) -> anyhow::Result<ValidatedRuntimeSelection> {
        if self.runtime.executor == ExecutorSelection::NativeThreaded {
            match &self.build.target {
                TargetConfig::Wasm32Wasi => {
                    anyhow::bail!(
                        "runtime executor `native-threaded` is unsupported for target `wasm32-wasi`"
                    );
                }
                TargetConfig::Custom(target) => {
                    anyhow::bail!(
                        "runtime executor `native-threaded` is unsupported for custom target `{target}`"
                    );
                }
                TargetConfig::Host
                | TargetConfig::WindowsMsvc
                | TargetConfig::WindowsGnu
                | TargetConfig::LinuxGnu
                | TargetConfig::LinuxMusl
                | TargetConfig::Macos => {}
            }
        }

        let mut unique = HashSet::with_capacity(self.runtime.backends.len());
        for backend in &self.runtime.backends {
            if !unique.insert(*backend) {
                anyhow::bail!("duplicate runtime backend selection `{}`", backend.as_str());
            }
            if *backend == BackendSelection::NativeNet {
                match &self.build.target {
                    TargetConfig::Wasm32Wasi => {
                        anyhow::bail!(
                            "runtime backend `native-net` is unsupported for target `wasm32-wasi`"
                        );
                    }
                    TargetConfig::Custom(target) => {
                        anyhow::bail!(
                            "runtime backend `native-net` is unsupported for custom target `{target}`"
                        );
                    }
                    TargetConfig::Host
                    | TargetConfig::WindowsMsvc
                    | TargetConfig::WindowsGnu
                    | TargetConfig::LinuxGnu
                    | TargetConfig::LinuxMusl
                    | TargetConfig::Macos => {}
                }
            }
        }

        Ok(ValidatedRuntimeSelection {
            executor: self.runtime.executor,
            backends: self.runtime.backends.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{BackendSelection, Config, ExecutorSelection, OptimizationLevel, TargetConfig};
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    struct Selection {
        target: TargetConfig,
        optimization: OptimizationLevel,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct RuntimeSelection {
        executor: ExecutorSelection,
        #[serde(default)]
        backends: Vec<BackendSelection>,
    }

    #[test]
    fn optimization_levels_round_trip_with_stable_names() {
        let cases = [
            (OptimizationLevel::None, "none"),
            (OptimizationLevel::Speed, "speed"),
            (OptimizationLevel::Size, "size"),
            (OptimizationLevel::Aggressive, "aggressive"),
        ];

        for (optimization, expected) in cases {
            let serialized = toml::to_string(&Selection {
                target: TargetConfig::Host,
                optimization,
            })
            .expect("selection serializes");
            assert!(
                serialized.contains(&format!("optimization = \"{expected}\"")),
                "{serialized}"
            );
            let restored: Selection = toml::from_str(&serialized).expect("selection parses");
            assert_eq!(
                std::mem::discriminant(&restored.optimization),
                std::mem::discriminant(&optimization)
            );
        }
    }

    #[test]
    fn wasm32_wasi_target_round_trips_with_the_public_config_name() {
        let serialized = toml::to_string(&Selection {
            target: TargetConfig::Wasm32Wasi,
            optimization: OptimizationLevel::Speed,
        })
        .expect("selection serializes");
        assert!(serialized.contains("target = \"wasm32-wasi\""));

        let restored: Selection = toml::from_str(&serialized).expect("selection parses");
        assert!(matches!(restored.target, TargetConfig::Wasm32Wasi));
    }

    #[test]
    fn executor_selections_round_trip_with_stable_names() {
        let cases = [
            (ExecutorSelection::Manual, "manual"),
            (ExecutorSelection::SingleThread, "single-thread"),
            (ExecutorSelection::NativeThreaded, "native-threaded"),
        ];
        for (executor, expected) in cases {
            let serialized = toml::to_string(&RuntimeSelection {
                executor,
                backends: Vec::new(),
            })
            .expect("selection serializes");
            assert!(serialized.contains(&format!("executor = \"{expected}\"")));
            let restored: RuntimeSelection = toml::from_str(&serialized).expect("selection parses");
            assert_eq!(restored.executor, executor);
        }
    }

    #[test]
    fn missing_runtime_section_defaults_to_manual() {
        let mut value = toml::Value::try_from(Config::default()).expect("config becomes TOML");
        value
            .as_table_mut()
            .expect("config is a table")
            .remove("runtime");
        let restored: Config = value.try_into().expect("legacy config parses");
        assert_eq!(restored.runtime.executor, ExecutorSelection::Manual);
        assert!(restored.runtime.backends.is_empty());

        let runtime: super::RuntimeConfig =
            toml::from_str("").expect("empty runtime section defaults");
        assert_eq!(runtime.executor, ExecutorSelection::Manual);
        assert!(runtime.backends.is_empty());
    }

    #[test]
    fn backend_selections_round_trip_in_declared_order_with_stable_names() {
        let selection = RuntimeSelection {
            executor: ExecutorSelection::Manual,
            backends: vec![
                BackendSelection::NativeNet,
                BackendSelection::MemoryConformance,
            ],
        };
        let serialized = toml::to_string(&selection).expect("selection serializes");
        assert!(
            serialized.contains("backends = [\"native-net\", \"memory-conformance\"]"),
            "{serialized}"
        );
        let restored: RuntimeSelection = toml::from_str(&serialized).expect("selection parses");
        assert_eq!(restored.backends, selection.backends);
    }

    #[test]
    fn backend_target_validation_preserves_portable_and_native_boundaries() {
        for target in [
            TargetConfig::Host,
            TargetConfig::WindowsMsvc,
            TargetConfig::WindowsGnu,
            TargetConfig::LinuxGnu,
            TargetConfig::LinuxMusl,
            TargetConfig::Macos,
            TargetConfig::Wasm32Wasi,
            TargetConfig::Custom("portable-vendor".to_owned()),
        ] {
            let mut config = Config::default();
            config.build.target = target;
            config.runtime.backends = vec![BackendSelection::MemoryConformance];
            assert!(config.validate_runtime_target().is_ok());
        }

        for target in [
            TargetConfig::Host,
            TargetConfig::WindowsMsvc,
            TargetConfig::WindowsGnu,
            TargetConfig::LinuxGnu,
            TargetConfig::LinuxMusl,
            TargetConfig::Macos,
        ] {
            let mut config = Config::default();
            config.build.target = target;
            config.runtime.backends = vec![BackendSelection::NativeNet];
            assert!(config.validate_runtime_target().is_ok());
        }

        for target in [
            TargetConfig::Wasm32Wasi,
            TargetConfig::Custom("unknown-vendor".to_owned()),
        ] {
            let mut config = Config::default();
            config.build.target = target;
            config.runtime.backends = vec![BackendSelection::NativeNet];
            assert!(config.validate_runtime_target().is_err());
        }
    }

    #[test]
    fn duplicate_backend_selections_are_rejected_before_planning() {
        for backend in [
            BackendSelection::MemoryConformance,
            BackendSelection::NativeNet,
        ] {
            let mut config = Config::default();
            config.runtime.backends = vec![backend, backend];
            let error = config
                .validate_runtime_target()
                .expect_err("duplicate selection must fail");
            let message = error.to_string();
            assert!(message.contains("duplicate"), "{message}");
            assert!(message.contains(backend.as_str()), "{message}");
        }
    }

    #[test]
    fn executor_target_validation_preserves_portable_and_native_boundaries() {
        for target in [
            TargetConfig::Host,
            TargetConfig::WindowsMsvc,
            TargetConfig::WindowsGnu,
            TargetConfig::LinuxGnu,
            TargetConfig::LinuxMusl,
            TargetConfig::Macos,
            TargetConfig::Wasm32Wasi,
            TargetConfig::Custom("portable-vendor".to_owned()),
        ] {
            for executor in [ExecutorSelection::Manual, ExecutorSelection::SingleThread] {
                let mut config = Config::default();
                config.build.target = target.clone();
                config.runtime.executor = executor;
                assert!(config.validate_runtime_target().is_ok(), "{target:?}");
            }
        }

        for target in [
            TargetConfig::Host,
            TargetConfig::WindowsMsvc,
            TargetConfig::WindowsGnu,
            TargetConfig::LinuxGnu,
            TargetConfig::LinuxMusl,
            TargetConfig::Macos,
        ] {
            let mut config = Config::default();
            config.build.target = target;
            config.runtime.executor = ExecutorSelection::NativeThreaded;
            assert!(config.validate_runtime_target().is_ok());
        }

        for target in [
            TargetConfig::Wasm32Wasi,
            TargetConfig::Custom("unknown-vendor".to_owned()),
        ] {
            let mut config = Config::default();
            config.build.target = target;
            config.runtime.executor = ExecutorSelection::NativeThreaded;
            assert!(config.validate_runtime_target().is_err());
        }
    }
}
