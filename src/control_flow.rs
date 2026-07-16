//! Identity-based control-flow graph for transformed CR functions.

use std::collections::HashMap;

use crate::semantic::{
    AwaitSlotId, DeclarationId, HirBlock, HirDeclaration, HirDefer, HirExpr, HirExprKind,
    HirFunction, HirParameter, HirStmt, HirUnit, LabelId, ScopeId, SourceFragment,
};
use crate::syntax::{Diagnostic, DiagnosticSeverity, SourceSpan};

/// Stable basic-block identity within one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// Stable identity of one source defer registration site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CleanupId(pub u32);

/// Stable identity of one await suspension edge within a function CFG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AwaitEdgeId(pub u32);

/// CFG output for all transformed functions in one translation unit.
#[derive(Debug, Clone)]
pub struct CfgUnit {
    pub functions: Vec<CfgFunction>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A function CFG with explicit scope stacks on blocks and edges.
#[derive(Debug, Clone)]
pub struct CfgFunction {
    pub name: String,
    pub return_type: SourceFragment,
    pub is_async: bool,
    pub parameters: Vec<HirParameter>,
    pub entry: BlockId,
    pub blocks: Vec<BasicBlock>,
    pub span: SourceSpan,
    pub body_span: SourceSpan,
}

/// A basic block containing linear instructions and one terminator.
#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub id: BlockId,
    pub scope_stack: Vec<ScopeId>,
    pub instructions: Vec<CfgInstruction>,
    pub terminator: CfgTerminator,
}

/// Linear operations that don't select the next block.
#[derive(Debug, Clone)]
pub enum CfgInstruction {
    Source(SourceFragment),
    Declaration(HirDeclaration),
    AssignAwaitResult {
        destination: DeclarationId,
        slot: AwaitSlotId,
        span: SourceSpan,
    },
    AssignExpression {
        destination: DeclarationId,
        expression: HirExpr,
        span: SourceSpan,
    },
    AssignExpressionSlot {
        slot: AwaitSlotId,
        expression: HirExpr,
        ty: SourceFragment,
        span: SourceSpan,
    },
    Evaluate(HirExpr),
    RegisterDefer(HirDefer),
    PushCleanup(CleanupRegistration),
    RunCleanups {
        exited_scopes: Vec<ScopeId>,
    },
}

/// A validated, dynamically activated cleanup call.
#[derive(Debug, Clone)]
pub struct CleanupRegistration {
    pub id: CleanupId,
    pub scope: ScopeId,
    pub function: SourceFragment,
    pub arguments: Vec<HirExpr>,
    pub span: SourceSpan,
}

/// One explicit non-default switch dispatch edge.
#[derive(Debug, Clone)]
pub struct CfgSwitchCase {
    pub value: HirExpr,
    pub edge: CfgEdge,
}

/// A value consumed by a branch, yield, or return terminator.
#[derive(Debug, Clone)]
pub enum CfgValue {
    Expression(Box<HirExpr>),
    AwaitResult(AwaitSlotId),
}

/// The single control-flow decision at the end of a block.
#[derive(Debug, Clone)]
pub enum CfgTerminator {
    Open,
    Goto(CfgEdge),
    Branch {
        condition: HirExpr,
        consequence: CfgEdge,
        alternative: CfgEdge,
    },
    Switch {
        expression: HirExpr,
        cases: Vec<CfgSwitchCase>,
        default: CfgEdge,
    },
    Suspend {
        edge: AwaitEdgeId,
        operand: HirExpr,
        slot: AwaitSlotId,
        continuation: CfgEdge,
        span: SourceSpan,
    },
    Yield {
        value: Option<CfgValue>,
        continuation: CfgEdge,
        span: SourceSpan,
    },
    Return {
        value: Option<CfgValue>,
        span: SourceSpan,
    },
    Unreachable,
}

/// A typed control-flow edge with both lexical endpoints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgEdge {
    pub target: BlockId,
    pub kind: EdgeKind,
    pub span: SourceSpan,
    pub source_scopes: Vec<ScopeId>,
    pub target_scopes: Vec<ScopeId>,
}

/// Why an edge exists, used to validate user jumps separately from structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    Fallthrough,
    ScopeEntry,
    Branch,
    Loop,
    Break,
    Continue,
    UserGoto,
    Resume,
    Cleanup,
}

/// Returns every semantic successor edge in deterministic terminator order.
#[must_use]
pub fn successor_edges(terminator: &CfgTerminator) -> Vec<&CfgEdge> {
    match terminator {
        CfgTerminator::Goto(edge) => vec![edge],
        CfgTerminator::Branch {
            consequence,
            alternative,
            ..
        } => vec![consequence, alternative],
        CfgTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| &case.edge)
            .chain(std::iter::once(default))
            .collect(),
        CfgTerminator::Suspend { continuation, .. } | CfgTerminator::Yield { continuation, .. } => {
            vec![continuation]
        }
        CfgTerminator::Open | CfgTerminator::Return { .. } | CfgTerminator::Unreachable => {
            Vec::new()
        }
    }
}

/// Returns mutable semantic successor edges in deterministic terminator order.
#[must_use]
pub fn successor_edges_mut(terminator: &mut CfgTerminator) -> Vec<&mut CfgEdge> {
    match terminator {
        CfgTerminator::Goto(edge) => vec![edge],
        CfgTerminator::Branch {
            consequence,
            alternative,
            ..
        } => vec![consequence, alternative],
        CfgTerminator::Switch { cases, default, .. } => {
            let mut edges: Vec<_> = cases.iter_mut().map(|case| &mut case.edge).collect();
            edges.push(default);
            edges
        }
        CfgTerminator::Suspend { continuation, .. } | CfgTerminator::Yield { continuation, .. } => {
            vec![continuation]
        }
        CfgTerminator::Open | CfgTerminator::Return { .. } | CfgTerminator::Unreachable => {
            Vec::new()
        }
    }
}

/// Returns every semantic successor block in deterministic terminator order.
#[must_use]
pub fn successor_blocks(terminator: &CfgTerminator) -> Vec<BlockId> {
    successor_edges(terminator)
        .into_iter()
        .map(|edge| edge.target)
        .collect()
}

/// Builds scoped control flow without mutating the source-backed HIR.
#[must_use]
pub fn build_cfg(hir: &HirUnit) -> CfgUnit {
    let mut diagnostics = hir.diagnostics.clone();
    let functions = hir
        .functions
        .iter()
        .map(|function| FunctionCfgBuilder::new(function, &mut diagnostics).build())
        .collect();
    CfgUnit {
        functions,
        diagnostics,
    }
}

#[derive(Clone)]
struct JumpTarget {
    block: BlockId,
    scopes: Vec<ScopeId>,
}

struct LoopTargets {
    break_target: JumpTarget,
    continue_target: JumpTarget,
}

