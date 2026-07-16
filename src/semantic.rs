//! Source-backed semantic HIR for functions that contain CR syntax.

use std::collections::HashMap;

use tree_sitter::Node;

use crate::symbol_index::{AsyncSymbolIndex, FunctionId, build_local_async_symbol_index};
use crate::syntax::{Diagnostic, DiagnosticSeverity, SourceSpan, SyntaxUnit};

macro_rules! semantic_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);
    };
}

semantic_id!(DeclarationId);
semantic_id!(ScopeId);
semantic_id!(LabelId);
semantic_id!(AwaitSlotId);
semantic_id!(ResultSlotId);

/// Semantic output for every source function that requires CR lowering.
#[derive(Debug, Clone)]
pub struct HirUnit {
    pub functions: Vec<HirFunction>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A transformed C or CR function.
#[derive(Debug, Clone)]
pub struct HirFunction {
    pub name: String,
    pub return_type: SourceFragment,
    pub is_async: bool,
    pub parameters: Vec<HirParameter>,
    pub scopes: Vec<HirScope>,
    pub labels: Vec<HirLabel>,
    pub body: HirBlock,
    pub span: SourceSpan,
}

/// An exact source fragment retained until semantic lowering needs structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFragment {
    pub text: String,
    pub span: SourceSpan,
}

/// A function parameter with declaration identity.
#[derive(Debug, Clone)]
pub struct HirParameter {
    pub id: DeclarationId,
    pub name: String,
    pub ty: SourceFragment,
    pub scope: ScopeId,
    pub span: SourceSpan,
}

/// A lexical scope and its parent relation.
#[derive(Debug, Clone)]
pub struct HirScope {
    pub id: ScopeId,
    pub parent: Option<ScopeId>,
    pub span: SourceSpan,
}

/// A resolved function label.
#[derive(Debug, Clone)]
pub struct HirLabel {
    pub id: LabelId,
    pub name: String,
    pub span: SourceSpan,
}

/// A lexical block whose statements execute in source order.
#[derive(Debug, Clone)]
pub struct HirBlock {
    pub scope: ScopeId,
    pub statements: Vec<HirStmt>,
    pub span: SourceSpan,
}

/// HIR statements required for CFG and scope-exit construction.
#[derive(Debug, Clone)]
pub enum HirStmt {
    Block(HirBlock),
    Declarations(Vec<HirDeclaration>),
    Expression(HirExpr),
    Defer(HirDefer),
    If {
        condition: HirExpr,
        consequence: Box<HirStmt>,
        alternative: Option<Box<HirStmt>>,
        span: SourceSpan,
    },
    While {
        condition: HirExpr,
        body: Box<HirStmt>,
        span: SourceSpan,
    },
    DoWhile {
        body: Box<HirStmt>,
        condition: HirExpr,
        span: SourceSpan,
    },
    For {
        initializer: Option<Box<HirStmt>>,
        condition: Option<Box<HirExpr>>,
        update: Option<Box<HirExpr>>,
        body: Box<HirStmt>,
        span: SourceSpan,
    },
    Switch {
        condition: HirExpr,
        body: Box<HirStmt>,
        span: SourceSpan,
    },
    Case {
        value: Option<HirExpr>,
        statements: Vec<HirStmt>,
        span: SourceSpan,
    },
    Return {
        value: Option<HirExpr>,
        span: SourceSpan,
    },
    Break(SourceSpan),
    Continue(SourceSpan),
    Goto {
        target: LabelId,
        name: String,
        span: SourceSpan,
    },
    Label {
        id: LabelId,
        name: String,
        statement: Box<HirStmt>,
        span: SourceSpan,
    },
    Source(SourceFragment),
    Empty(SourceSpan),
}

/// A local declaration with a stable identity.
#[derive(Debug, Clone)]
pub struct HirDeclaration {
    pub id: DeclarationId,
    pub name: String,
    pub ty: SourceFragment,
    pub scope: ScopeId,
    pub initializer: Option<HirExpr>,
    pub is_task: bool,
    pub span: SourceSpan,
}

/// A registered defer call and its lexical scope.
#[derive(Debug, Clone)]
pub struct HirDefer {
    pub scope: ScopeId,
    pub call: HirExpr,
    pub span: SourceSpan,
}

/// Source-backed expressions with explicit CR extension nodes.
#[derive(Debug, Clone)]
pub struct HirExpr {
    pub kind: HirExprKind,
    pub span: SourceSpan,
}

/// Expression structure needed to identify suspension and evaluation order.
#[derive(Debug, Clone)]
pub enum HirExprKind {
    Source(String),
    AwaitResultRef(AwaitSlotId),
    TaskRef {
        declaration: DeclarationId,
        name: String,
    },
    Await(Box<HirExpr>),
    Yield(Option<Box<HirExpr>>),
    AsyncCall {
        target: Option<FunctionId>,
        callee: String,
        result_type: SourceFragment,
        arguments: Vec<HirExpr>,
    },
    Binary {
        left: Box<HirExpr>,
        operator: String,
        right: Box<HirExpr>,
    },
    Conditional {
        condition: Box<HirExpr>,
        consequence: Box<HirExpr>,
        alternative: Box<HirExpr>,
    },
    Comma {
        left: Box<HirExpr>,
        right: Box<HirExpr>,
    },
    Assignment {
        left: Box<HirExpr>,
        operator: String,
        right: Box<HirExpr>,
    },
    Unary {
        operator: String,
        operand: Box<HirExpr>,
    },
    Call {
        function: Box<HirExpr>,
        arguments: Vec<HirExpr>,
    },
    Composite {
        source: String,
        extensions: Vec<HirExpr>,
    },
}

/// Converts Tree-sitter syntax into source-backed semantic HIR.
#[must_use]
pub fn build_hir(unit: &SyntaxUnit) -> HirUnit {
    let project_path = unit
        .path()
        .file_name()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| unit.path().to_path_buf());
    let symbols = build_local_async_symbol_index(unit, "");
    let mut hir = HirBuilder::new(unit, &symbols.index, &project_path).build();
    hir.diagnostics.extend(symbols.diagnostics);
    hir
}

/// Converts syntax into HIR using a project-level async symbol index.
#[must_use]
pub fn build_hir_with_symbol_index(
    unit: &SyntaxUnit,
    symbols: &AsyncSymbolIndex,
    project_path: &std::path::Path,
) -> HirUnit {
    HirBuilder::new(unit, symbols, project_path).build()
}

