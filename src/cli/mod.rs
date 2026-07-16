//! CLI command dispatch.

use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::template::TemplateEngine;

#[derive(Parser)]
#[command(
    name = "crc",
    version,
    about = "CR source-to-source compiler for async, await, yield, and defer"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Configuration file path.
    #[arg(short, long, default_value = "crc.toml")]
    pub config: PathBuf,

    /// Project root directory.
    #[arg(short, long, default_value = ".")]
    pub root: PathBuf,

    /// Enable verbose logging.
    #[arg(short, long)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new CR project.
    Create {
        /// Project name.
        name: String,

        /// Target directory relative to the project root.
        #[arg(short, long)]
        dir: Option<PathBuf>,
    },

    /// Generate project C artifacts.
    Build {
        /// Use release build settings when native build integration is enabled.
        #[arg(short, long)]
        release: bool,
    },

    /// Start incremental development mode.
    Dev {
        /// Reserved development server port.
        #[arg(short, long)]
        port: Option<u16>,
    },

    /// Validate project sources without a native build.
    Check,

    /// Remove generated artifacts.
    Clean,

    /// Print the resolved configuration.
    Config,

    /// Write a default configuration file.
    Init,
}

pub fn run(cli: Cli) -> Result<()> {
    let start_root = cli.root.canonicalize().unwrap_or(cli.root.clone());
    if let Commands::Create { name, dir } = &cli.command {
        return create_project(&start_root, name, dir.as_deref());
    }

    let (project_root, config_path) = resolve_project(&start_root, &cli.config);
    let config = if config_path.exists() {
        Config::load_from_file(&config_path)?
    } else {
        Config::default()
    };

    match cli.command {
        Commands::Create { .. } => unreachable!(),
        Commands::Build { release } => {
            build_project(&config, &project_root, release)?;
        }
        Commands::Dev { port } => {
            dev_mode(&config, &project_root, port)?;
        }
        Commands::Check => {
            check_project(&config, &project_root)?;
        }
        Commands::Clean => {
            clean_project(&config, &project_root)?;
        }
        Commands::Config => {
            println!("{}", toml::to_string_pretty(&config)?);
        }
        Commands::Init => {
            Config::default().save_to_file(&config_path)?;
            println!("created configuration: {}", config_path.display());
        }
    }

    Ok(())
}

fn resolve_project(start_root: &Path, configured: &Path) -> (PathBuf, PathBuf) {
    if configured.is_absolute() {
        let root = configured
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| start_root.to_path_buf());
        return (root, configured.to_path_buf());
    }

    let direct = start_root.join(configured);
    if direct.exists() || configured != Path::new("crc.toml") {
        return (start_root.to_path_buf(), direct);
    }

    for ancestor in start_root.ancestors() {
        let candidate = ancestor.join("crc.toml");
        if candidate.is_file() {
            return (ancestor.to_path_buf(), candidate);
        }
    }
    (start_root.to_path_buf(), direct)
}

fn create_project(root: &Path, name: &str, dir: Option<&Path>) -> Result<()> {
    validate_project_name(name)?;
    let target_dir = dir.map_or_else(|| root.join(name), |directory| root.join(directory));

    if target_dir.exists() {
        anyhow::bail!("target directory already exists: {}", target_dir.display());
    }

    std::fs::create_dir_all(target_dir.join("crc/dist"))?;
    std::fs::create_dir_all(target_dir.join("include"))?;
    TemplateEngine::new()?.create_project(&target_dir, name)?;

    println!("created project: {}", target_dir.display());
    println!("{}", target_dir.display());
    println!("├── CMakeLists.txt");
    println!("├── meson.build");
    println!("├── crc.toml");
    println!("├── crc/");
    println!("│   ├── src/main.cr");
    println!("│   ├── include/main.hr");
    println!("│   └── dist/");
    println!("├── include/");
    println!("└── src/main.c");

    Ok(())
}

fn validate_project_name(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        anyhow::bail!("project name must contain only ASCII letters, digits, '_' or '-': {name}");
    }
    Ok(())
}

fn build_project(config: &Config, project_root: &Path, _release: bool) -> Result<()> {
    crate::Compiler::new(config.clone()).build_project(project_root)?;
    println!("build completed");
    Ok(())
}

fn dev_mode(config: &Config, project_root: &Path, _port: Option<u16>) -> Result<()> {
    println!("starting development mode");
    println!(
        "watching: {}",
        project_root.join(&config.project.crate_dir).display()
    );

    let mut watcher = crate::watch::Watcher::new(config.clone());
    watcher.start(project_root.to_path_buf())?;

    ctrlc::set_handler(move || {
        println!("stopping development mode");
        std::process::exit(0);
    })?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn check_project(config: &Config, project_root: &Path) -> Result<()> {
    crate::Compiler::new(config.clone()).check_project(project_root)?;
    println!("check completed");
    Ok(())
}

fn clean_project(config: &Config, project_root: &Path) -> Result<()> {
    if let Some(dist_dir) = crate::Compiler::new(config.clone()).clean_project(project_root)? {
        println!("removed: {}", dist_dir.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_project_names_that_can_escape_templates() {
        assert!(validate_project_name("valid-project_2").is_ok());
        assert!(validate_project_name("bad\"name").is_err());
        assert!(validate_project_name("../bad").is_err());
    }

    #[test]
    fn discovers_the_nearest_parent_project() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let project = directory.path().join("project");
        let nested = project.join("src/nested");
        std::fs::create_dir_all(&nested).expect("nested directory");
        std::fs::write(project.join("crc.toml"), "").expect("project marker");

        let (root, config) = resolve_project(&nested, Path::new("crc.toml"));
        assert_eq!(root, project);
        assert_eq!(config, root.join("crc.toml"));
    }
}