#[derive(Clone)]
struct SwitchCaseBody {
    value: Option<HirExpr>,
    statements: Vec<HirStmt>,
    span: SourceSpan,
}

fn flatten_switch_cases(statements: &[HirStmt], cases: &mut Vec<SwitchCaseBody>) {
    for statement in statements {
        match statement {
            HirStmt::Case {
                value,
                statements,
                span,
            } => flatten_one_switch_case(value.clone(), statements, span.clone(), cases),
            HirStmt::Block(block) => flatten_switch_cases(&block.statements, cases),
            _ => {}
        }
    }
}

fn flatten_one_switch_case(
    value: Option<HirExpr>,
    statements: &[HirStmt],
    span: SourceSpan,
    cases: &mut Vec<SwitchCaseBody>,
) {
    let split = statements
        .iter()
        .position(|statement| matches!(statement, HirStmt::Case { .. }))
        .unwrap_or(statements.len());
    cases.push(SwitchCaseBody {
        value,
        statements: statements[..split].to_vec(),
        span,
    });
    flatten_switch_cases(&statements[split..], cases);
}

struct FunctionCfgBuilder<'function, 'diagnostics> {
    function: &'function HirFunction,
    diagnostics: &'diagnostics mut Vec<Diagnostic>,
    blocks: Vec<BasicBlock>,
    current: Option<BlockId>,
    next_await_slot: u32,
    label_targets: HashMap<LabelId, JumpTarget>,
    loop_targets: Vec<LoopTargets>,
    switch_break_targets: Vec<JumpTarget>,
}

impl<'function, 'diagnostics> FunctionCfgBuilder<'function, 'diagnostics> {
    fn new(
        function: &'function HirFunction,
        diagnostics: &'diagnostics mut Vec<Diagnostic>,
    ) -> Self {
        Self {
            function,
            diagnostics,
            blocks: Vec::new(),
            current: None,
            next_await_slot: 0,
            label_targets: HashMap::new(),
            loop_targets: Vec::new(),
            switch_break_targets: Vec::new(),
        }
    }

    fn build(mut self) -> CfgFunction {
        let root_scopes = self.scope_stack(self.function.body.scope);
        self.allocate_label_targets(&self.function.body, &root_scopes);
        let entry = self.new_block(root_scopes.clone());
        self.current = Some(entry);
        self.build_statements(&self.function.body.statements, &root_scopes);
        if let Some(current) = self.current {
            self.block_mut(current).terminator = CfgTerminator::Return {
                value: None,
                span: self.function.body.span.clone(),
            };
        }
        for block in &mut self.blocks {
            if matches!(block.terminator, CfgTerminator::Open) {
                block.terminator = CfgTerminator::Unreachable;
            }
        }
        assign_await_edge_ids(&mut self.blocks);
        CfgFunction {
            name: self.function.name.clone(),
            return_type: self.function.return_type.clone(),
            is_async: self.function.is_async,
            parameters: self.function.parameters.clone(),
            entry,
            blocks: self.blocks,
            span: self.function.span.clone(),
            body_span: self.function.body.span.clone(),
        }
    }

    fn build_statements(&mut self, statements: &[HirStmt], scopes: &[ScopeId]) {
        for statement in statements {
            if self.current.is_none() && !matches!(statement, HirStmt::Label { .. }) {
                self.current = Some(self.new_block(scopes.to_vec()));
            }
            self.build_statement(statement, scopes);
        }
    }

    fn build_statement(&mut self, statement: &HirStmt, scopes: &[ScopeId]) {
        match statement {
            HirStmt::Block(block) => self.build_nested_block(block, scopes),
            HirStmt::Declarations(declarations) => {
                for declaration in declarations {
                    self.build_declaration(declaration, scopes);
                }
            }
            HirStmt::Expression(expression) => self.build_expression_stmt(expression, scopes),
            HirStmt::Defer(defer) => {
                self.emit(CfgInstruction::RegisterDefer(defer.clone()));
            }
            HirStmt::If {
                condition,
                consequence,
                alternative,
                ..
            } => self.build_if(condition, consequence, alternative.as_deref(), scopes),
            HirStmt::While {
                condition, body, ..
            } => self.build_while(condition, body, scopes),
            HirStmt::DoWhile {
                body, condition, ..
            } => self.build_do_while(body, condition, scopes),
            HirStmt::For {
                initializer,
                condition,
                update,
                body,
                ..
            } => self.build_for(
                initializer.as_deref(),
                condition.as_deref(),
                update.as_deref(),
                body,
                scopes,
            ),
            HirStmt::Switch {
                condition, body, ..
            } => self.build_switch(condition, body, scopes),
            HirStmt::Case { span, .. } => {
                self.control_error("CRC2007", "case label appears outside a switch", span);
            }
            HirStmt::Return { value, span } => {
                self.build_return(value.as_ref(), span, scopes);
            }
            HirStmt::Break(span) => self.build_break(span),
            HirStmt::Continue(span) => self.build_continue(span),
            HirStmt::Goto { target, span, .. } => {
                let Some(jump_target) = self.label_targets.get(target).cloned() else {
                    self.control_error("CRC2003", "goto target has no CFG block", span);
                    return;
                };
                self.terminate_goto(jump_target, EdgeKind::UserGoto, span);
            }
            HirStmt::Label { id, statement, .. } => {
                self.build_label(*id, statement);
            }
            HirStmt::Source(fragment) => self.emit(CfgInstruction::Source(fragment.clone())),
            HirStmt::Empty(_) => {}
        }
    }

    fn build_nested_block(&mut self, block: &HirBlock, parent_scopes: &[ScopeId]) {
        let mut scopes = parent_scopes.to_vec();
        if scopes.last() != Some(&block.scope) {
            scopes.push(block.scope);
        }
        let entry = self.new_block(scopes.clone());
        self.link_current(entry, &scopes);
        self.current = Some(entry);
        self.build_statements(&block.statements, &scopes);
        let after = self.new_block(parent_scopes.to_vec());
        self.link_current(after, parent_scopes);
        self.current = Some(after);
    }

    fn build_declaration(&mut self, declaration: &HirDeclaration, scopes: &[ScopeId]) {
        if let Some(initializer) = &declaration.initializer
            && expression_contains_await(initializer)
            && !matches!(initializer.kind, HirExprKind::Await(_))
        {
            let mut declaration_without_initializer = declaration.clone();
            declaration_without_initializer.initializer = None;
            self.emit(CfgInstruction::Declaration(declaration_without_initializer));
            let expression =
                self.lower_nested_awaits_typed(initializer, scopes, Some(declaration.ty.clone()));
            self.emit(CfgInstruction::AssignExpression {
                destination: declaration.id,
                expression,
                span: declaration.span.clone(),
            });
            return;
        }
        match declaration.initializer.clone() {
            Some(HirExpr {
                kind: HirExprKind::Await(operand),
                span,
            }) => {
                let mut declaration_without_initializer = declaration.clone();
                declaration_without_initializer.initializer = None;
                self.emit(CfgInstruction::Declaration(declaration_without_initializer));
                let slot = self.fresh_await_slot();
                let suspend_block = self.new_block(scopes.to_vec());
                self.link_current(suspend_block, scopes);
                self.current = Some(suspend_block);
                let continuation = self.new_block(scopes.to_vec());
                self.terminate_suspend(*operand, slot, continuation, scopes, span);
                self.current = Some(continuation);
                self.emit(CfgInstruction::AssignAwaitResult {
                    destination: declaration.id,
                    slot,
                    span: declaration.span.clone(),
                });
            }
            _ => self.emit(CfgInstruction::Declaration(declaration.clone())),
        }
    }