struct HirBuilder<'unit, 'symbols> {
    unit: &'unit SyntaxUnit,
    symbols: &'symbols AsyncSymbolIndex,
    project_path: &'symbols std::path::Path,
    functions: Vec<HirFunction>,
    diagnostics: Vec<Diagnostic>,
    next_declaration: u32,
    next_scope: u32,
}

impl<'unit, 'symbols> HirBuilder<'unit, 'symbols> {
    fn new(
        unit: &'unit SyntaxUnit,
        symbols: &'symbols AsyncSymbolIndex,
        project_path: &'symbols std::path::Path,
    ) -> Self {
        Self {
            unit,
            symbols,
            project_path,
            functions: Vec::new(),
            diagnostics: unit.diagnostics().to_vec(),
            next_declaration: 0,
            next_scope: 0,
        }
    }

    fn build(mut self) -> HirUnit {
        let root = self.unit.tree().root_node();
        for node in named_children(root) {
            if node.kind() == "function_definition"
                && function_needs_lowering(node)
                && let Some(function) = self.build_function(node)
            {
                self.functions.push(function);
            }
        }
        HirUnit {
            functions: self.functions,
            diagnostics: self.diagnostics,
        }
    }

    fn build_function(&mut self, node: Node<'_>) -> Option<HirFunction> {
        let declarator = self.required_field(node, "declarator")?;
        let body_node = self.required_field(node, "body")?;
        let type_node = self.required_field(node, "type")?;
        let name_node = declarator_identifier(declarator)?;
        let name = self.unit.text(name_node).to_owned();
        let is_async = named_children(node)
            .into_iter()
            .any(|child| child.kind() == "async_specifier");

        let labels = self.collect_labels(body_node);
        let label_ids = labels
            .iter()
            .map(|label| (label.name.clone(), label.id))
            .collect();
        let return_type = self.fragment(type_node);
        let mut function_builder = FunctionBuilder {
            unit: self.unit,
            diagnostics: &mut self.diagnostics,
            next_declaration: &mut self.next_declaration,
            next_scope: &mut self.next_scope,
            scopes: Vec::new(),
            label_ids,
            is_async,
            symbols: self.symbols,
            project_path: self.project_path,
        };
        let body = function_builder.build_block(body_node, None);
        let parameters = function_builder.build_parameters(declarator, body.scope);

        let mut function = HirFunction {
            name,
            return_type,
            is_async,
            parameters,
            scopes: function_builder.scopes,
            labels,
            body,
            span: self.unit.span(node),
        };
        resolve_task_references(&mut function);
        Some(function)
    }

    fn collect_labels(&mut self, body: Node<'_>) -> Vec<HirLabel> {
        let mut labels = Vec::new();
        visit_descendants(body, |node| {
            if node.kind() != "labeled_statement" {
                return;
            }
            let Some(label_node) = node.child_by_field_name("label") else {
                return;
            };
            let name = self.unit.text(label_node).to_owned();
            if labels.iter().any(|label: &HirLabel| label.name == name) {
                self.diagnostics.push(Diagnostic {
                    code: "CRS1004",
                    severity: DiagnosticSeverity::Error,
                    message: format!("duplicate label `{name}`"),
                    primary_span: self.unit.span(label_node),
                    related: Vec::new(),
                });
                return;
            }
            labels.push(HirLabel {
                id: LabelId(labels.len() as u32),
                name,
                span: self.unit.span(label_node),
            });
        });
        labels
    }

    fn required_field<'tree>(&mut self, node: Node<'tree>, field: &str) -> Option<Node<'tree>> {
        let child = node.child_by_field_name(field);
        if child.is_none() {
            self.diagnostics.push(Diagnostic {
                code: "CRS1000",
                severity: DiagnosticSeverity::Error,
                message: format!("missing required `{field}` syntax"),
                primary_span: self.unit.span(node),
                related: Vec::new(),
            });
        }
        child
    }

    fn fragment(&self, node: Node<'_>) -> SourceFragment {
        SourceFragment {
            text: self.unit.text(node).to_owned(),
            span: self.unit.span(node),
        }
    }
}

struct FunctionBuilder<'builder, 'unit> {
    unit: &'unit SyntaxUnit,
    diagnostics: &'builder mut Vec<Diagnostic>,
    next_declaration: &'builder mut u32,
    next_scope: &'builder mut u32,
    scopes: Vec<HirScope>,
    label_ids: HashMap<String, LabelId>,
    is_async: bool,
    symbols: &'builder AsyncSymbolIndex,
    project_path: &'builder std::path::Path,
}

impl FunctionBuilder<'_, '_> {
    fn build_parameters(&mut self, declarator: Node<'_>, scope: ScopeId) -> Vec<HirParameter> {
        let Some(parameters) = find_descendant(declarator, "parameter_list") else {
            return Vec::new();
        };
        named_children(parameters)
            .into_iter()
            .filter(|node| node.kind() == "parameter_declaration")
            .filter_map(|node| {
                let type_node = node.child_by_field_name("type")?;
                let declarator = node.child_by_field_name("declarator");
                if declarator.is_none() && self.unit.text(type_node) == "void" {
                    return None;
                }
                let name_node = declarator.and_then(declarator_identifier);
                let name = name_node
                    .map(|name| self.unit.text(name).to_owned())
                    .unwrap_or_else(|| format!("__cr_param_{}", *self.next_declaration));
                let mut ty = name_node
                    .and_then(|name_node| declarator.map(|declarator| (declarator, name_node)))
                    .map(|(declarator, name_node)| {
                        self.type_with_declarator(
                            type_node,
                            declarator,
                            name_node,
                            declaration_qualifiers(
                                &self.unit.source()[node.start_byte()..declarator.start_byte()],
                            ),
                        )
                    })
                    .unwrap_or_else(|| self.fragment(type_node));
                if let Some(array) = ty.text.find('[') {
                    ty.text = format!("{} *", ty.text[..array].trim_end());
                }
                Some(HirParameter {
                    id: self.fresh_declaration(),
                    name,
                    ty,
                    scope,
                    span: self.unit.span(node),
                })
            })
            .collect()
    }

