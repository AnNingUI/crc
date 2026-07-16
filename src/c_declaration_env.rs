//! Conservative file-scope declaration environments for movable C layouts.

use std::collections::BTreeMap;

use tree_sitter::Node;

use crate::syntax::SyntaxUnit;

/// Why a compiler-private declaration can't move to an earlier source anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationMoveBlock {
    TypeNotVisible,
    PreprocessorBoundary,
}

/// File-scope facts that affect generated type and prototype placement.
#[derive(Debug, Clone, Default)]
pub struct CDeclarationEnvironment {
    type_declarations: BTreeMap<String, Vec<usize>>,
    local_type_declarations: Vec<LocalTypeDeclaration>,
    preprocessing_boundaries: Vec<usize>,
    packing_barrier: bool,
}

#[derive(Debug, Clone)]
struct LocalTypeDeclaration {
    name: String,
    function_start: usize,
    function_end: usize,
}

impl CDeclarationEnvironment {
    /// Returns whether a context type was declared only inside its function.
    #[must_use]
    pub fn contains_local_context_type<'a>(
        &self,
        types: impl IntoIterator<Item = &'a str>,
        function_start: usize,
        function_end: usize,
    ) -> bool {
        types.into_iter().flat_map(type_identifiers).any(|name| {
            self.local_type_declarations.iter().any(|declaration| {
                declaration.name == name
                    && declaration.function_start == function_start
                    && declaration.function_end == function_end
            })
        })
    }

    /// Checks whether every locally declared type is visible at an anchor.
    #[must_use]
    pub fn classify_visibility_at<'a>(
        &self,
        types: impl IntoIterator<Item = &'a str>,
        anchor: usize,
    ) -> Option<DeclarationMoveBlock> {
        for identifier in types.into_iter().flat_map(type_identifiers) {
            if self
                .type_declarations
                .get(identifier)
                .is_some_and(|positions| positions.iter().all(|position| *position > anchor))
            {
                return Some(DeclarationMoveBlock::TypeNotVisible);
            }
        }
        None
    }

    /// Checks whether type fragments remain valid when moved to `anchor`.
    #[must_use]
    pub fn classify_move<'a>(
        &self,
        types: impl IntoIterator<Item = &'a str>,
        original: usize,
        anchor: usize,
    ) -> Option<DeclarationMoveBlock> {
        if anchor >= original {
            return None;
        }
        if self
            .preprocessing_boundaries
            .iter()
            .any(|position| anchor < *position && *position < original)
        {
            return Some(DeclarationMoveBlock::PreprocessorBoundary);
        }
        for identifier in types.into_iter().flat_map(type_identifiers) {
            if self
                .type_declarations
                .get(identifier)
                .is_some_and(|positions| {
                    positions.iter().all(|position| *position > anchor)
                        && positions.iter().any(|position| *position <= original)
                })
            {
                return Some(DeclarationMoveBlock::TypeNotVisible);
            }
        }
        None
    }

    /// Returns whether the translation unit contains a packing directive.
    #[must_use]
    pub fn has_packing_barrier(&self) -> bool {
        self.packing_barrier
    }

    /// Returns the indexed declaration positions for structural tests.
    #[cfg(test)]
    fn declarations(&self, name: &str) -> &[usize] {
        self.type_declarations
            .get(name)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }
}

/// Builds a conservative environment from one parsed translation unit.
#[must_use]
pub fn build_c_declaration_environment(unit: &SyntaxUnit) -> CDeclarationEnvironment {
    let mut environment = CDeclarationEnvironment {
        packing_barrier: contains_packing_directive(unit.source()),
        ..CDeclarationEnvironment::default()
    };
    collect_file_scope(unit, unit.tree().root_node(), &mut environment);
    for positions in environment.type_declarations.values_mut() {
        positions.sort_unstable();
        positions.dedup();
    }
    environment.preprocessing_boundaries.sort_unstable();
    environment.preprocessing_boundaries.dedup();
    environment
}

fn contains_packing_directive(source: &str) -> bool {
    let lower = source.to_ascii_lowercase();
    lower.lines().any(|line| {
        line.chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>()
            .starts_with("#pragmapack")
    }) || lower.contains("_pragma(\"pack")
        || (lower.contains("__attribute__") && lower.contains("packed"))
        || lower.contains("__declspec(align")
}

fn collect_file_scope(
    unit: &SyntaxUnit,
    node: Node<'_>,
    environment: &mut CDeclarationEnvironment,
) {
    for child in named_children(node) {
        if child.kind() == "function_definition" {
            collect_local_types(unit, child, environment);
            continue;
        }
        if child.kind().starts_with("preproc_") {
            environment
                .preprocessing_boundaries
                .push(child.start_byte());
            collect_file_scope(unit, child, environment);
            continue;
        }
        if child.kind() == "type_definition" {
            collect_type_names(unit, child, child.start_byte(), environment);
        } else if child.kind() == "declaration" {
            collect_defined_tag_names(unit, child, child.start_byte(), environment);
        }
    }
}

fn collect_defined_tag_names(
    unit: &SyntaxUnit,
    node: Node<'_>,
    position: usize,
    environment: &mut CDeclarationEnvironment,
) {
    if defines_tag(node) {
        for child in named_children(node) {
            if child.kind() == "type_identifier" {
                environment
                    .type_declarations
                    .entry(unit.text(child).to_owned())
                    .or_default()
                    .push(position);
            }
        }
        return;
    }
    for child in named_children(node) {
        collect_defined_tag_names(unit, child, position, environment);
    }
}