    fn build_expression_stmt(&mut self, expression: &HirExpr, scopes: &[ScopeId]) {
        match &expression.kind {
            HirExprKind::Await(operand) => {
                let slot = self.fresh_await_slot();
                let suspend_block = self.new_block(scopes.to_vec());
                self.link_current(suspend_block, scopes);
                self.current = Some(suspend_block);
                let continuation = self.new_block(scopes.to_vec());
                self.terminate_suspend(
                    *operand.clone(),
                    slot,
                    continuation,
                    scopes,
                    expression.span.clone(),
                );
                self.current = Some(continuation);
            }
            HirExprKind::Yield(value) => {
                let continuation = self.new_block(scopes.to_vec());
                let value = value
                    .as_ref()
                    .map(|value| CfgValue::Expression(value.clone()));
                self.terminate_yield(value, continuation, scopes, expression.span.clone());
                self.current = Some(continuation);
            }
            _ if expression_contains_await(expression) => {
                let expression = self.lower_nested_awaits(expression, scopes);
                self.emit(CfgInstruction::Evaluate(expression));
            }
            _ => self.emit(CfgInstruction::Evaluate(expression.clone())),
        }
    }

    fn build_return(&mut self, value: Option<&HirExpr>, span: &SourceSpan, scopes: &[ScopeId]) {
        let return_value = match value {
            Some(HirExpr {
                kind: HirExprKind::Await(operand),
                span: await_span,
            }) => {
                let slot = self.fresh_await_slot();
                let suspend_block = self.new_block(scopes.to_vec());
                self.link_current(suspend_block, scopes);
                self.current = Some(suspend_block);
                let continuation = self.new_block(scopes.to_vec());
                self.terminate_suspend(
                    *operand.clone(),
                    slot,
                    continuation,
                    scopes,
                    await_span.clone(),
                );
                self.current = Some(continuation);
                Some(CfgValue::AwaitResult(slot))
            }
            Some(value) if expression_contains_await(value) => Some(CfgValue::Expression(
                Box::new(self.lower_nested_awaits_typed(
                    value,
                    scopes,
                    Some(self.function.return_type.clone()),
                )),
            )),
            Some(value) => Some(CfgValue::Expression(Box::new(value.clone()))),
            None => None,
        };
        if let Some(current) = self.current.take() {
            self.block_mut(current).terminator = CfgTerminator::Return {
                value: return_value,
                span: span.clone(),
            };
        }
    }

    fn build_if(
        &mut self,
        condition: &HirExpr,
        consequence: &HirStmt,
        alternative: Option<&HirStmt>,
        scopes: &[ScopeId],
    ) {
        let condition = self.lower_nested_awaits(condition, scopes);
        let then_block = self.new_block(scopes.to_vec());
        let else_block = self.new_block(scopes.to_vec());
        let merge_block = self.new_block(scopes.to_vec());
        if let Some(current) = self.current.take() {
            let source_scopes = self.block(current).scope_stack.clone();
            self.block_mut(current).terminator = CfgTerminator::Branch {
                condition: condition.clone(),
                consequence: CfgEdge {
                    target: then_block,
                    kind: EdgeKind::Branch,
                    span: condition.span.clone(),
                    source_scopes: source_scopes.clone(),
                    target_scopes: scopes.to_vec(),
                },
                alternative: CfgEdge {
                    target: else_block,
                    kind: EdgeKind::Branch,
                    span: condition.span.clone(),
                    source_scopes,
                    target_scopes: scopes.to_vec(),
                },
            };
        }

        self.current = Some(then_block);
        self.build_statement(consequence, scopes);
        self.link_current(merge_block, scopes);

        self.current = Some(else_block);
        if let Some(alternative) = alternative {
            self.build_statement(alternative, scopes);
        }
        self.link_current(merge_block, scopes);
        self.current = Some(merge_block);
    }

    fn build_while(&mut self, condition: &HirExpr, body: &HirStmt, scopes: &[ScopeId]) {
        let condition_block = self.new_block(scopes.to_vec());
        let body_block = self.new_block(scopes.to_vec());
        let after_block = self.new_block(scopes.to_vec());
        self.link_current(condition_block, scopes);
        self.current = Some(condition_block);
        let condition = self.lower_nested_awaits(condition, scopes);
        let branch_block = self.current.take().unwrap_or(condition_block);
        let condition_scopes = self.block(branch_block).scope_stack.clone();
        self.block_mut(branch_block).terminator = CfgTerminator::Branch {
            condition: condition.clone(),
            consequence: CfgEdge {
                target: body_block,
                kind: EdgeKind::Branch,
                span: condition.span.clone(),
                source_scopes: condition_scopes.clone(),
                target_scopes: scopes.to_vec(),
            },
            alternative: CfgEdge {
                target: after_block,
                kind: EdgeKind::Branch,
                span: condition.span.clone(),
                source_scopes: condition_scopes,
                target_scopes: scopes.to_vec(),
            },
        };
        self.loop_targets.push(LoopTargets {
            break_target: JumpTarget {
                block: after_block,
                scopes: scopes.to_vec(),
            },
            continue_target: JumpTarget {
                block: condition_block,
                scopes: scopes.to_vec(),
            },
        });
        self.current = Some(body_block);
        self.build_statement(body, scopes);
        self.link_current(condition_block, scopes);
        self.loop_targets.pop();
        self.current = Some(after_block);
    }