    fn build_block(&mut self, node: Node<'_>, parent: Option<ScopeId>) -> HirBlock {
        let scope = self.fresh_scope(parent, node);
        let statements = named_children(node)
            .into_iter()
            .filter(|child| child.kind() != "comment")
            .flat_map(|child| self.build_statement(child, scope))
            .collect();
        HirBlock {
            scope,
            statements,
            span: self.unit.span(node),
        }
    }

    fn build_statement(&mut self, node: Node<'_>, scope: ScopeId) -> Vec<HirStmt> {
        let statement = match node.kind() {
            "compound_statement" => HirStmt::Block(self.build_block(node, Some(scope))),
            "declaration" => {
                return vec![HirStmt::Declarations(self.build_declarations(node, scope))];
            }
            "expression_statement" => node
                .named_child(0)
                .map(|expr| HirStmt::Expression(self.build_expression(expr)))
                .unwrap_or_else(|| HirStmt::Empty(self.unit.span(node))),
            "defer_statement" => {
                let Some(call) = node.child_by_field_name("call") else {
                    self.missing_field(node, "call");
                    return vec![HirStmt::Source(self.fragment(node))];
                };
                HirStmt::Defer(HirDefer {
                    scope,
                    call: self.build_expression(call),
                    span: self.unit.span(node),
                })
            }
            "return_statement" => HirStmt::Return {
                value: node.named_child(0).map(|expr| self.build_expression(expr)),
                span: self.unit.span(node),
            },
            "if_statement" => self.build_if(node, scope),
            "while_statement" => self.build_while(node, scope),
            "do_statement" => self.build_do_while(node, scope),
            "for_statement" => self.build_for(node, scope),
            "switch_statement" => self.build_switch(node, scope),
            "case_statement" => self.build_case(node, scope),
            "break_statement" => HirStmt::Break(self.unit.span(node)),
            "continue_statement" => HirStmt::Continue(self.unit.span(node)),
            "goto_statement" => self.build_goto(node),
            "labeled_statement" => self.build_label(node, scope),
            _ => {
                if contains_cr(node) {
                    self.diagnostics.push(Diagnostic {
                        code: "CRS1005",
                        severity: DiagnosticSeverity::Error,
                        message: format!(
                            "CR syntax inside unsupported `{}` control flow",
                            node.kind()
                        ),
                        primary_span: self.unit.span(node),
                        related: Vec::new(),
                    });
                }
                HirStmt::Source(self.fragment(node))
            }
        };
        vec![statement]
    }

    fn build_declarations(&mut self, node: Node<'_>, scope: ScopeId) -> Vec<HirDeclaration> {
        let Some(type_node) = node.child_by_field_name("type") else {
            self.missing_field(node, "type");
            return Vec::new();
        };
        let is_task = named_children(node)
            .into_iter()
            .any(|child| child.kind() == "async_specifier");
        if is_task && !self.is_async {
            self.diagnostics.push(Diagnostic {
                code: "CRS1006",
                severity: DiagnosticSeverity::Error,
                message: "an `__async` task binding is only valid inside an async function"
                    .to_owned(),
                primary_span: self.unit.span(node),
                related: Vec::new(),
            });
        }
        let mut declarations = Vec::new();
        let mut cursor = node.walk();
        for declarator in node.children_by_field_name("declarator", &mut cursor) {
            let (type_declarator, name_node, initializer) =
                if declarator.kind() == "init_declarator" {
                    let type_declarator = declarator.child_by_field_name("declarator");
                    (
                        type_declarator,
                        type_declarator.and_then(declarator_identifier),
                        declarator.child_by_field_name("value"),
                    )
                } else {
                    (Some(declarator), declarator_identifier(declarator), None)
                };
            let Some(name_node) = name_node else {
                self.missing_field(declarator, "declarator name");
                continue;
            };
            let initializer = initializer.map(|expr| self.build_expression(expr));
            if is_task
                && !matches!(
                    initializer.as_ref().map(|expression| &expression.kind),
                    Some(HirExprKind::AsyncCall { .. })
                )
            {
                self.diagnostics.push(Diagnostic {
                    code: "CRS1007",
                    severity: DiagnosticSeverity::Error,
                    message: "an `__async` task binding requires an async function call".to_owned(),
                    primary_span: self.unit.span(declarator),
                    related: Vec::new(),
                });
            }
            declarations.push(HirDeclaration {
                id: self.fresh_declaration(),
                name: self.unit.text(name_node).to_owned(),
                ty: type_declarator
                    .map(|type_declarator| {
                        self.type_with_declarator(
                            type_node,
                            type_declarator,
                            name_node,
                            declaration_qualifiers(
                                &self.unit.source()
                                    [node.start_byte()..type_declarator.start_byte()],
                            ),
                        )
                    })
                    .unwrap_or_else(|| self.fragment(type_node)),
                scope,
                initializer,
                is_task,
                span: self.unit.span(declarator),
            });
        }
        declarations
    }

    fn build_if(&mut self, node: Node<'_>, scope: ScopeId) -> HirStmt {
        let Some(condition) = node.child_by_field_name("condition") else {
            self.missing_field(node, "condition");
            return HirStmt::Source(self.fragment(node));
        };
        let Some(consequence) = node.child_by_field_name("consequence") else {
            self.missing_field(node, "consequence");
            return HirStmt::Source(self.fragment(node));
        };
        let alternative = node
            .child_by_field_name("alternative")
            .and_then(|else_clause| else_clause.named_child(0));
        HirStmt::If {
            condition: self.build_expression(condition),
            consequence: Box::new(self.one_statement(consequence, scope)),
            alternative: alternative
                .map(|statement| Box::new(self.one_statement(statement, scope))),
            span: self.unit.span(node),
        }
    }

    fn build_while(&mut self, node: Node<'_>, scope: ScopeId) -> HirStmt {
        let Some(condition) = node.child_by_field_name("condition") else {
            self.missing_field(node, "condition");
            return HirStmt::Source(self.fragment(node));
        };
        let Some(body) = node.child_by_field_name("body") else {
            self.missing_field(node, "body");
            return HirStmt::Source(self.fragment(node));
        };
        HirStmt::While {
            condition: self.build_expression(condition),
            body: Box::new(self.one_statement(body, scope)),
            span: self.unit.span(node),
        }
    }

