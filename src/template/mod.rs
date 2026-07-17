//! Compiler-owned project templates used by `crc create`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tera::{Context, Tera};

struct ProjectTemplate {
    name: &'static str,
    destination: &'static str,
    source: &'static str,
}

const PROJECT_TEMPLATES: &[ProjectTemplate] = &[
    ProjectTemplate {
        name: "cmake",
        destination: "CMakeLists.txt",
        source: include_str!("templates/CMakeLists.txt"),
    },
    ProjectTemplate {
        name: "meson",
        destination: "meson.build",
        source: include_str!("templates/meson.build"),
    },
    ProjectTemplate {
        name: "config",
        destination: "crc.toml",
        source: include_str!("templates/crc.toml"),
    },
    ProjectTemplate {
        name: "cr_source",
        destination: "crc/src/main.cr",
        source: include_str!("templates/main.cr"),
    },
    ProjectTemplate {
        name: "cr_header",
        destination: "crc/include/main.hr",
        source: include_str!("templates/main.hr"),
    },
    ProjectTemplate {
        name: "c_source",
        destination: "src/main.c",
        source: include_str!("templates/main.c"),
    },
    ProjectTemplate {
        name: "gitignore",
        destination: ".gitignore",
        source: include_str!("templates/.gitignore"),
    },
];

pub struct TemplateEngine {
    tera: Tera,
}

impl TemplateEngine {
    pub fn new() -> Result<Self> {
        let mut tera = Tera::default();
        for template in PROJECT_TEMPLATES {
            tera.add_raw_template(template.name, template.source)?;
        }
        Ok(Self { tera })
    }

    pub fn create_project(&self, target: &Path, project_name: &str) -> Result<Vec<PathBuf>> {
        let context = Self::create_context(project_name, "0.1.0");
        let mut written = Vec::with_capacity(PROJECT_TEMPLATES.len());
        for template in PROJECT_TEMPLATES {
            let destination = target.join(template.destination);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&destination, self.tera.render(template.name, &context)?)?;
            written.push(destination);
        }
        Ok(written)
    }

    pub fn render(&self, name: &str, context: &Context) -> Result<String> {
        Ok(self.tera.render(name, context)?)
    }

    pub fn create_context(project_name: &str, version: &str) -> Context {
        let mut context = Context::new();
        context.insert("project_name", project_name);
        context.insert("version", version);
        context.insert("crate_dir", "crc/src");
        context.insert("dist_dir", "crc/dist");
        context.insert("include_dir", "crc/include");
        context.insert("src_dir", "src");
        context
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_templates_render_the_complete_project() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let engine = TemplateEngine::new().expect("templates load");
        let written = engine
            .create_project(directory.path(), "template_test")
            .expect("project renders");

        assert_eq!(written.len(), PROJECT_TEMPLATES.len());
        assert!(directory.path().join("crc/include/main.hr").is_file());
        assert!(directory.path().join("crc/src/main.cr").is_file());
        assert!(directory.path().join("src/main.c").is_file());
        assert!(!directory.path().join("crc/include/cr_runtime.h").exists());
        let cmake =
            fs::read_to_string(directory.path().join("CMakeLists.txt")).expect("CMake template");
        assert!(cmake.contains("project(template_test LANGUAGES C)"));
        assert!(cmake.contains("${CR_DIST_DIR}/include"));
        assert!(cmake.contains("crc-generated-dependencies.cmake"));
        assert!(cmake.contains("CR_GENERATED_DEPENDENCIES"));
        let meson =
            fs::read_to_string(directory.path().join("meson.build")).expect("Meson template");
        assert!(meson.contains("dependencies: cr_generated_dependencies"));
        let config =
            fs::read_to_string(directory.path().join("crc.toml")).expect("config template");
        assert!(config.contains("[runtime]"));
        assert!(config.contains("executor = \"manual\""));
        assert!(config.contains("backends = []"));
    }
}