fn collect_local_types(
    unit: &SyntaxUnit,
    function: Node<'_>,
    environment: &mut CDeclarationEnvironment,
) {
    fn visit(
        unit: &SyntaxUnit,
        node: Node<'_>,
        function: Node<'_>,
        output: &mut Vec<LocalTypeDeclaration>,
    ) {
        if node.kind() == "type_definition" || defines_tag(node) {
            let mut names = Vec::new();
            collect_type_identifier_text(unit, node, &mut names);
            names.sort();
            names.dedup();
            output.extend(names.into_iter().map(|name| LocalTypeDeclaration {
                name,
                function_start: function.start_byte(),
                function_end: function.end_byte(),
            }));
            if node.kind() == "type_definition" {
                return;
            }
        }
        for child in named_children(node) {
            visit(unit, child, function, output);
        }
    }
    for child in named_children(function) {
        if child.kind() == "compound_statement" {
            visit(
                unit,
                child,
                function,
                &mut environment.local_type_declarations,
            );
        }
    }
}

fn defines_tag(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "struct_specifier" | "union_specifier" | "enum_specifier"
    ) && named_children(node)
        .iter()
        .any(|child| matches!(child.kind(), "field_declaration_list" | "enumerator_list"))
}

fn collect_type_identifier_text(unit: &SyntaxUnit, node: Node<'_>, output: &mut Vec<String>) {
    if node.kind() == "type_identifier" {
        output.push(unit.text(node).to_owned());
    }
    for child in named_children(node) {
        if child.kind() != "field_declaration_list" {
            collect_type_identifier_text(unit, child, output);
        }
    }
}

fn collect_type_names(
    unit: &SyntaxUnit,
    node: Node<'_>,
    position: usize,
    environment: &mut CDeclarationEnvironment,
) {
    if node.kind() == "type_identifier" {
        environment
            .type_declarations
            .entry(unit.text(node).to_owned())
            .or_default()
            .push(position);
    }
    for child in named_children(node) {
        if child.kind() != "field_declaration_list" {
            collect_type_names(unit, child, position, environment);
        }
    }
}

fn type_identifiers(fragment: &str) -> impl Iterator<Item = &str> {
    fragment
        .split(|character: char| !(character == '_' || character.is_ascii_alphanumeric()))
        .filter(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .next()
                    .is_some_and(|character| character == '_' || character.is_ascii_alphabetic())
                && !is_c_type_keyword(word)
        })
}

fn is_c_type_keyword(word: &str) -> bool {
    matches!(
        word,
        "void"
            | "char"
            | "short"
            | "int"
            | "long"
            | "float"
            | "double"
            | "signed"
            | "unsigned"
            | "_Bool"
            | "bool"
            | "const"
            | "volatile"
            | "restrict"
            | "_Atomic"
            | "struct"
            | "union"
            | "enum"
            | "static"
            | "extern"
            | "register"
            | "auto"
    )
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::syntax::SyntaxParser;

    use super::*;

    #[test]
    fn classifies_type_and_preprocessor_visibility_barriers() {
        let source = r#"
typedef int EarlyValue;
__async int parent(void) { return 0; }

typedef int LaterValue;
typedef int UnusedValue;
EarlyValue file_value;

__async int typed_child(LaterValue value) { return value; }
__async int early_child(EarlyValue value) { return value + file_value; }

#define CHILD_VALUE int
__async int macro_child(CHILD_VALUE value) { return value; }

__async int local_child(int value) {
    typedef int LocalValue;
    LocalValue held = value;
    __yield held;
    return held;
}
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("environment.cr"), source)
            .expect("source parses");
        let environment = build_c_declaration_environment(&syntax);
        let parent = source.find("__async int parent").expect("parent position");
        let typed_child = source
            .find("__async int typed_child")
            .expect("typed child position");
        let macro_child = source
            .find("__async int macro_child")
            .expect("macro child position");
        let early_child = source
            .find("__async int early_child")
            .expect("early child position");

        assert!(!environment.declarations("LaterValue").is_empty());
        assert_eq!(
            environment.classify_move(["LaterValue"], typed_child, parent),
            Some(DeclarationMoveBlock::TypeNotVisible)
        );
        assert_eq!(
            environment.classify_move(["int"], typed_child, parent),
            None
        );
        assert_eq!(
            environment.classify_move(["EarlyValue"], early_child, parent),
            None
        );
        assert_eq!(
            environment.classify_move(["CHILD_VALUE"], macro_child, typed_child),
            Some(DeclarationMoveBlock::PreprocessorBoundary)
        );
        assert_eq!(
            environment.classify_move(["LaterValue"], parent, typed_child),
            None
        );
        assert_eq!(
            environment.classify_visibility_at(["LaterValue"], parent),
            Some(DeclarationMoveBlock::TypeNotVisible)
        );
        assert_eq!(
            environment.classify_visibility_at(["LaterValue"], typed_child),
            None
        );
        let local_start = source
            .find("__async int local_child")
            .expect("local child position");
        let local_child = named_children(syntax.tree().root_node())
            .into_iter()
            .find(|node| node.start_byte() == local_start)
            .expect("local child node");
        assert!(environment.contains_local_context_type(
            ["LocalValue"],
            local_child.start_byte(),
            local_child.end_byte()
        ));
    }

    #[test]
    fn detects_packing_as_a_layout_barrier() {
        let source = "#pragma pack(push, 1)\ntypedef struct Packed { char value; } Packed;\n#pragma pack(pop)\n";
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("packing.cr"), source)
            .expect("source parses");
        let environment = build_c_declaration_environment(&syntax);
        assert!(environment.has_packing_barrier());
    }
}