    fn build_do_while(&mut self, node: Node<'_>, scope: ScopeId) -> HirStmt {
        let Some(body) = node.child_by_field_name("body") else {
            self.missing_field(node, "body");
            return HirStmt::Source(self.fragment(node));
        };
        let Some(condition) = node.child_by_field_name("condition") else {
            self.missing_field(node, "condition");
            return HirStmt::Source(self.fragment(node));
        };
        HirStmt::DoWhile {
            body: Box::new(self.one_statement(body, scope)),
            condition: self.build_expression(condition),
            span: self.unit.span(node),
        }
    }

    fn build_for(&mut self, node: Node<'_>, scope: ScopeId) -> HirStmt {
        let Some(body) = node.child_by_field_name("body") else {
            self.missing_field(node, "body");
            return HirStmt::Source(self.fragment(node));
        };
        HirStmt::For {
            initializer: node.child_by_field_name("initializer").map(|child| {
                if child.kind() == "declaration" {
                    Box::new(self.one_statement(child, scope))
                } else {
                    Box::new(HirStmt::Expression(self.build_expression(child)))
                }
            }),
            condition: node
                .child_by_field_name("condition")
                .map(|child| Box::new(self.build_expression(child))),
            update: node
                .child_by_field_name("update")
                .map(|child| Box::new(self.build_expression(child))),
            body: Box::new(self.one_statement(body, scope)),
            span: self.unit.span(node),
        }
    }

    fn build_switch(&mut self, node: Node<'_>, scope: ScopeId) -> HirStmt {
        let Some(condition) = node.child_by_field_name("condition") else {
            self.missing_field(node, "condition");
            return HirStmt::Source(self.fragment(node));
        };
        let Some(body) = node.child_by_field_name("body") else {
            self.missing_field(node, "body");
            return HirStmt::Source(self.fragment(node));
        };
        HirStmt::Switch {
            condition: self.build_expression(condition),
            body: Box::new(self.one_statement(body, scope)),
            span: self.unit.span(node),
        }
    }

    fn build_case(&mut self, node: Node<'_>, scope: ScopeId) -> HirStmt {
        let value_node = node.child_by_field_name("value");
        let value = value_node.map(|value| self.build_expression(value));
        if value.as_ref().is_some_and(expression_contains_extension) {
            self.diagnostics.push(Diagnostic {
                code: "CRS1008",
                severity: DiagnosticSeverity::Error,
                message: "a switch case value can't contain CR suspension syntax".to_owned(),
                primary_span: self.unit.span(node),
                related: Vec::new(),
            });
        }
        let statements = named_children(node)
            .into_iter()
            .filter(|child| value_node.is_none_or(|value| child.id() != value.id()))
            .filter(|child| child.kind() != "comment")
            .flat_map(|child| self.build_statement(child, scope))
            .collect();
        HirStmt::Case {
            value,
            statements,
            span: self.unit.span(node),
        }
    }

    fn build_goto(&mut self, node: Node<'_>) -> HirStmt {
        let Some(label) = node.child_by_field_name("label") else {
            self.missing_field(node, "label");
            return HirStmt::Source(self.fragment(node));
        };
        let name = self.unit.text(label).to_owned();
        let Some(target) = self.label_ids.get(&name).copied() else {
            self.diagnostics.push(Diagnostic {
                code: "CRS1003",
                severity: DiagnosticSeverity::Error,
                message: format!("unknown label `{name}`"),
                primary_span: self.unit.span(label),
                related: Vec::new(),
            });
            return HirStmt::Source(self.fragment(node));
        };
        HirStmt::Goto {
            target,
            name,
            span: self.unit.span(node),
        }
    }

    fn build_label(&mut self, node: Node<'_>, scope: ScopeId) -> HirStmt {
        let Some(label) = node.child_by_field_name("label") else {
            self.missing_field(node, "label");
            return HirStmt::Source(self.fragment(node));
        };
        let name = self.unit.text(label).to_owned();
        let Some(id) = self.label_ids.get(&name).copied() else {
            return HirStmt::Source(self.fragment(node));
        };
        let statement = named_children(node)
            .into_iter()
            .find(|child| child.id() != label.id());
        HirStmt::Label {
            id,
            name,
            statement: Box::new(
                statement
                    .map(|child| self.one_statement(child, scope))
                    .unwrap_or_else(|| HirStmt::Empty(self.unit.span(node))),
            ),
            span: self.unit.span(node),
        }
    }

    fn one_statement(&mut self, node: Node<'_>, scope: ScopeId) -> HirStmt {
        let mut statements = self.build_statement(node, scope);
        if statements.len() == 1 {
            statements.remove(0)
        } else {
            HirStmt::Source(self.fragment(node))
        }
    }

