//! Stable C-linkage identities for CR asynchronous functions.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use tree_sitter::Node;

use crate::syntax::{Diagnostic, DiagnosticSeverity, RelatedDiagnostic, SourceSpan, SyntaxUnit};

/// Stable identity of one asynchronous function within a project index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionId(pub u32);

/// Normalized identity of one source translation unit.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TranslationUnitId(pub String);

/// C linkage identity used to merge declarations with definitions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AsyncLinkageKey {
    External {
        name: String,
    },
    Internal {
        translation_unit: TranslationUnitId,
        name: String,
    },
}

/// Whether one indexed site declares or defines its function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AsyncSymbolSiteKind {
    Declaration,
    Definition,
}

/// One async parameter with its source declaration and adjusted C type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncParameter {
    pub name: Option<String>,
    pub declared_type: String,
    pub adjusted_type: String,
    pub span: SourceSpan,
}

/// One source location that contributes to an indexed async symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncSymbolSite {
    pub translation_unit: TranslationUnitId,
    pub kind: AsyncSymbolSiteKind,
    pub result_type: String,
    pub parameters: String,
    pub parameter_types: Vec<AsyncParameter>,
    pub is_variadic: bool,
    pub span: SourceSpan,
}

/// One merged asynchronous function symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsyncSymbol {
    pub id: FunctionId,
    pub key: AsyncLinkageKey,
    pub name: String,
    pub result_type: String,
    pub parameters: Vec<AsyncParameter>,
    pub is_variadic: bool,
    pub public_stem: String,
    pub sites: Vec<AsyncSymbolSite>,
}

/// Visibility of the concrete generated task layout from one translation unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutVisibility {
    Visible,
    Opaque,
}

/// A symbol lookup result relative to one translation unit.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedAsyncSymbol<'index> {
    pub symbol: &'index AsyncSymbol,
    pub layout_visibility: LayoutVisibility,
}

/// One parsed input to project symbol-index construction.
#[derive(Clone, Copy)]
pub struct AsyncSymbolInput<'unit> {
    pub project_path: &'unit Path,
    pub unit: &'unit SyntaxUnit,
}

/// A deterministic collection of project asynchronous symbols.
#[derive(Debug, Clone, Default)]
pub struct AsyncSymbolIndex {
    symbols: Vec<AsyncSymbol>,
    by_key: BTreeMap<AsyncLinkageKey, FunctionId>,
}

impl AsyncSymbolIndex {
    /// Returns symbols in stable `FunctionId` order.
    #[must_use]
    pub fn symbols(&self) -> &[AsyncSymbol] {
        &self.symbols
    }

    /// Resolves one async name with C internal linkage taking precedence.
    #[must_use]
    pub fn resolve(&self, project_path: &Path, name: &str) -> Option<ResolvedAsyncSymbol<'_>> {
        let translation_unit = normalize_project_path(project_path)?;
        let internal = AsyncLinkageKey::Internal {
            translation_unit: translation_unit.clone(),
            name: name.to_owned(),
        };
        let external = AsyncLinkageKey::External {
            name: name.to_owned(),
        };
        let id = self
            .by_key
            .get(&internal)
            .or_else(|| self.by_key.get(&external))?;
        let symbol = &self.symbols[id.0 as usize];
        let layout_visibility = if symbol.sites.iter().any(|site| {
            site.kind == AsyncSymbolSiteKind::Definition
                && site.translation_unit == translation_unit
        }) {
            LayoutVisibility::Visible
        } else {
            LayoutVisibility::Opaque
        };
        Some(ResolvedAsyncSymbol {
            symbol,
            layout_visibility,
        })
    }

    /// Returns one symbol by its stable identity.
    #[must_use]
    pub fn symbol(&self, id: FunctionId) -> Option<&AsyncSymbol> {
        self.symbols.get(id.0 as usize)
    }

    /// Resolves a known identity relative to one translation unit.
    #[must_use]
    pub fn resolve_id(
        &self,
        project_path: &Path,
        id: FunctionId,
    ) -> Option<ResolvedAsyncSymbol<'_>> {
        let translation_unit = normalize_project_path(project_path)?;
        let symbol = self.symbol(id)?;
        let layout_visibility = if symbol.sites.iter().any(|site| {
            site.kind == AsyncSymbolSiteKind::Definition
                && site.translation_unit == translation_unit
        }) {
            LayoutVisibility::Visible
        } else {
            LayoutVisibility::Opaque
        };
        Some(ResolvedAsyncSymbol {
            symbol,
            layout_visibility,
        })
    }

    /// Returns whether this unit already declares or defines an identity.
    #[must_use]
    pub fn has_site_before(
        &self,
        project_path: &Path,
        id: FunctionId,
        source_position: usize,
    ) -> bool {
        let Some(translation_unit) = normalize_project_path(project_path) else {
            return false;
        };
        self.symbol(id).is_some_and(|symbol| {
            symbol.sites.iter().any(|site| {
                site.translation_unit == translation_unit && site.span.start_byte < source_position
            })
        })
    }
}