    fn build_do_while(&mut self, body: &HirStmt, condition: &HirExpr, scopes: &[ScopeId]) {
        let body_block = self.new_block(scopes.to_vec());
        let condition_block = self.new_block(scopes.to_vec());
        let after_block = self.new_block(scopes.to_vec());
        self.link_current(body_block, scopes);
        self.loop_targets.push(LoopTargets {
            break_target: JumpTarget {
                block: after_block,
                scopes: scopes.to_vec(),
            },
            continue_target: JumpTarget {
                block: condition_block,
                scopes: scopes.to_vec(),
            },
        });
        self.current = Some(body_block);
        self.build_statement(body, scopes);
        self.link_current(condition_block, scopes);
        self.current = Some(condition_block);
        let condition = self.lower_nested_awaits(condition, scopes);
        let branch_block = self.current.take().unwrap_or(condition_block);
        let condition_scopes = self.block(branch_block).scope_stack.clone();
        self.block_mut(branch_block).terminator = CfgTerminator::Branch {
            condition: condition.clone(),
            consequence: CfgEdge {
                target: body_block,
                kind: EdgeKind::Loop,
                span: condition.span.clone(),
                source_scopes: condition_scopes.clone(),
                target_scopes: scopes.to_vec(),
            },
            alternative: CfgEdge {
                target: after_block,
                kind: EdgeKind::Branch,
                span: condition.span.clone(),
                source_scopes: condition_scopes,
                target_scopes: scopes.to_vec(),
            },
        };
        self.loop_targets.pop();
        self.current = Some(after_block);
    }

    fn build_for(
        &mut self,
        initializer: Option<&HirStmt>,
        condition: Option<&HirExpr>,
        update: Option<&HirExpr>,
        body: &HirStmt,
        scopes: &[ScopeId],
    ) {
        if let Some(initializer) = initializer {
            self.build_statement(initializer, scopes);
        }
        let condition_block = self.new_block(scopes.to_vec());
        let body_block = self.new_block(scopes.to_vec());
        let update_block = self.new_block(scopes.to_vec());
        let after_block = self.new_block(scopes.to_vec());
        self.link_current(condition_block, scopes);

        if let Some(condition) = condition {
            self.current = Some(condition_block);
            let condition = self.lower_nested_awaits(condition, scopes);
            let branch_block = self.current.take().unwrap_or(condition_block);
            let condition_scopes = self.block(branch_block).scope_stack.clone();
            self.block_mut(branch_block).terminator = CfgTerminator::Branch {
                condition: condition.clone(),
                consequence: CfgEdge {
                    target: body_block,
                    kind: EdgeKind::Branch,
                    span: condition.span.clone(),
                    source_scopes: condition_scopes.clone(),
                    target_scopes: scopes.to_vec(),
                },
                alternative: CfgEdge {
                    target: after_block,
                    kind: EdgeKind::Branch,
                    span: condition.span.clone(),
                    source_scopes: condition_scopes,
                    target_scopes: scopes.to_vec(),
                },
            };
        } else {
            self.set_goto(condition_block, body_block, scopes, EdgeKind::Loop);
        }

        self.loop_targets.push(LoopTargets {
            break_target: JumpTarget {
                block: after_block,
                scopes: scopes.to_vec(),
            },
            continue_target: JumpTarget {
                block: update_block,
                scopes: scopes.to_vec(),
            },
        });
        self.current = Some(body_block);
        self.build_statement(body, scopes);
        self.link_current(update_block, scopes);
        self.current = Some(update_block);
        if let Some(update) = update {
            let update = self.lower_nested_awaits(update, scopes);
            self.emit(CfgInstruction::Evaluate(update));
        }
        self.link_current(condition_block, scopes);
        self.loop_targets.pop();
        self.current = Some(after_block);
    }

    fn build_switch(&mut self, condition: &HirExpr, body: &HirStmt, scopes: &[ScopeId]) {
        let condition = self.lower_nested_awaits(condition, scopes);
        let mut cases = Vec::new();
        let case_scopes = if let HirStmt::Block(block) = body {
            let mut case_scopes = scopes.to_vec();
            case_scopes.push(block.scope);
            flatten_switch_cases(&block.statements, &mut cases);
            case_scopes
        } else {
            flatten_switch_cases(std::slice::from_ref(body), &mut cases);
            scopes.to_vec()
        };
        let after_block = self.new_block(scopes.to_vec());
        if cases.is_empty() {
            self.emit(CfgInstruction::Evaluate(condition.clone()));
            self.link_current(after_block, scopes);
            self.current = Some(after_block);
            return;
        }

        let case_blocks: Vec<_> = cases
            .iter()
            .map(|_| self.new_block(case_scopes.clone()))
            .collect();
        if let Some(current) = self.current.take() {
            let source_scopes = self.block(current).scope_stack.clone();
            let mut dispatch_cases = Vec::new();
            let mut default_target = after_block;
            for (index, case) in cases.iter().enumerate() {
                if let Some(value) = &case.value {
                    dispatch_cases.push(CfgSwitchCase {
                        value: value.clone(),
                        edge: CfgEdge {
                            target: case_blocks[index],
                            kind: EdgeKind::Branch,
                            span: case.span.clone(),
                            source_scopes: source_scopes.clone(),
                            target_scopes: case_scopes.clone(),
                        },
                    });
                } else {
                    default_target = case_blocks[index];
                }
            }
            self.block_mut(current).terminator = CfgTerminator::Switch {
                expression: condition.clone(),
                cases: dispatch_cases,
                default: CfgEdge {
                    target: default_target,
                    kind: EdgeKind::Branch,
                    span: condition.span.clone(),
                    source_scopes,
                    target_scopes: if default_target == after_block {
                        scopes.to_vec()
                    } else {
                        case_scopes.clone()
                    },
                },
            };
        }
        self.switch_break_targets.push(JumpTarget {
            block: after_block,
            scopes: scopes.to_vec(),
        });
        for (index, case) in cases.iter().enumerate() {
            self.current = Some(case_blocks[index]);
            self.build_statements(&case.statements, &case_scopes);
            if let Some(next) = case_blocks.get(index + 1) {
                self.link_current(*next, &case_scopes);
            } else {
                self.link_current(after_block, scopes);
            }
        }
        self.switch_break_targets.pop();
        self.current = Some(after_block);
    }

    fn build_break(&mut self, span: &SourceSpan) {
        let target = self
            .loop_targets
            .last()
            .map(|targets| targets.break_target.clone())
            .or_else(|| self.switch_break_targets.last().cloned());
        if let Some(target) = target {
            self.terminate_goto(target, EdgeKind::Break, span);
        } else {
            self.control_error("CRC2001", "`break` has no enclosing loop or switch", span);
        }
    }

    fn build_continue(&mut self, span: &SourceSpan) {
        if let Some(target) = self
            .loop_targets
            .last()
            .map(|targets| targets.continue_target.clone())
        {
            self.terminate_goto(target, EdgeKind::Continue, span);
        } else {
            self.control_error("CRC2002", "`continue` has no enclosing loop", span);
        }
    }

    fn build_label(&mut self, id: LabelId, statement: &HirStmt) {
        let Some(target) = self.label_targets.get(&id).cloned() else {
            return;
        };
        self.link_current(target.block, &target.scopes);
        self.current = Some(target.block);
        self.build_statement(statement, &target.scopes);
    }

