//! Source-preserving `.hr` to `.h` public ABI generation.

use tree_sitter::Node;

use crate::syntax::{Diagnostic, DiagnosticSeverity, SyntaxUnit};

/// Generated public header and its diagnostics.
#[derive(Debug, Clone)]
pub struct HeaderEmission {
    pub source: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// Replaces public asynchronous declarations with their opaque task ABI.
#[must_use]
pub fn emit_header(unit: &SyntaxUnit, prefix: &str) -> HeaderEmission {
    let mut diagnostics = unit.diagnostics().to_vec();
    let mut replacements = Vec::new();
    let mut declarations = Vec::new();
    collect_file_scope_async_declarations(unit.tree().root_node(), &mut declarations);
    for node in declarations {
        match emit_async_declaration(unit, node, prefix) {
            Ok(replacement) => replacements.push((node.start_byte(), node.end_byte(), replacement)),
            Err(diagnostic) => diagnostics.push(*diagnostic),
        }
    }
    replacements.sort_by_key(|replacement| replacement.0);

    let mut source = String::new();
    if !replacements.is_empty() && !unit.source().contains("cr_runtime.h") {
        source.push_str("#include \"cr_runtime.h\"\n\n");
    }
    let mut cursor = 0;
    for (start, end, replacement) in replacements {
        source.push_str(&unit.source()[cursor..start]);
        source.push_str(&replacement);
        cursor = end;
    }
    source.push_str(&unit.source()[cursor..]);
    source = source.replace(".hr\"", ".h\"").replace(".hr>", ".h>");

    HeaderEmission {
        source,
        diagnostics,
    }
}

fn collect_file_scope_async_declarations<'tree>(
    node: Node<'tree>,
    declarations: &mut Vec<Node<'tree>>,
) {
    for child in named_children(node) {
        if child.kind() == "declaration" && has_direct_child(child, "async_specifier") {
            declarations.push(child);
        } else if child.kind().starts_with("preproc_") {
            collect_file_scope_async_declarations(child, declarations);
        }
    }
}

fn emit_async_declaration(
    unit: &SyntaxUnit,
    node: Node<'_>,
    prefix: &str,
) -> Result<String, Box<Diagnostic>> {
    let type_node = required_field(unit, node, "type")?;
    let mut cursor = node.walk();
    let declarators: Vec<_> = node
        .children_by_field_name("declarator", &mut cursor)
        .collect();
    if declarators.len() != 1 {
        return Err(Box::new(error(
            unit,
            node,
            "CRH1001",
            "an async header declaration must declare exactly one function",
        )));
    }
    let declarator = declarators[0];
    let function_declarator =
        find_descendant(declarator, "function_declarator").ok_or_else(|| {
            Box::new(error(
                unit,
                declarator,
                "CRH1002",
                "`__async` header declaration requires a function declarator",
            ))
        })?;
    let name_node = declarator_identifier(function_declarator).ok_or_else(|| {
        Box::new(error(
            unit,
            declarator,
            "CRH1003",
            "unable to resolve async function name",
        ))
    })?;
    let parameters = function_declarator
        .child_by_field_name("parameters")
        .ok_or_else(|| {
            Box::new(error(
                unit,
                function_declarator,
                "CRH1004",
                "async function declaration has no parameter list",
            ))
        })?;

    let name = c_identifier(unit.text(name_node));
    let prefix = c_identifier(prefix);
    let stem = if prefix.is_empty() {
        name
    } else {
        format!("{prefix}{name}")
    };
    let task = format!("{stem}_task");
    let return_type = unit.text(type_node).trim();
    let parameter_text = unit.text(parameters);
    let parameter_text = parameter_text
        .strip_prefix('(')
        .and_then(|parameters| parameters.strip_suffix(')'))
        .unwrap_or(parameter_text)
        .trim();
    let create_parameters = if parameter_text.is_empty() || parameter_text == "void" {
        "cr_error *out_error".to_owned()
    } else {
        format!("{parameter_text}, cr_error *out_error")
    };

    let mut output = String::new();
    output.push_str(&format!("typedef struct {task} {task};\n"));
    output.push_str(&format!("{task} *{stem}_create({create_parameters});\n"));
    output.push_str(&format!(
        "cr_poll_status {stem}_poll({task} *task, const cr_poll_context *poll_context);\n"
    ));
    output.push_str(&format!("void {stem}_destroy({task} *task);\n"));
    if return_type != "void" {
        output.push_str(&format!(
            "const {return_type} *{stem}_result(const {task} *task);\n"
        ));
        output.push_str(&format!(
            "const {return_type} *{stem}_yielded(const {task} *task);\n"
        ));
    }
    output.push_str(&format!(
        "const cr_error *{stem}_error(const {task} *task);\n"
    ));
    output.push_str(&format!(
        "cr_awaitable {stem}_as_awaitable({task} *task);\n"
    ));
    output.push_str(&format!(
        "cr_awaitable {stem}_into_awaitable({task} *task);"
    ));
    Ok(output)
}