/// Symbol-index output plus user-facing source diagnostics.
#[derive(Debug, Clone)]
pub struct AsyncSymbolIndexBuild {
    pub index: AsyncSymbolIndex,
    pub diagnostics: Vec<Diagnostic>,
}

/// Builds a deterministic index from parsed project inputs.
#[must_use]
pub fn build_async_symbol_index(
    inputs: &[AsyncSymbolInput<'_>],
    prefix: &str,
) -> AsyncSymbolIndexBuild {
    let mut diagnostics = Vec::new();
    let mut grouped: BTreeMap<AsyncLinkageKey, Vec<AsyncSymbolSite>> = BTreeMap::new();

    for input in inputs {
        let Some(translation_unit) = normalize_project_path(input.project_path) else {
            diagnostics.push(Diagnostic {
                code: "CRS2001",
                severity: DiagnosticSeverity::Error,
                message: format!(
                    "symbol-index path must be project-relative: {}",
                    input.project_path.display()
                ),
                primary_span: input.unit.span(input.unit.tree().root_node()),
                related: Vec::new(),
            });
            continue;
        };
        collect_async_sites(
            input.unit,
            input.unit.tree().root_node(),
            &translation_unit,
            &mut grouped,
        );
    }

    let mut symbols = Vec::with_capacity(grouped.len());
    let mut by_key = BTreeMap::new();
    for (key, mut sites) in grouped {
        sites.sort_by(|left, right| {
            (
                &left.translation_unit,
                left.span.start_byte,
                left.kind,
                &left.result_type,
                &left.parameters,
            )
                .cmp(&(
                    &right.translation_unit,
                    right.span.start_byte,
                    right.kind,
                    &right.result_type,
                    &right.parameters,
                ))
        });
        diagnose_sites(&key, &sites, &mut diagnostics);
        let id = FunctionId(symbols.len() as u32);
        let name = linkage_name(&key).to_owned();
        let result_type = canonical_site(&sites)
            .map(|site| site.result_type.clone())
            .unwrap_or_default();
        let parameters = canonical_site(&sites)
            .map(|site| site.parameter_types.clone())
            .unwrap_or_default();
        let is_variadic = canonical_site(&sites).is_some_and(|site| site.is_variadic);
        let public_stem = symbol_stem(prefix, &key);
        by_key.insert(key.clone(), id);
        symbols.push(AsyncSymbol {
            id,
            key,
            name,
            result_type,
            parameters,
            is_variadic,
            public_stem,
            sites,
        });
    }

    AsyncSymbolIndexBuild {
        index: AsyncSymbolIndex { symbols, by_key },
        diagnostics,
    }
}

/// Builds a one-file index for standalone compilation.
#[must_use]
pub fn build_local_async_symbol_index(unit: &SyntaxUnit, prefix: &str) -> AsyncSymbolIndexBuild {
    let local_path = unit
        .path()
        .file_name()
        .map(Path::new)
        .unwrap_or_else(|| unit.path());
    build_async_symbol_index(
        &[AsyncSymbolInput {
            project_path: local_path,
            unit,
        }],
        prefix,
    )
}

fn collect_async_sites(
    unit: &SyntaxUnit,
    node: Node<'_>,
    translation_unit: &TranslationUnitId,
    grouped: &mut BTreeMap<AsyncLinkageKey, Vec<AsyncSymbolSite>>,
) {
    for child in named_children(node) {
        if child.kind() == "function_definition" && has_direct_child(child, "async_specifier") {
            if let Some(declarator) = child.child_by_field_name("declarator") {
                collect_site(
                    unit,
                    child,
                    declarator,
                    AsyncSymbolSiteKind::Definition,
                    translation_unit,
                    grouped,
                );
            }
        } else if child.kind() == "declaration" && has_direct_child(child, "async_specifier") {
            let mut cursor = child.walk();
            for declarator in child.children_by_field_name("declarator", &mut cursor) {
                collect_site(
                    unit,
                    child,
                    declarator,
                    AsyncSymbolSiteKind::Declaration,
                    translation_unit,
                    grouped,
                );
            }
        } else if child.kind().starts_with("preproc_") {
            collect_async_sites(unit, child, translation_unit, grouped);
        }
    }
}

fn collect_site(
    unit: &SyntaxUnit,
    owner: Node<'_>,
    declarator: Node<'_>,
    kind: AsyncSymbolSiteKind,
    translation_unit: &TranslationUnitId,
    grouped: &mut BTreeMap<AsyncLinkageKey, Vec<AsyncSymbolSite>>,
) {
    let Some(function_declarator) = find_descendant(declarator, "function_declarator") else {
        return;
    };
    let Some(name_node) = declarator_identifier(function_declarator) else {
        return;
    };
    let Some(result_node) = owner.child_by_field_name("type") else {
        return;
    };
    let Some(parameters) = function_declarator.child_by_field_name("parameters") else {
        return;
    };
    let name = unit.text(name_node).trim().to_owned();
    let internal = named_children(owner).into_iter().any(|child| {
        child.kind() == "storage_class_specifier" && unit.text(child).trim() == "static"
    });
    let key = if internal {
        AsyncLinkageKey::Internal {
            translation_unit: translation_unit.clone(),
            name,
        }
    } else {
        AsyncLinkageKey::External { name }
    };
    let (parameter_types, is_variadic) = collect_parameters(unit, parameters);
    grouped.entry(key).or_default().push(AsyncSymbolSite {
        translation_unit: translation_unit.clone(),
        kind,
        result_type: normalize_fragment(unit.text(result_node)),
        parameters: normalize_fragment(unit.text(parameters)),
        parameter_types,
        is_variadic,
        span: unit.span(owner),
    });
}

fn collect_parameters(unit: &SyntaxUnit, parameters: Node<'_>) -> (Vec<AsyncParameter>, bool) {
    let children = named_children(parameters);
    let is_variadic = children
        .iter()
        .any(|parameter| parameter.kind() == "variadic_parameter");
    let parameter_nodes: Vec<_> = children
        .into_iter()
        .filter(|parameter| parameter.kind() == "parameter_declaration")
        .collect();
    let mut result = Vec::new();
    for parameter in &parameter_nodes {
        let Some(type_node) = parameter.child_by_field_name("type") else {
            continue;
        };
        let declarator = parameter.child_by_field_name("declarator");
        if parameter_nodes.len() == 1
            && declarator.is_none()
            && normalize_fragment(unit.text(type_node)) == "void"
        {
            continue;
        }
        let name_node = declarator.and_then(declarator_identifier);
        let name = name_node.map(|name| unit.text(name).to_owned());
        let specifiers = parameter_specifiers(unit, *parameter, declarator, type_node);
        let declared_type = parameter_type(unit, &specifiers, declarator, name_node, None);
        let adjusted_type = match (declarator, name_node) {
            (Some(declarator), Some(name_node)) => {
                match first_parameter_adjustment(declarator, name_node) {
                    Some(ParameterAdjustment::Array(array)) => {
                        let inner = array
                            .child_by_field_name("declarator")
                            .expect("array parameter has an inner declarator");
                        let removal = (inner.end_byte(), array.end_byte());
                        let declarator_text = unit.text(declarator);
                        let removal_start = removal.0.saturating_sub(declarator.start_byte());
                        let removal_end = removal.1.saturating_sub(declarator.start_byte());
                        let mut without_first_dimension = declarator_text.to_owned();
                        without_first_dimension.replace_range(removal_start..removal_end, "");
                        let remaining_is_name =
                            without_first_dimension.trim() == unit.text(name_node);
                        parameter_type(
                            unit,
                            &specifiers,
                            Some(declarator),
                            Some(name_node),
                            Some(ParameterRewrite {
                                replacement: if remaining_is_name { "*" } else { "(*)" },
                                removal: Some(removal),
                            }),
                        )
                    }
                    Some(ParameterAdjustment::Function) => parameter_type(
                        unit,
                        &specifiers,
                        Some(declarator),
                        Some(name_node),
                        Some(ParameterRewrite {
                            replacement: "(*)",
                            removal: None,
                        }),
                    ),
                    None => declared_type.clone(),
                }
            }
            _ => declared_type.clone(),
        };
        result.push(AsyncParameter {
            name,
            declared_type,
            adjusted_type,
            span: unit.span(*parameter),
        });
    }
    (result, is_variadic)
}

#[derive(Clone, Copy)]
enum ParameterAdjustment<'tree> {
    Array(Node<'tree>),
    Function,
}

fn first_parameter_adjustment<'tree>(
    declarator: Node<'tree>,
    name: Node<'tree>,
) -> Option<ParameterAdjustment<'tree>> {
    let mut current = name;
    while current.id() != declarator.id() {
        let parent = current.parent()?;
        match parent.kind() {
            "pointer_declarator" => return None,
            "array_declarator" => return Some(ParameterAdjustment::Array(parent)),
            "function_declarator" => return Some(ParameterAdjustment::Function),
            _ => {}
        }
        current = parent;
    }
    None
}

