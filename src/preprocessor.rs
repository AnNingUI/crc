//! Native preprocessor validation for the configured active C view.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result};

use crate::config::{CStandard, Config};

pub fn validate_translation_unit(
    project_root: &Path,
    source: &Path,
    config: &Config,
) -> Result<()> {
    if !config.preprocessor.enabled {
        return Ok(());
    }
    let command = resolve_command(config.preprocessor.command.as_deref())?;
    let is_msvc = command
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("cl"));
    let output = if is_msvc {
        preprocess_msvc(&command, project_root, source, config)?
    } else {
        preprocess_gnu(&command, project_root, source, config)?
    };
    if output.status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "native preprocessing failed for {}\n{}",
            source.display(),
            String::from_utf8_lossy(&output.stderr)
        )
    }
}

fn resolve_command(configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(command) = configured {
        return Ok(command.to_path_buf());
    }
    for candidate in ["clang", "gcc", "cl"] {
        let probe = if candidate == "cl" { "/?" } else { "--version" };
        if Command::new(candidate)
            .arg(probe)
            .output()
            .is_ok_and(|output| output.status.success())
        {
            return Ok(PathBuf::from(candidate));
        }
    }
    anyhow::bail!("preprocessing is enabled, but Clang, GCC, or MSVC wasn't found")
}

fn preprocess_gnu(
    command: &Path,
    project_root: &Path,
    source: &Path,
    config: &Config,
) -> Result<Output> {
    let mut process = Command::new(command);
    process.arg("-E").arg("-x").arg("c").arg(format!(
        "-std={}",
        c_standard_name(&config.build.c_standard)
    ));
    for include in &config.preprocessor.include_paths {
        process.arg("-I").arg(project_root.join(include));
    }
    for define in &config.preprocessor.defines {
        process.arg(format!("-D{define}"));
    }
    for include in &config.preprocessor.forced_includes {
        process.arg("-include").arg(project_root.join(include));
    }
    process
        .arg(source)
        .current_dir(project_root)
        .output()
        .with_context(|| format!("failed to start preprocessor {}", command.display()))
}

fn preprocess_msvc(
    command: &Path,
    project_root: &Path,
    source: &Path,
    config: &Config,
) -> Result<Output> {
    let mut process = Command::new(command);
    process.arg("/nologo").arg("/EP").arg("/TC");
    for include in &config.preprocessor.include_paths {
        process.arg(format!("/I{}", project_root.join(include).display()));
    }
    for define in &config.preprocessor.defines {
        process.arg(format!("/D{define}"));
    }
    for include in &config.preprocessor.forced_includes {
        process.arg(format!("/FI{}", project_root.join(include).display()));
    }
    process
        .arg(source)
        .current_dir(project_root)
        .output()
        .with_context(|| format!("failed to start preprocessor {}", command.display()))
}

fn c_standard_name(standard: &CStandard) -> &'static str {
    match standard {
        CStandard::C89 => "c89",
        CStandard::C99 => "c99",
        CStandard::C11 => "c11",
        CStandard::C17 => "c17",
        CStandard::C23 => "c23",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn configured_macro_selects_a_valid_active_view() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let source = directory.path().join("active.cr");
        fs::write(
            &source,
            "#if ENABLED\nint selected;\n#else\n#include \"missing.h\"\n#endif\n",
        )
        .expect("source");
        let mut config = Config::default();
        config.preprocessor.enabled = true;
        config.preprocessor.defines.push("ENABLED=1".to_owned());
        validate_translation_unit(directory.path(), &source, &config)
            .expect("active preprocessor view");
    }
}