    fn terminate_suspend(
        &mut self,
        operand: HirExpr,
        slot: AwaitSlotId,
        continuation: BlockId,
        target_scopes: &[ScopeId],
        span: SourceSpan,
    ) {
        if let Some(current) = self.current.take() {
            let source_scopes = self.block(current).scope_stack.clone();
            self.block_mut(current).terminator = CfgTerminator::Suspend {
                edge: AwaitEdgeId(u32::MAX),
                operand,
                slot,
                continuation: CfgEdge {
                    target: continuation,
                    kind: EdgeKind::Resume,
                    span: span.clone(),
                    source_scopes,
                    target_scopes: target_scopes.to_vec(),
                },
                span,
            };
        }
    }

    fn lower_nested_awaits(&mut self, expression: &HirExpr, scopes: &[ScopeId]) -> HirExpr {
        self.lower_nested_awaits_typed(expression, scopes, None)
    }

    fn lower_nested_awaits_typed(
        &mut self,
        expression: &HirExpr,
        scopes: &[ScopeId],
        expected_type: Option<SourceFragment>,
    ) -> HirExpr {
        if !expression_contains_await(expression) {
            return expression.clone();
        }
        if let HirExprKind::Binary {
            left,
            operator,
            right,
        } = &expression.kind
            && matches!(operator.as_str(), "&&" | "||")
        {
            return self.lower_short_circuit(left, operator, right, &expression.span, scopes);
        }
        if let HirExprKind::Conditional {
            condition,
            consequence,
            alternative,
        } = &expression.kind
        {
            return self.lower_conditional(
                condition,
                consequence,
                alternative,
                &expression.span,
                scopes,
                expected_type,
            );
        }
        if let HirExprKind::Comma { left, right } = &expression.kind {
            let left = self.lower_nested_awaits_typed(left, scopes, None);
            self.emit(CfgInstruction::Evaluate(left));
            return self.lower_nested_awaits_typed(right, scopes, expected_type);
        }
        if let HirExprKind::Assignment {
            left,
            operator,
            right,
        } = &expression.kind
        {
            if operator != "=" || expression_contains_await(left) || !expression_is_pure(left) {
                self.control_error(
                    "CRC2009",
                    "assignment suspension requires a simple side-effect-free left operand",
                    &expression.span,
                );
                return expression.clone();
            }
            return HirExpr {
                kind: HirExprKind::Assignment {
                    left: left.clone(),
                    operator: operator.clone(),
                    right: Box::new(self.lower_nested_awaits_typed(right, scopes, expected_type)),
                },
                span: expression.span.clone(),
            };
        }
        if !expression_is_linearizable(expression) {
            self.control_error(
                "CRC2006",
                "suspension in this expression requires sequenced branch or temporary lowering",
                &expression.span,
            );
            return expression.clone();
        }
        match &expression.kind {
            HirExprKind::Await(operand) => {
                let slot = self.fresh_await_slot();
                let suspend_block = self.new_block(scopes.to_vec());
                self.link_current(suspend_block, scopes);
                self.current = Some(suspend_block);
                let continuation = self.new_block(scopes.to_vec());
                self.terminate_suspend(
                    *operand.clone(),
                    slot,
                    continuation,
                    scopes,
                    expression.span.clone(),
                );
                self.current = Some(continuation);
                HirExpr {
                    kind: HirExprKind::AwaitResultRef(slot),
                    span: expression.span.clone(),
                }
            }
            HirExprKind::Call {
                function,
                arguments,
            } => HirExpr {
                kind: HirExprKind::Call {
                    function: Box::new(self.lower_nested_awaits(function, scopes)),
                    arguments: arguments
                        .iter()
                        .map(|argument| self.lower_nested_awaits(argument, scopes))
                        .collect(),
                },
                span: expression.span.clone(),
            },
            HirExprKind::AsyncCall {
                target,
                callee,
                result_type,
                arguments,
            } => HirExpr {
                kind: HirExprKind::AsyncCall {
                    target: *target,
                    callee: callee.clone(),
                    result_type: result_type.clone(),
                    arguments: arguments
                        .iter()
                        .map(|argument| self.lower_nested_awaits(argument, scopes))
                        .collect(),
                },
                span: expression.span.clone(),
            },
            HirExprKind::Binary {
                left,
                operator,
                right,
            } => HirExpr {
                kind: HirExprKind::Binary {
                    left: Box::new(self.lower_nested_awaits_typed(
                        left,
                        scopes,
                        expected_type.clone(),
                    )),
                    operator: operator.clone(),
                    right: Box::new(self.lower_nested_awaits_typed(
                        right,
                        scopes,
                        expected_type.clone(),
                    )),
                },
                span: expression.span.clone(),
            },
            HirExprKind::Composite { source, extensions } => HirExpr {
                kind: HirExprKind::Composite {
                    source: source.clone(),
                    extensions: extensions
                        .iter()
                        .map(|extension| {
                            self.lower_nested_awaits_typed(extension, scopes, expected_type.clone())
                        })
                        .collect(),
                },
                span: expression.span.clone(),
            },
            HirExprKind::Conditional { .. }
            | HirExprKind::Comma { .. }
            | HirExprKind::Assignment { .. } => unreachable!(),
            HirExprKind::Yield(_)
            | HirExprKind::Source(_)
            | HirExprKind::AwaitResultRef(_)
            | HirExprKind::TaskRef { .. }
            | HirExprKind::Unary { .. } => expression.clone(),
        }
    }

    fn lower_conditional(
        &mut self,
        condition: &HirExpr,
        consequence: &HirExpr,
        alternative: &HirExpr,
        span: &SourceSpan,
        scopes: &[ScopeId],
        expected_type: Option<SourceFragment>,
    ) -> HirExpr {
        let Some(result_type) = expected_type else {
            self.control_error(
                "CRC2008",
                "conditional suspension needs a result type from its context",
                span,
            );
            return HirExpr {
                kind: HirExprKind::Conditional {
                    condition: Box::new(condition.clone()),
                    consequence: Box::new(consequence.clone()),
                    alternative: Box::new(alternative.clone()),
                },
                span: span.clone(),
            };
        };
        let condition = self.lower_nested_awaits_typed(
            condition,
            scopes,
            Some(SourceFragment {
                text: "int".to_owned(),
                span: condition.span.clone(),
            }),
        );
        let slot = self.fresh_await_slot();
        let consequence_block = self.new_block(scopes.to_vec());
        let alternative_block = self.new_block(scopes.to_vec());
        let merge_block = self.new_block(scopes.to_vec());
        if let Some(current) = self.current.take() {
            let source_scopes = self.block(current).scope_stack.clone();
            self.block_mut(current).terminator = CfgTerminator::Branch {
                condition,
                consequence: CfgEdge {
                    target: consequence_block,
                    kind: EdgeKind::Branch,
                    span: span.clone(),
                    source_scopes: source_scopes.clone(),
                    target_scopes: scopes.to_vec(),
                },
                alternative: CfgEdge {
                    target: alternative_block,
                    kind: EdgeKind::Branch,
                    span: span.clone(),
                    source_scopes,
                    target_scopes: scopes.to_vec(),
                },
            };
        }

        self.current = Some(consequence_block);
        let consequence =
            self.lower_nested_awaits_typed(consequence, scopes, Some(result_type.clone()));
        self.emit(CfgInstruction::AssignExpressionSlot {
            slot,
            expression: consequence,
            ty: result_type.clone(),
            span: span.clone(),
        });
        self.link_current(merge_block, scopes);

        self.current = Some(alternative_block);
        let alternative =
            self.lower_nested_awaits_typed(alternative, scopes, Some(result_type.clone()));
        self.emit(CfgInstruction::AssignExpressionSlot {
            slot,
            expression: alternative,
            ty: result_type,
            span: span.clone(),
        });
        self.link_current(merge_block, scopes);
        self.current = Some(merge_block);
        HirExpr {
            kind: HirExprKind::AwaitResultRef(slot),
            span: span.clone(),
        }
    }