struct ParameterRewrite<'a> {
    replacement: &'a str,
    removal: Option<(usize, usize)>,
}

fn parameter_type(
    unit: &SyntaxUnit,
    specifiers: &str,
    declarator: Option<Node<'_>>,
    name: Option<Node<'_>>,
    rewrite: Option<ParameterRewrite<'_>>,
) -> String {
    let Some(declarator) = declarator else {
        return normalize_fragment(specifiers);
    };
    let mut declarator_text = unit.text(declarator).to_owned();
    if let Some(removal) = rewrite.as_ref().and_then(|rewrite| rewrite.removal) {
        let start = removal.0.saturating_sub(declarator.start_byte());
        let end = removal.1.saturating_sub(declarator.start_byte());
        declarator_text.replace_range(start..end, "");
    }
    if let Some(name) = name {
        let start = name.start_byte().saturating_sub(declarator.start_byte());
        let end = name.end_byte().saturating_sub(declarator.start_byte());
        let replacement = rewrite.as_ref().map_or("", |rewrite| rewrite.replacement);
        declarator_text.replace_range(start..end, replacement);
    }
    let declarator_text = declarator_text.trim();
    if declarator_text.is_empty() {
        normalize_fragment(specifiers)
    } else {
        normalize_fragment(&format!("{} {declarator_text}", specifiers.trim()))
    }
}