    fn build_expression(&mut self, node: Node<'_>) -> HirExpr {
        let kind = match node.kind() {
            "parenthesized_expression" => {
                return node
                    .named_child(0)
                    .map(|child| self.build_expression(child))
                    .unwrap_or_else(|| HirExpr {
                        kind: HirExprKind::Source(self.unit.text(node).to_owned()),
                        span: self.unit.span(node),
                    });
            }
            "await_expression" => {
                if !self.is_async {
                    self.extension_outside_async(node, "__await");
                }
                let argument = node
                    .child_by_field_name("argument")
                    .map(|child| self.build_expression(child));
                match argument {
                    Some(argument) => HirExprKind::Await(Box::new(argument)),
                    None => HirExprKind::Source(self.unit.text(node).to_owned()),
                }
            }
            "yield_expression" => {
                if !self.is_async {
                    self.extension_outside_async(node, "__yield");
                }
                HirExprKind::Yield(
                    node.child_by_field_name("value")
                        .map(|child| Box::new(self.build_expression(child))),
                )
            }
            "call_expression" => {
                let function = node
                    .child_by_field_name("function")
                    .map(|child| self.build_expression(child));
                let arguments = node
                    .child_by_field_name("arguments")
                    .map(named_children)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|child| child.kind() != "comment")
                    .map(|child| self.build_expression(child))
                    .collect();
                match function {
                    Some(function) => {
                        let resolved = match &function.kind {
                            HirExprKind::Source(name) => {
                                self.symbols.resolve(self.project_path, name.trim())
                            }
                            _ => None,
                        };
                        if let Some(resolved) = resolved {
                            let result_span = resolved
                                .symbol
                                .sites
                                .iter()
                                .find(|site| {
                                    site.kind
                                        == crate::symbol_index::AsyncSymbolSiteKind::Definition
                                })
                                .or_else(|| resolved.symbol.sites.first())
                                .map_or_else(|| self.unit.span(node), |site| site.span.clone());
                            HirExprKind::AsyncCall {
                                target: Some(resolved.symbol.id),
                                callee: resolved.symbol.name.clone(),
                                result_type: SourceFragment {
                                    text: resolved.symbol.result_type.clone(),
                                    span: result_span,
                                },
                                arguments,
                            }
                        } else {
                            HirExprKind::Call {
                                function: Box::new(function),
                                arguments,
                            }
                        }
                    }
                    None => HirExprKind::Source(self.unit.text(node).to_owned()),
                }
            }
            "binary_expression" => {
                let left = node
                    .child_by_field_name("left")
                    .map(|child| self.build_expression(child));
                let right = node
                    .child_by_field_name("right")
                    .map(|child| self.build_expression(child));
                let operator = node
                    .child_by_field_name("operator")
                    .map(|child| self.unit.text(child).to_owned());
                match (left, operator, right) {
                    (Some(left), Some(operator), Some(right)) => HirExprKind::Binary {
                        left: Box::new(left),
                        operator,
                        right: Box::new(right),
                    },
                    _ => HirExprKind::Source(self.unit.text(node).to_owned()),
                }
            }
            "conditional_expression" => {
                let condition = node
                    .child_by_field_name("condition")
                    .map(|child| self.build_expression(child));
                let consequence = node
                    .child_by_field_name("consequence")
                    .map(|child| self.build_expression(child));
                let alternative = node
                    .child_by_field_name("alternative")
                    .map(|child| self.build_expression(child));
                match (condition, consequence, alternative) {
                    (Some(condition), Some(consequence), Some(alternative)) => {
                        HirExprKind::Conditional {
                            condition: Box::new(condition),
                            consequence: Box::new(consequence),
                            alternative: Box::new(alternative),
                        }
                    }
                    _ => HirExprKind::Source(self.unit.text(node).to_owned()),
                }
            }
            "comma_expression" => {
                let left = node
                    .child_by_field_name("left")
                    .map(|child| self.build_expression(child));
                let right = node
                    .child_by_field_name("right")
                    .map(|child| self.build_expression(child));
                match (left, right) {
                    (Some(left), Some(right)) => HirExprKind::Comma {
                        left: Box::new(left),
                        right: Box::new(right),
                    },
                    _ => HirExprKind::Source(self.unit.text(node).to_owned()),
                }
            }
            "assignment_expression" => {
                let left = node
                    .child_by_field_name("left")
                    .map(|child| self.build_expression(child));
                let right = node
                    .child_by_field_name("right")
                    .map(|child| self.build_expression(child));
                let operator = node
                    .child_by_field_name("operator")
                    .map(|child| self.unit.text(child).to_owned());
                match (left, operator, right) {
                    (Some(left), Some(operator), Some(right)) => HirExprKind::Assignment {
                        left: Box::new(left),
                        operator,
                        right: Box::new(right),
                    },
                    _ => HirExprKind::Source(self.unit.text(node).to_owned()),
                }
            }
            _ if contains_cr(node) => HirExprKind::Composite {
                source: self.unit.text(node).to_owned(),
                extensions: named_children(node)
                    .into_iter()
                    .filter(|child| contains_cr(*child))
                    .map(|child| self.build_expression(child))
                    .collect(),
            },
            _ => HirExprKind::Source(self.unit.text(node).to_owned()),
        };
        HirExpr {
            kind,
            span: self.unit.span(node),
        }
    }

    fn fresh_declaration(&mut self) -> DeclarationId {
        let id = DeclarationId(*self.next_declaration);
        *self.next_declaration += 1;
        id
    }

    fn fresh_scope(&mut self, parent: Option<ScopeId>, node: Node<'_>) -> ScopeId {
        let id = ScopeId(*self.next_scope);
        *self.next_scope += 1;
        self.scopes.push(HirScope {
            id,
            parent,
            span: self.unit.span(node),
        });
        id
    }

    fn fragment(&self, node: Node<'_>) -> SourceFragment {
        SourceFragment {
            text: self.unit.text(node).to_owned(),
            span: self.unit.span(node),
        }
    }

    fn type_with_declarator(
        &self,
        type_node: Node<'_>,
        declarator: Node<'_>,
        name_node: Node<'_>,
        qualifiers: Vec<&str>,
    ) -> SourceFragment {
        let declarator_text = self.unit.text(declarator);
        let relative_start = name_node
            .start_byte()
            .saturating_sub(declarator.start_byte());
        let relative_end = name_node.end_byte().saturating_sub(declarator.start_byte());
        let mut abstract_declarator = String::with_capacity(declarator_text.len());
        abstract_declarator.push_str(&declarator_text[..relative_start]);
        abstract_declarator.push_str(&declarator_text[relative_end..]);
        let abstract_declarator = abstract_declarator.trim();
        let base = if qualifiers.is_empty() {
            self.unit.text(type_node).to_owned()
        } else {
            format!(
                "{} {}",
                qualifiers.join(" "),
                self.unit.text(type_node).trim()
            )
        };
        let text = if abstract_declarator.is_empty() {
            base
        } else {
            format!("{base} {abstract_declarator}")
        };
        SourceFragment {
            text,
            span: self.unit.span(type_node),
        }
    }

    fn missing_field(&mut self, node: Node<'_>, field: &str) {
        self.diagnostics.push(Diagnostic {
            code: "CRS1000",
            severity: DiagnosticSeverity::Error,
            message: format!("missing required `{field}` syntax"),
            primary_span: self.unit.span(node),
            related: Vec::new(),
        });
    }

    fn extension_outside_async(&mut self, node: Node<'_>, extension: &str) {
        self.diagnostics.push(Diagnostic {
            code: "CRS1001",
            severity: DiagnosticSeverity::Error,
            message: format!("`{extension}` is only valid inside an `__async` function"),
            primary_span: self.unit.span(node),
            related: Vec::new(),
        });
    }
}

fn declaration_qualifiers(prefix: &str) -> Vec<&'static str> {
    let words: std::collections::BTreeSet<_> = prefix
        .split(|character: char| !(character == '_' || character.is_ascii_alphanumeric()))
        .filter(|word| !word.is_empty())
        .collect();
    ["const", "volatile", "restrict", "_Atomic"]
        .into_iter()
        .filter(|qualifier| words.contains(qualifier))
        .collect()
}