    fn lower_short_circuit(
        &mut self,
        left: &HirExpr,
        operator: &str,
        right: &HirExpr,
        span: &SourceSpan,
        scopes: &[ScopeId],
    ) -> HirExpr {
        let left = self.lower_nested_awaits(left, scopes);
        let slot = self.fresh_await_slot();
        let rhs_block = self.new_block(scopes.to_vec());
        let short_block = self.new_block(scopes.to_vec());
        let merge_block = self.new_block(scopes.to_vec());
        if let Some(current) = self.current.take() {
            let source_scopes = self.block(current).scope_stack.clone();
            let (consequence, alternative) = if operator == "&&" {
                (rhs_block, short_block)
            } else {
                (short_block, rhs_block)
            };
            self.block_mut(current).terminator = CfgTerminator::Branch {
                condition: left,
                consequence: CfgEdge {
                    target: consequence,
                    kind: EdgeKind::Branch,
                    span: span.clone(),
                    source_scopes: source_scopes.clone(),
                    target_scopes: scopes.to_vec(),
                },
                alternative: CfgEdge {
                    target: alternative,
                    kind: EdgeKind::Branch,
                    span: span.clone(),
                    source_scopes,
                    target_scopes: scopes.to_vec(),
                },
            };
        }

        self.current = Some(short_block);
        self.emit(CfgInstruction::AssignExpressionSlot {
            slot,
            expression: HirExpr {
                kind: HirExprKind::Source(if operator == "&&" { "0" } else { "1" }.to_owned()),
                span: span.clone(),
            },
            ty: SourceFragment {
                text: "int".to_owned(),
                span: span.clone(),
            },
            span: span.clone(),
        });
        self.link_current(merge_block, scopes);

        self.current = Some(rhs_block);
        let right = self.lower_nested_awaits(right, scopes);
        let right_span = right.span.clone();
        self.emit(CfgInstruction::AssignExpressionSlot {
            slot,
            expression: HirExpr {
                kind: HirExprKind::Unary {
                    operator: "!!".to_owned(),
                    operand: Box::new(right),
                },
                span: right_span,
            },
            ty: SourceFragment {
                text: "int".to_owned(),
                span: span.clone(),
            },
            span: span.clone(),
        });
        self.link_current(merge_block, scopes);
        self.current = Some(merge_block);
        HirExpr {
            kind: HirExprKind::AwaitResultRef(slot),
            span: span.clone(),
        }
    }

    fn terminate_yield(
        &mut self,
        value: Option<CfgValue>,
        continuation: BlockId,
        target_scopes: &[ScopeId],
        span: SourceSpan,
    ) {
        if let Some(current) = self.current.take() {
            let source_scopes = self.block(current).scope_stack.clone();
            self.block_mut(current).terminator = CfgTerminator::Yield {
                value,
                continuation: CfgEdge {
                    target: continuation,
                    kind: EdgeKind::Resume,
                    span: span.clone(),
                    source_scopes,
                    target_scopes: target_scopes.to_vec(),
                },
                span,
            };
        }
    }

    fn terminate_goto(&mut self, target: JumpTarget, kind: EdgeKind, span: &SourceSpan) {
        if let Some(current) = self.current.take() {
            let source_scopes = self.block(current).scope_stack.clone();
            self.block_mut(current).terminator = CfgTerminator::Goto(CfgEdge {
                target: target.block,
                kind,
                span: span.clone(),
                source_scopes,
                target_scopes: target.scopes,
            });
        }
    }

    fn link_current(&mut self, target: BlockId, target_scopes: &[ScopeId]) {
        let Some(current) = self.current.take() else {
            return;
        };
        if !matches!(self.block(current).terminator, CfgTerminator::Open) {
            return;
        }
        let source_scopes = self.block(current).scope_stack.clone();
        self.block_mut(current).terminator = CfgTerminator::Goto(CfgEdge {
            target,
            kind: EdgeKind::Fallthrough,
            span: self.function.span.clone(),
            source_scopes,
            target_scopes: target_scopes.to_vec(),
        });
    }

    fn set_goto(
        &mut self,
        source: BlockId,
        target: BlockId,
        target_scopes: &[ScopeId],
        kind: EdgeKind,
    ) {
        let source_scopes = self.block(source).scope_stack.clone();
        self.block_mut(source).terminator = CfgTerminator::Goto(CfgEdge {
            target,
            kind,
            span: self.function.span.clone(),
            source_scopes,
            target_scopes: target_scopes.to_vec(),
        });
    }

    fn emit(&mut self, instruction: CfgInstruction) {
        if let Some(current) = self.current {
            self.block_mut(current).instructions.push(instruction);
        }
    }

    fn allocate_label_targets(&mut self, block: &HirBlock, scopes: &[ScopeId]) {
        for statement in &block.statements {
            self.allocate_statement_labels(statement, scopes);
        }
    }

    fn allocate_statement_labels(&mut self, statement: &HirStmt, scopes: &[ScopeId]) {
        match statement {
            HirStmt::Block(block) => {
                let mut nested = scopes.to_vec();
                if nested.last() != Some(&block.scope) {
                    nested.push(block.scope);
                }
                self.allocate_label_targets(block, &nested);
            }
            HirStmt::If {
                consequence,
                alternative,
                ..
            } => {
                self.allocate_statement_labels(consequence, scopes);
                if let Some(alternative) = alternative {
                    self.allocate_statement_labels(alternative, scopes);
                }
            }
            HirStmt::While { body, .. }
            | HirStmt::DoWhile { body, .. }
            | HirStmt::For { body, .. }
            | HirStmt::Switch { body, .. } => self.allocate_statement_labels(body, scopes),
            HirStmt::Case { statements, .. } => {
                for statement in statements {
                    self.allocate_statement_labels(statement, scopes);
                }
            }
            HirStmt::Label { id, statement, .. } => {
                let block = self.new_block(scopes.to_vec());
                self.label_targets.insert(
                    *id,
                    JumpTarget {
                        block,
                        scopes: scopes.to_vec(),
                    },
                );
                self.allocate_statement_labels(statement, scopes);
            }
            _ => {}
        }
    }