fn parameter_specifiers(
    unit: &SyntaxUnit,
    parameter: Node<'_>,
    declarator: Option<Node<'_>>,
    fallback_type: Node<'_>,
) -> String {
    let Some(declarator) = declarator else {
        return unit.text(fallback_type).to_owned();
    };
    let parts: Vec<_> = named_children(parameter)
        .into_iter()
        .filter(|child| {
            child.end_byte() <= declarator.start_byte() && child.kind() != "storage_class_specifier"
        })
        .map(|child| unit.text(child))
        .collect();
    if parts.is_empty() {
        unit.text(fallback_type).to_owned()
    } else {
        parts.join(" ")
    }
}

fn diagnose_sites(
    key: &AsyncLinkageKey,
    sites: &[AsyncSymbolSite],
    diagnostics: &mut Vec<Diagnostic>,
) {
    let definitions: Vec<_> = sites
        .iter()
        .filter(|site| site.kind == AsyncSymbolSiteKind::Definition)
        .collect();
    if let Some(first) = definitions.first() {
        for duplicate in definitions.iter().skip(1) {
            diagnostics.push(Diagnostic {
                code: "CRS2002",
                severity: DiagnosticSeverity::Error,
                message: format!("duplicate async definition `{}`", linkage_name(key)),
                primary_span: duplicate.span.clone(),
                related: vec![RelatedDiagnostic {
                    message: "first definition is here".to_owned(),
                    span: first.span.clone(),
                }],
            });
        }
    }
    let Some(canonical) = canonical_site(sites) else {
        return;
    };
    for conflict in sites.iter().filter(|site| {
        site.result_type != canonical.result_type || !parameters_compatible(site, canonical)
    }) {
        diagnostics.push(Diagnostic {
            code: "CRS2003",
            severity: DiagnosticSeverity::Error,
            message: format!("conflicting async declaration `{}`", linkage_name(key)),
            primary_span: conflict.span.clone(),
            related: vec![RelatedDiagnostic {
                message: "canonical declaration is here".to_owned(),
                span: canonical.span.clone(),
            }],
        });
    }
}