#[derive(Clone)]
struct DeclarationCandidate {
    id: DeclarationId,
    name: String,
    scope: ScopeId,
    start_byte: usize,
    is_task: bool,
}

fn resolve_task_references(function: &mut HirFunction) {
    let mut declarations: Vec<_> = function
        .parameters
        .iter()
        .map(|parameter| DeclarationCandidate {
            id: parameter.id,
            name: parameter.name.clone(),
            scope: parameter.scope,
            start_byte: parameter.span.start_byte,
            is_task: false,
        })
        .collect();
    collect_declaration_candidates(&function.body.statements, &mut declarations);
    let mut scopes = vec![function.body.scope];
    resolve_block_task_references(&mut function.body, &mut scopes, &declarations);
}

fn collect_declaration_candidates(
    statements: &[HirStmt],
    declarations: &mut Vec<DeclarationCandidate>,
) {
    for statement in statements {
        match statement {
            HirStmt::Block(block) => {
                collect_declaration_candidates(&block.statements, declarations);
            }
            HirStmt::Declarations(items) => {
                declarations.extend(items.iter().map(|declaration| DeclarationCandidate {
                    id: declaration.id,
                    name: declaration.name.clone(),
                    scope: declaration.scope,
                    start_byte: declaration.span.start_byte,
                    is_task: declaration.is_task,
                }));
            }
            HirStmt::If {
                consequence,
                alternative,
                ..
            } => {
                collect_statement_candidates(consequence, declarations);
                if let Some(alternative) = alternative {
                    collect_statement_candidates(alternative, declarations);
                }
            }
            HirStmt::While { body, .. }
            | HirStmt::DoWhile { body, .. }
            | HirStmt::Switch { body, .. }
            | HirStmt::Label {
                statement: body, ..
            } => collect_statement_candidates(body, declarations),
            HirStmt::For {
                initializer, body, ..
            } => {
                if let Some(initializer) = initializer {
                    collect_statement_candidates(initializer, declarations);
                }
                collect_statement_candidates(body, declarations);
            }
            HirStmt::Case { statements, .. } => {
                collect_declaration_candidates(statements, declarations);
            }
            HirStmt::Expression(_)
            | HirStmt::Defer(_)
            | HirStmt::Return { .. }
            | HirStmt::Break(_)
            | HirStmt::Continue(_)
            | HirStmt::Goto { .. }
            | HirStmt::Source(_)
            | HirStmt::Empty(_) => {}
        }
    }
}

fn collect_statement_candidates(statement: &HirStmt, declarations: &mut Vec<DeclarationCandidate>) {
    collect_declaration_candidates(std::slice::from_ref(statement), declarations);
}

fn resolve_block_task_references(
    block: &mut HirBlock,
    scopes: &mut Vec<ScopeId>,
    declarations: &[DeclarationCandidate],
) {
    let pushed = scopes.last() != Some(&block.scope);
    if pushed {
        scopes.push(block.scope);
    }
    for statement in &mut block.statements {
        resolve_statement_task_references(statement, scopes, declarations);
    }
    if pushed {
        scopes.pop();
    }
}

fn resolve_statement_task_references(
    statement: &mut HirStmt,
    scopes: &mut Vec<ScopeId>,
    declarations: &[DeclarationCandidate],
) {
    match statement {
        HirStmt::Block(block) => resolve_block_task_references(block, scopes, declarations),
        HirStmt::Declarations(items) => {
            for declaration in items {
                if let Some(initializer) = &mut declaration.initializer {
                    resolve_expression_task_references(initializer, scopes, declarations);
                }
            }
        }
        HirStmt::Expression(expression) => {
            resolve_expression_task_references(expression, scopes, declarations);
        }
        HirStmt::Defer(defer) => {
            resolve_expression_task_references(&mut defer.call, scopes, declarations);
        }
        HirStmt::If {
            condition,
            consequence,
            alternative,
            ..
        } => {
            resolve_expression_task_references(condition, scopes, declarations);
            resolve_statement_task_references(consequence, scopes, declarations);
            if let Some(alternative) = alternative {
                resolve_statement_task_references(alternative, scopes, declarations);
            }
        }
        HirStmt::While {
            condition, body, ..
        } => {
            resolve_expression_task_references(condition, scopes, declarations);
            resolve_statement_task_references(body, scopes, declarations);
        }
        HirStmt::DoWhile {
            body, condition, ..
        } => {
            resolve_statement_task_references(body, scopes, declarations);
            resolve_expression_task_references(condition, scopes, declarations);
        }
        HirStmt::For {
            initializer,
            condition,
            update,
            body,
            ..
        } => {
            if let Some(initializer) = initializer {
                resolve_statement_task_references(initializer, scopes, declarations);
            }
            if let Some(condition) = condition {
                resolve_expression_task_references(condition, scopes, declarations);
            }
            if let Some(update) = update {
                resolve_expression_task_references(update, scopes, declarations);
            }
            resolve_statement_task_references(body, scopes, declarations);
        }
        HirStmt::Switch {
            condition, body, ..
        } => {
            resolve_expression_task_references(condition, scopes, declarations);
            resolve_statement_task_references(body, scopes, declarations);
        }
        HirStmt::Case {
            value, statements, ..
        } => {
            if let Some(value) = value {
                resolve_expression_task_references(value, scopes, declarations);
            }
            for statement in statements {
                resolve_statement_task_references(statement, scopes, declarations);
            }
        }
        HirStmt::Return { value, .. } => {
            if let Some(value) = value {
                resolve_expression_task_references(value, scopes, declarations);
            }
        }
        HirStmt::Label { statement, .. } => {
            resolve_statement_task_references(statement, scopes, declarations);
        }
        HirStmt::Break(_)
        | HirStmt::Continue(_)
        | HirStmt::Goto { .. }
        | HirStmt::Source(_)
        | HirStmt::Empty(_) => {}
    }
}