    fn scope_stack(&self, scope: ScopeId) -> Vec<ScopeId> {
        let parents: HashMap<_, _> = self
            .function
            .scopes
            .iter()
            .map(|item| (item.id, item.parent))
            .collect();
        let mut stack = Vec::new();
        let mut current = Some(scope);
        while let Some(id) = current {
            stack.push(id);
            current = parents.get(&id).copied().flatten();
        }
        stack.reverse();
        stack
    }

    fn fresh_await_slot(&mut self) -> AwaitSlotId {
        let slot = AwaitSlotId(self.next_await_slot);
        self.next_await_slot += 1;
        slot
    }

    fn new_block(&mut self, scope_stack: Vec<ScopeId>) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock {
            id,
            scope_stack,
            instructions: Vec::new(),
            terminator: CfgTerminator::Open,
        });
        id
    }

    fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }

    fn block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        &mut self.blocks[id.0 as usize]
    }

    fn control_error(&mut self, code: &'static str, message: &str, span: &SourceSpan) {
        self.diagnostics.push(Diagnostic {
            code,
            severity: DiagnosticSeverity::Error,
            message: message.to_owned(),
            primary_span: span.clone(),
            related: Vec::new(),
        });
    }
}

fn assign_await_edge_ids(blocks: &mut [BasicBlock]) {
    let mut suspensions: Vec<_> = blocks
        .iter()
        .filter_map(|block| match &block.terminator {
            CfgTerminator::Suspend { span, .. } => Some((block.id, span.start_byte, span.end_byte)),
            _ => None,
        })
        .collect();
    suspensions.sort_by_key(|(block, start, end)| (*block, *start, *end));
    for (index, (block, _, _)) in suspensions.into_iter().enumerate() {
        if let CfgTerminator::Suspend { edge, .. } = &mut blocks[block.0 as usize].terminator {
            *edge = AwaitEdgeId(index as u32);
        }
    }
}

fn expression_contains_await(expression: &HirExpr) -> bool {
    match &expression.kind {
        HirExprKind::Await(_) => true,
        HirExprKind::Yield(value) => value.as_deref().is_some_and(expression_contains_await),
        HirExprKind::Call {
            function,
            arguments,
        } => expression_contains_await(function) || arguments.iter().any(expression_contains_await),
        HirExprKind::AsyncCall { arguments, .. } => arguments.iter().any(expression_contains_await),
        HirExprKind::Binary { left, right, .. } => {
            expression_contains_await(left) || expression_contains_await(right)
        }
        HirExprKind::Conditional {
            condition,
            consequence,
            alternative,
        } => {
            expression_contains_await(condition)
                || expression_contains_await(consequence)
                || expression_contains_await(alternative)
        }
        HirExprKind::Comma { left, right } => {
            expression_contains_await(left) || expression_contains_await(right)
        }
        HirExprKind::Assignment { left, right, .. } => {
            expression_contains_await(left) || expression_contains_await(right)
        }
        HirExprKind::Unary { operand, .. } => expression_contains_await(operand),
        HirExprKind::Composite { extensions, .. } => {
            extensions.iter().any(expression_contains_await)
        }
        HirExprKind::Source(_) | HirExprKind::AwaitResultRef(_) | HirExprKind::TaskRef { .. } => {
            false
        }
    }
}

fn expression_is_linearizable(expression: &HirExpr) -> bool {
    match &expression.kind {
        HirExprKind::Source(source) => source_is_pure(source),
        HirExprKind::AwaitResultRef(_) | HirExprKind::TaskRef { .. } => true,
        HirExprKind::Await(operand) => !expression_contains_await(operand),
        HirExprKind::Yield(_) => false,
        HirExprKind::Call {
            function,
            arguments,
        } => {
            !expression_contains_await(function)
                && arguments.iter().all(|argument| {
                    if expression_contains_await(argument) {
                        expression_is_linearizable(argument)
                    } else {
                        expression_is_pure(argument)
                    }
                })
        }
        HirExprKind::AsyncCall { arguments, .. } => arguments.iter().all(|argument| {
            if expression_contains_await(argument) {
                expression_is_linearizable(argument)
            } else {
                expression_is_pure(argument)
            }
        }),
        HirExprKind::Binary {
            left,
            operator,
            right,
        } => {
            !matches!(operator.as_str(), "&&" | "||")
                && expression_is_linearizable(left)
                && expression_is_linearizable(right)
        }
        HirExprKind::Conditional { .. }
        | HirExprKind::Comma { .. }
        | HirExprKind::Assignment { .. } => false,
        HirExprKind::Unary { operand, .. } => expression_is_linearizable(operand),
        HirExprKind::Composite { source, extensions } => {
            !has_sequencing_operator(source) && extensions.iter().all(expression_is_linearizable)
        }
    }
}

fn expression_is_pure(expression: &HirExpr) -> bool {
    match &expression.kind {
        HirExprKind::Source(source) => source_is_pure(source),
        HirExprKind::AwaitResultRef(_) | HirExprKind::TaskRef { .. } => true,
        HirExprKind::Composite { source, extensions } => {
            !has_sequencing_operator(source) && extensions.iter().all(expression_is_pure)
        }
        HirExprKind::Call { .. }
        | HirExprKind::AsyncCall { .. }
        | HirExprKind::Await(_)
        | HirExprKind::Yield(_) => false,
        HirExprKind::Binary { left, right, .. } => {
            expression_is_pure(left) && expression_is_pure(right)
        }
        HirExprKind::Unary { operand, .. } => expression_is_pure(operand),
        HirExprKind::Conditional { .. }
        | HirExprKind::Comma { .. }
        | HirExprKind::Assignment { .. } => false,
    }
}

fn source_is_pure(source: &str) -> bool {
    !has_sequencing_operator(source)
}