fn parameters_compatible(left: &AsyncSymbolSite, right: &AsyncSymbolSite) -> bool {
    left.is_variadic == right.is_variadic
        && left.parameter_types.len() == right.parameter_types.len()
        && left
            .parameter_types
            .iter()
            .zip(&right.parameter_types)
            .all(|(left, right)| left.adjusted_type == right.adjusted_type)
}

fn canonical_site(sites: &[AsyncSymbolSite]) -> Option<&AsyncSymbolSite> {
    sites
        .iter()
        .find(|site| site.kind == AsyncSymbolSiteKind::Definition)
        .or_else(|| sites.first())
}

fn linkage_name(key: &AsyncLinkageKey) -> &str {
    match key {
        AsyncLinkageKey::External { name } | AsyncLinkageKey::Internal { name, .. } => name,
    }
}

fn symbol_stem(prefix: &str, key: &AsyncLinkageKey) -> String {
    let prefix = c_identifier(prefix);
    match key {
        AsyncLinkageKey::External { name } => format!("{prefix}{}", c_identifier(name)),
        AsyncLinkageKey::Internal {
            translation_unit,
            name,
        } => format!(
            "{prefix}{}_{}",
            c_identifier(&translation_unit.0),
            c_identifier(name)
        ),
    }
}

fn normalize_project_path(path: &Path) -> Option<TranslationUnitId> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| TranslationUnitId(parts.join("/")))
}