fn resolve_expression_task_references(
    expression: &mut HirExpr,
    scopes: &[ScopeId],
    declarations: &[DeclarationCandidate],
) {
    if let HirExprKind::Source(source) = &expression.kind {
        let name = source.trim();
        if is_identifier(name)
            && let Some(declaration) =
                resolve_declaration(name, expression.span.start_byte, scopes, declarations)
            && declaration.is_task
        {
            expression.kind = HirExprKind::TaskRef {
                declaration: declaration.id,
                name: name.to_owned(),
            };
            return;
        }
    }
    match &mut expression.kind {
        HirExprKind::Await(argument) => {
            resolve_expression_task_references(argument, scopes, declarations);
        }
        HirExprKind::Yield(value) => {
            if let Some(value) = value {
                resolve_expression_task_references(value, scopes, declarations);
            }
        }
        HirExprKind::AsyncCall { arguments, .. } => {
            for argument in arguments {
                resolve_expression_task_references(argument, scopes, declarations);
            }
        }
        HirExprKind::Unary { operand, .. } => {
            resolve_expression_task_references(operand, scopes, declarations);
        }
        HirExprKind::Binary { left, right, .. }
        | HirExprKind::Comma { left, right }
        | HirExprKind::Assignment { left, right, .. } => {
            resolve_expression_task_references(left, scopes, declarations);
            resolve_expression_task_references(right, scopes, declarations);
        }
        HirExprKind::Conditional {
            condition,
            consequence,
            alternative,
        } => {
            resolve_expression_task_references(condition, scopes, declarations);
            resolve_expression_task_references(consequence, scopes, declarations);
            resolve_expression_task_references(alternative, scopes, declarations);
        }
        HirExprKind::Call {
            function,
            arguments,
        } => {
            resolve_expression_task_references(function, scopes, declarations);
            for argument in arguments {
                resolve_expression_task_references(argument, scopes, declarations);
            }
        }
        HirExprKind::Composite { extensions, .. } => {
            for extension in extensions {
                resolve_expression_task_references(extension, scopes, declarations);
            }
        }
        HirExprKind::Source(_) | HirExprKind::AwaitResultRef(_) | HirExprKind::TaskRef { .. } => {}
    }
}

fn resolve_declaration<'declarations>(
    name: &str,
    reference_start: usize,
    scopes: &[ScopeId],
    declarations: &'declarations [DeclarationCandidate],
) -> Option<&'declarations DeclarationCandidate> {
    declarations
        .iter()
        .filter(|declaration| declaration.name == name && declaration.start_byte <= reference_start)
        .filter_map(|declaration| {
            scopes
                .iter()
                .rposition(|scope| *scope == declaration.scope)
                .map(|depth| (depth, declaration.start_byte, declaration))
        })
        .max_by_key(|(depth, start, _)| (*depth, *start))
        .map(|(_, _, declaration)| declaration)
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn function_needs_lowering(node: Node<'_>) -> bool {
    contains_kind(node, "async_specifier")
        || contains_kind(node, "await_expression")
        || contains_kind(node, "yield_expression")
        || contains_kind(node, "defer_statement")
}

fn expression_contains_extension(expression: &HirExpr) -> bool {
    match &expression.kind {
        HirExprKind::Await(_) | HirExprKind::Yield(_) => true,
        HirExprKind::Call {
            function,
            arguments,
        } => {
            expression_contains_extension(function)
                || arguments.iter().any(expression_contains_extension)
        }
        HirExprKind::AsyncCall { arguments, .. } => {
            arguments.iter().any(expression_contains_extension)
        }
        HirExprKind::Unary { operand, .. } => expression_contains_extension(operand),
        HirExprKind::Binary { left, right, .. } => {
            expression_contains_extension(left) || expression_contains_extension(right)
        }
        HirExprKind::Conditional {
            condition,
            consequence,
            alternative,
        } => {
            expression_contains_extension(condition)
                || expression_contains_extension(consequence)
                || expression_contains_extension(alternative)
        }
        HirExprKind::Comma { left, right } => {
            expression_contains_extension(left) || expression_contains_extension(right)
        }
        HirExprKind::Assignment { left, right, .. } => {
            expression_contains_extension(left) || expression_contains_extension(right)
        }
        HirExprKind::Composite { extensions, .. } => {
            extensions.iter().any(expression_contains_extension)
        }
        HirExprKind::Source(_) | HirExprKind::AwaitResultRef(_) | HirExprKind::TaskRef { .. } => {
            false
        }
    }
}

fn contains_cr(node: Node<'_>) -> bool {
    contains_kind(node, "await_expression")
        || contains_kind(node, "yield_expression")
        || contains_kind(node, "defer_statement")
}