fn required_field<'tree>(
    unit: &SyntaxUnit,
    node: Node<'tree>,
    field: &str,
) -> Result<Node<'tree>, Box<Diagnostic>> {
    node.child_by_field_name(field).ok_or_else(|| {
        Box::new(error(
            unit,
            node,
            "CRH1000",
            &format!("missing required `{field}` syntax"),
        ))
    })
}

fn error(unit: &SyntaxUnit, node: Node<'_>, code: &'static str, message: &str) -> Diagnostic {
    Diagnostic {
        code,
        severity: DiagnosticSeverity::Error,
        message: message.to_owned(),
        primary_span: unit.span(node),
        related: Vec::new(),
    }
}

fn has_direct_child(node: Node<'_>, kind: &str) -> bool {
    named_children(node)
        .into_iter()
        .any(|child| child.kind() == kind)
}

fn declarator_identifier(node: Node<'_>) -> Option<Node<'_>> {
    if node.kind() == "identifier" {
        return Some(node);
    }
    node.child_by_field_name("declarator")
        .and_then(declarator_identifier)
        .or_else(|| {
            named_children(node)
                .into_iter()
                .find(|child| child.kind() == "identifier")
        })
        .or_else(|| find_descendant(node, "identifier"))
}

fn find_descendant<'tree>(node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    let mut cursor = node.walk();
    loop {
        let current = cursor.node();
        if current.kind() == kind {
            return Some(current);
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return None;
            }
        }
    }
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn c_identifier(value: &str) -> String {
    value
        .chars()
        .enumerate()
        .map(|(index, character)| {
            let valid = if index == 0 {
                character == '_' || character.is_ascii_alphabetic()
            } else {
                character == '_' || character.is_ascii_alphanumeric()
            };
            if valid { character } else { '_' }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    use crate::runtime_abi::runtime_header;
    use crate::syntax::SyntaxParser;

    use super::*;

    #[test]
    fn emits_opaque_public_async_abi_and_rewrites_hr_include() {
        let source = r#"
#include "dependency.hr"

__async int fetch(int socket);
void ordinary(int value);
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("api.hr"), source)
            .expect("header parses");
        let emission = emit_header(&syntax, "cr_");

        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );
        assert!(emission.source.contains("typedef struct cr_fetch_task"));
        assert!(
            emission
                .source
                .contains("cr_fetch_create(int socket, cr_error *out_error)")
        );
        assert!(emission.source.contains("cr_fetch_as_awaitable"));
        assert!(emission.source.contains("dependency.h"));
        assert!(emission.source.contains("void ordinary(int value);"));
    }

    #[test]
    fn generated_header_is_valid_c11() {
        let source = r#"
#ifndef API_H
#define API_H
__async int fetch(int socket);
#endif
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("api.hr"), source)
            .expect("header parses");
        let emission = emit_header(&syntax, "cr_");
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );
        assert!(!emission.source.contains("__async"));
        assert!(emission.source.contains("#ifndef API_H"));

        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("cr_runtime.h"), runtime_header()).expect("runtime header");
        fs::write(directory.path().join("api.h"), emission.source).expect("generated header");
        fs::write(
            directory.path().join("test.c"),
            "#include \"api.h\"\nint main(void) { return 0; }\n",
        )
        .expect("test source");
        let compiler = ["clang", "gcc"]
            .into_iter()
            .find(|compiler| Command::new(compiler).arg("--version").output().is_ok())
            .expect("Clang or GCC is required for this test");
        let output = Command::new(compiler)
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-fsyntax-only"])
            .arg("test.c")
            .current_dir(directory.path())
            .output()
            .expect("native compiler runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