fn normalize_fragment(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
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
    use std::path::PathBuf;

    use crate::syntax::SyntaxParser;

    use super::*;

    fn parse(path: &str, source: &str) -> SyntaxUnit {
        SyntaxParser::new()
            .expect("grammar loads")
            .parse(PathBuf::from(path), source)
            .expect("source parses")
    }

    #[test]
    fn merges_external_header_declaration_with_definition() {
        let header = parse("crc/include/fetch.hr", "__async int fetch(int socket);");
        let source = parse(
            "crc/src/fetch.cr",
            "__async int fetch(int descriptor) { return descriptor; }",
        );
        let inputs = [
            AsyncSymbolInput {
                project_path: Path::new("crc/src/fetch.cr"),
                unit: &source,
            },
            AsyncSymbolInput {
                project_path: Path::new("crc/include/fetch.hr"),
                unit: &header,
            },
        ];
        let build = build_async_symbol_index(&inputs, "cr_");

        assert!(build.diagnostics.is_empty(), "{:?}", build.diagnostics);
        assert_eq!(build.index.symbols().len(), 1);
        let symbol = &build.index.symbols()[0];
        assert_eq!(symbol.sites.len(), 2);
        assert_eq!(symbol.public_stem, "cr_fetch");
        assert_eq!(symbol.parameters.len(), 1);
        assert_eq!(symbol.parameters[0].name.as_deref(), Some("descriptor"));
        assert_eq!(symbol.parameters[0].adjusted_type, "int");
        assert_eq!(
            build
                .index
                .resolve(Path::new("crc/src/fetch.cr"), "fetch")
                .expect("definition resolves")
                .layout_visibility,
            LayoutVisibility::Visible
        );
        assert_eq!(
            build
                .index
                .resolve(Path::new("crc/src/caller.cr"), "fetch")
                .expect("declaration resolves")
                .layout_visibility,
            LayoutVisibility::Opaque
        );
    }

    #[test]
    fn records_declarator_aware_adjusted_parameter_types() {
        let header = parse(
            "crc/include/shapes.hr",
            r#"
__async int shapes(
    const char *name,
    int values[4],
    int matrix[3][4],
    int callback(int),
    int (*handler)(int),
    long
);
"#,
        );
        let build = build_async_symbol_index(
            &[AsyncSymbolInput {
                project_path: Path::new("crc/include/shapes.hr"),
                unit: &header,
            }],
            "cr_",
        );

        assert!(build.diagnostics.is_empty(), "{:?}", build.diagnostics);
        let symbol = &build.index.symbols()[0];
        let parameters = &symbol.parameters;
        assert_eq!(parameters.len(), 6);
        assert_eq!(parameters[0].name.as_deref(), Some("name"));
        assert_eq!(parameters[0].adjusted_type, "const char *");
        assert_eq!(parameters[1].declared_type, "int [4]");
        assert_eq!(parameters[1].adjusted_type, "int *");
        assert_eq!(parameters[2].declared_type, "int [3][4]");
        assert_eq!(parameters[2].adjusted_type, "int (*)[4]");
        assert_eq!(parameters[3].declared_type, "int (int)");
        assert_eq!(parameters[3].adjusted_type, "int (*)(int)");
        assert_eq!(parameters[4].declared_type, "int (*)(int)");
        assert_eq!(parameters[4].adjusted_type, "int (*)(int)");
        assert_eq!(parameters[5].name, None);
        assert_eq!(parameters[5].adjusted_type, "long");
    }

    #[test]
    fn identity_order_is_independent_of_input_order() {
        let alpha = parse("crc/src/alpha.cr", "__async int alpha(void) { return 1; }");
        let beta = parse("crc/src/beta.cr", "__async int beta(void) { return 2; }");
        let forward = build_async_symbol_index(
            &[
                AsyncSymbolInput {
                    project_path: Path::new("crc/src/alpha.cr"),
                    unit: &alpha,
                },
                AsyncSymbolInput {
                    project_path: Path::new("crc/src/beta.cr"),
                    unit: &beta,
                },
            ],
            "cr_",
        );
        let reverse = build_async_symbol_index(
            &[
                AsyncSymbolInput {
                    project_path: Path::new("crc/src/beta.cr"),
                    unit: &beta,
                },
                AsyncSymbolInput {
                    project_path: Path::new("crc/src/alpha.cr"),
                    unit: &alpha,
                },
            ],
            "cr_",
        );

        assert_eq!(forward.index.symbols(), reverse.index.symbols());
        assert_eq!(forward.diagnostics, reverse.diagnostics);
    }

    #[test]
    fn same_internal_name_in_two_units_stays_distinct() {
        let first = parse(
            "crc/src/first.cr",
            "static __async int local(void) { return 1; }",
        );
        let second = parse(
            "crc/src/second.cr",
            "static __async int local(void) { return 2; }",
        );
        let build = build_async_symbol_index(
            &[
                AsyncSymbolInput {
                    project_path: Path::new("crc/src/first.cr"),
                    unit: &first,
                },
                AsyncSymbolInput {
                    project_path: Path::new("crc/src/second.cr"),
                    unit: &second,
                },
            ],
            "cr_",
        );

        assert!(build.diagnostics.is_empty(), "{:?}", build.diagnostics);
        assert_eq!(build.index.symbols().len(), 2);
        assert_ne!(
            build.index.symbols()[0].public_stem,
            build.index.symbols()[1].public_stem
        );
    }

    #[test]
    fn diagnoses_duplicate_definition_and_conflicting_signature() {
        let first = parse(
            "crc/src/first.cr",
            "__async int shared(int value) { return value; }",
        );
        let second = parse(
            "crc/src/second.cr",
            "__async long shared(long value) { return value; }",
        );
        let build = build_async_symbol_index(
            &[
                AsyncSymbolInput {
                    project_path: Path::new("crc/src/first.cr"),
                    unit: &first,
                },
                AsyncSymbolInput {
                    project_path: Path::new("crc/src/second.cr"),
                    unit: &second,
                },
            ],
            "cr_",
        );

        assert!(
            build
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CRS2002")
        );
        assert!(
            build
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CRS2003")
        );
    }

    #[test]
    fn rejects_non_project_relative_input_path() {
        let unit = parse("outside.cr", "__async int outside(void) { return 0; }");
        let absolute = if cfg!(windows) {
            Path::new("C:/outside.cr")
        } else {
            Path::new("/outside.cr")
        };
        let build = build_async_symbol_index(
            &[AsyncSymbolInput {
                project_path: absolute,
                unit: &unit,
            }],
            "cr_",
        );

        assert!(build.index.symbols().is_empty());
        assert_eq!(build.diagnostics.len(), 1);
        assert_eq!(build.diagnostics[0].code, "CRS2001");
    }
}