fn contains_kind(node: Node<'_>, kind: &str) -> bool {
    let mut found = false;
    visit_descendants(node, |child| {
        if child.kind() == kind {
            found = true;
        }
    });
    found
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

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn visit_descendants(mut root: Node<'_>, mut visit: impl FnMut(Node<'_>)) {
    let mut cursor = root.walk();
    loop {
        root = cursor.node();
        visit(root);
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::symbol_index::{AsyncSymbolInput, build_async_symbol_index};
    use crate::syntax::SyntaxParser;

    use super::*;

    #[test]
    fn builds_identity_based_hir_for_async_and_sync_defer_functions() {
        let source = r#"
__async int fetch(int socket) {
    int bytes = __await read_socket(socket);
    __defer close_socket(socket);
    return bytes;
}

int guarded(int handle) {
    __defer close_handle(handle);
    return handle;
}

int untouched(void) { return 7; }
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("unit.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);

        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        assert_eq!(hir.functions.len(), 2);
        assert_eq!(hir.functions[0].name, "fetch");
        assert!(hir.functions[0].is_async);
        assert_eq!(hir.functions[1].name, "guarded");
        assert!(!hir.functions[1].is_async);
        assert_ne!(
            hir.functions[0].parameters[0].id,
            hir.functions[1].parameters[0].id
        );
    }

    #[test]
    fn assigns_nested_scope_and_label_identities() {
        let source = r#"
__async int flow(int ready) {
start:
    if (ready) {
        __defer release(ready);
        goto start;
    }
    return 0;
}
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("flow.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);

        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let function = &hir.functions[0];
        assert_eq!(function.labels.len(), 1);
        assert!(function.scopes.len() >= 2);
        assert!(
            function
                .scopes
                .iter()
                .any(|scope| scope.parent == Some(function.body.scope))
        );
    }

    #[test]
    fn diagnoses_await_outside_async_function() {
        let source = "int invalid(void) { return __await task; }";
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("invalid.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);

        assert!(
            hir.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CRS1001")
        );
    }

    #[test]
    fn resolves_async_task_binding_to_function_identity() {
        let source = r#"
__async int child(int value) { return value; }

__async int parent(int value) {
    __async int task = child(value);
    return __await task;
}
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("tasks.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);

        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let parent = &hir.functions[1];
        let HirStmt::Declarations(declarations) = &parent.body.statements[0] else {
            panic!("task declaration expected");
        };
        assert!(declarations[0].is_task);
        let Some(HirExprKind::AsyncCall {
            target: Some(target),
            callee,
            ..
        }) = declarations[0]
            .initializer
            .as_ref()
            .map(|expression| &expression.kind)
        else {
            panic!("identity-bearing async call expected");
        };
        assert_eq!(callee, "child");
        let child = build_local_async_symbol_index(&syntax, "")
            .index
            .resolve(Path::new("tasks.cr"), "child")
            .expect("child resolves")
            .symbol
            .id;
        assert_eq!(*target, child);

        let HirStmt::Return {
            value: Some(expression),
            ..
        } = &parent.body.statements[1]
        else {
            panic!("awaiting return expected");
        };
        let HirExprKind::Await(operand) = &expression.kind else {
            panic!("await expression expected");
        };
        assert!(matches!(
            &operand.kind,
            HirExprKind::TaskRef { declaration, name }
                if *declaration == declarations[0].id && name == "task"
        ));
    }

    #[test]
    fn resolves_header_declared_async_call_to_project_identity() {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let header = parser
            .parse(
                PathBuf::from("include/fetch.hr"),
                "__async int fetch(int socket);",
            )
            .expect("header parses");
        let caller = parser
            .parse(
                PathBuf::from("src/caller.cr"),
                "__async int caller(int socket) { return __await fetch(socket); }",
            )
            .expect("caller parses");
        let symbols = build_async_symbol_index(
            &[
                AsyncSymbolInput {
                    project_path: Path::new("include/fetch.hr"),
                    unit: &header,
                },
                AsyncSymbolInput {
                    project_path: Path::new("src/caller.cr"),
                    unit: &caller,
                },
            ],
            "",
        );

        assert!(symbols.diagnostics.is_empty(), "{:?}", symbols.diagnostics);
        let expected = symbols
            .index
            .resolve(Path::new("src/caller.cr"), "fetch")
            .expect("header declaration resolves")
            .symbol
            .id;
        let hir = build_hir_with_symbol_index(&caller, &symbols.index, Path::new("src/caller.cr"));

        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let HirStmt::Return {
            value: Some(expression),
            ..
        } = &hir.functions[0].body.statements[0]
        else {
            panic!("return expected");
        };
        let HirExprKind::Await(operand) = &expression.kind else {
            panic!("await expected");
        };
        assert!(matches!(
            &operand.kind,
            HirExprKind::AsyncCall {
                target: Some(target),
                callee,
                ..
            } if *target == expected && callee == "fetch"
        ));
    }

    #[test]
    fn resolves_shadowed_task_bindings_to_distinct_declarations() {
        let source = r#"
__async int child(int value) { return value; }

__async int parent(void) {
    __async int task = child(1);
    int first = __await task;
    {
        __async int task = child(2);
        int second = __await task;
    }
    return __await task;
}
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("shadowed.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);

        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let parent = &hir.functions[1];
        let HirStmt::Declarations(outer_task) = &parent.body.statements[0] else {
            panic!("outer task expected");
        };
        let HirStmt::Declarations(first) = &parent.body.statements[1] else {
            panic!("first result expected");
        };
        let HirStmt::Block(inner) = &parent.body.statements[2] else {
            panic!("inner block expected");
        };
        let HirStmt::Declarations(inner_task) = &inner.statements[0] else {
            panic!("inner task expected");
        };
        let HirStmt::Declarations(second) = &inner.statements[1] else {
            panic!("second result expected");
        };
        assert_ne!(outer_task[0].id, inner_task[0].id);
        assert_awaits_task(&first[0], outer_task[0].id);
        assert_awaits_task(&second[0], inner_task[0].id);

        let HirStmt::Return {
            value: Some(expression),
            ..
        } = &parent.body.statements[3]
        else {
            panic!("outer return expected");
        };
        assert_expression_awaits_task(expression, outer_task[0].id);
    }

    #[test]
    fn ordinary_local_shadow_prevents_outer_task_resolution() {
        let source = r#"
__async int child(int value) { return value; }

__async int parent(void) {
    __async int task = child(1);
    {
        int task = 0;
        return __await task;
    }
}
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("ordinary-shadow.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);

        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let parent = &hir.functions[1];
        let HirStmt::Block(inner) = &parent.body.statements[1] else {
            panic!("inner block expected");
        };
        let HirStmt::Return {
            value: Some(expression),
            ..
        } = &inner.statements[1]
        else {
            panic!("return expected");
        };
        let HirExprKind::Await(operand) = &expression.kind else {
            panic!("await expected");
        };
        assert!(matches!(&operand.kind, HirExprKind::Source(name) if name == "task"));
    }

    #[test]
    fn diagnoses_task_binding_without_indexed_async_target() {
        let source = r#"
__async int parent(void) {
    __async int task = missing();
    return __await task;
}
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("missing.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);

        assert!(
            hir.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CRS1007")
        );
    }

    #[test]
    fn await_result_reference_keeps_slot_identity_and_source_span() {
        let span = SourceSpan {
            path: PathBuf::from("identity.cr"),
            start_byte: 12,
            end_byte: 25,
            start: crate::syntax::SourcePoint { row: 1, column: 4 },
            end: crate::syntax::SourcePoint { row: 1, column: 17 },
        };
        let expression = HirExpr {
            kind: HirExprKind::AwaitResultRef(AwaitSlotId(9)),
            span: span.clone(),
        };

        assert!(matches!(
            expression.kind,
            HirExprKind::AwaitResultRef(AwaitSlotId(9))
        ));
        assert_eq!(expression.span, span);
    }

    fn assert_awaits_task(declaration: &HirDeclaration, expected: DeclarationId) {
        let expression = declaration
            .initializer
            .as_ref()
            .expect("result initializer expected");
        assert_expression_awaits_task(expression, expected);
    }

    fn assert_expression_awaits_task(expression: &HirExpr, expected: DeclarationId) {
        let HirExprKind::Await(operand) = &expression.kind else {
            panic!("await expected");
        };
        assert!(matches!(
            &operand.kind,
            HirExprKind::TaskRef { declaration, name }
                if *declaration == expected && name == "task"
        ));
    }
}