fn has_sequencing_operator(source: &str) -> bool {
    if source.contains("&&")
        || source.contains("||")
        || source.contains('?')
        || source.contains(',')
        || source.contains("++")
        || source.contains("--")
    {
        return true;
    }
    let bytes = source.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        if *byte != b'=' {
            return false;
        }
        let previous = index.checked_sub(1).and_then(|index| bytes.get(index));
        let next = bytes.get(index + 1);
        !matches!(previous, Some(b'=' | b'!' | b'<' | b'>')) && next != Some(&b'=')
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use crate::semantic::build_hir;
    use crate::syntax::SyntaxParser;

    use super::*;

    fn cfg_for(source: &str) -> CfgUnit {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("flow.cr"), source)
            .expect("source parses");
        build_cfg(&build_hir(&syntax))
    }

    #[test]
    fn splits_await_into_suspend_and_continuation_blocks() {
        let cfg = cfg_for(
            r#"
__async int fetch(int socket) {
    int bytes = __await read_socket(socket);
    return bytes;
}
"#,
        );

        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
        let function = &cfg.functions[0];
        let suspend = function
            .blocks
            .iter()
            .find_map(|block| match &block.terminator {
                CfgTerminator::Suspend { continuation, .. } => Some(continuation),
                _ => None,
            })
            .expect("suspend terminator");
        assert_eq!(suspend.source_scopes, suspend.target_scopes);
        assert!(
            function.blocks[suspend.target.0 as usize]
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, CfgInstruction::AssignAwaitResult { .. }))
        );
    }

    #[test]
    fn assigns_await_edges_in_block_and_source_order() {
        let cfg = cfg_for(
            r#"
__async int sequence(cr_awaitable first, cr_awaitable second) {
    int one = __await first;
    return one + __await second;
}
"#,
        );

        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
        let suspensions: Vec<_> = cfg.functions[0]
            .blocks
            .iter()
            .filter_map(|block| match &block.terminator {
                CfgTerminator::Suspend { edge, span, .. } => {
                    Some((block.id, span.start_byte, *edge))
                }
                _ => None,
            })
            .collect();
        assert_eq!(suspensions.len(), 2);
        assert_eq!(suspensions[0].2, AwaitEdgeId(0));
        assert_eq!(suspensions[1].2, AwaitEdgeId(1));
        assert!(suspensions[0].0 < suspensions[1].0);
        assert!(suspensions[0].1 < suspensions[1].1);
    }

    #[test]
    fn retains_sync_defer_as_a_cfg_registration() {
        let cfg = cfg_for(
            r#"
int guarded(int handle) {
    __defer close_handle(handle);
    return handle;
}
"#,
        );

        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
        assert!(!cfg.functions[0].is_async);
        assert!(cfg.functions[0].blocks.iter().any(|block| {
            block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, CfgInstruction::RegisterDefer(_)))
        }));
    }

    #[test]
    fn records_scope_exit_on_outward_goto() {
        let cfg = cfg_for(
            r#"
__async int flow(int value) {
done:
    if (value) {
        __defer release(value);
        goto done;
    }
    return 0;
}
"#,
        );

        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
        let outward = cfg.functions[0]
            .blocks
            .iter()
            .filter_map(|block| match &block.terminator {
                CfgTerminator::Goto(edge)
                    if edge.source_scopes.len() > edge.target_scopes.len() =>
                {
                    Some(edge)
                }
                _ => None,
            })
            .next()
            .expect("outward edge");
        assert!(outward.source_scopes.starts_with(&outward.target_scopes));
    }

    #[test]
    fn nested_await_results_remain_structured_in_cfg_expressions() {
        let cfg = cfg_for(
            r#"
__async int next(void) { return 1; }
__async int flow(int *values) {
    int indexed = values[__await next()];
    int shorted = (__await next()) && (__await next());
    return indexed + shorted;
}
"#,
        );

        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
        let flow = cfg
            .functions
            .iter()
            .find(|function| function.name == "flow")
            .expect("flow CFG");
        let mut slots = BTreeSet::new();
        for block in &flow.blocks {
            for instruction in &block.instructions {
                match instruction {
                    CfgInstruction::Declaration(declaration) => {
                        if let Some(initializer) = &declaration.initializer {
                            collect_structured_slots(initializer, &mut slots);
                        }
                    }
                    CfgInstruction::AssignExpression { expression, .. }
                    | CfgInstruction::AssignExpressionSlot { expression, .. }
                    | CfgInstruction::Evaluate(expression) => {
                        collect_structured_slots(expression, &mut slots);
                    }
                    CfgInstruction::RegisterDefer(defer) => {
                        collect_structured_slots(&defer.call, &mut slots);
                    }
                    CfgInstruction::PushCleanup(registration) => {
                        for argument in &registration.arguments {
                            collect_structured_slots(argument, &mut slots);
                        }
                    }
                    CfgInstruction::Source(_)
                    | CfgInstruction::AssignAwaitResult { .. }
                    | CfgInstruction::RunCleanups { .. } => {}
                }
            }
            match &block.terminator {
                CfgTerminator::Branch { condition, .. } => {
                    collect_structured_slots(condition, &mut slots);
                }
                CfgTerminator::Switch {
                    expression, cases, ..
                } => {
                    collect_structured_slots(expression, &mut slots);
                    for case in cases {
                        collect_structured_slots(&case.value, &mut slots);
                    }
                }
                CfgTerminator::Suspend { operand, .. } => {
                    collect_structured_slots(operand, &mut slots);
                }
                CfgTerminator::Yield { value, .. } | CfgTerminator::Return { value, .. } => {
                    if let Some(CfgValue::Expression(expression)) = value {
                        collect_structured_slots(expression, &mut slots);
                    }
                }
                CfgTerminator::Open | CfgTerminator::Goto(_) | CfgTerminator::Unreachable => {}
            }
        }
        assert!(slots.len() >= 3, "structured slots: {slots:?}");
    }

    fn collect_structured_slots(expression: &HirExpr, slots: &mut BTreeSet<AwaitSlotId>) {
        match &expression.kind {
            HirExprKind::Source(source) => {
                assert!(!source.contains("__cr_await_result_"), "{source}");
            }
            HirExprKind::AwaitResultRef(slot) => {
                slots.insert(*slot);
            }
            HirExprKind::Await(operand) | HirExprKind::Unary { operand, .. } => {
                collect_structured_slots(operand, slots);
            }
            HirExprKind::Yield(value) => {
                if let Some(value) = value {
                    collect_structured_slots(value, slots);
                }
            }
            HirExprKind::AsyncCall { arguments, .. } => {
                for argument in arguments {
                    collect_structured_slots(argument, slots);
                }
            }
            HirExprKind::Binary { left, right, .. }
            | HirExprKind::Comma { left, right }
            | HirExprKind::Assignment { left, right, .. } => {
                collect_structured_slots(left, slots);
                collect_structured_slots(right, slots);
            }
            HirExprKind::Conditional {
                condition,
                consequence,
                alternative,
            } => {
                collect_structured_slots(condition, slots);
                collect_structured_slots(consequence, slots);
                collect_structured_slots(alternative, slots);
            }
            HirExprKind::Call {
                function,
                arguments,
            } => {
                collect_structured_slots(function, slots);
                for argument in arguments {
                    collect_structured_slots(argument, slots);
                }
            }
            HirExprKind::Composite { source, extensions } => {
                assert!(!source.contains("__cr_await_result_"), "{source}");
                for extension in extensions {
                    collect_structured_slots(extension, slots);
                }
            }
            HirExprKind::TaskRef { .. } => {}
        }
    }
}
