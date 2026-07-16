//! Verified coroutine CFG optimization infrastructure.

use std::collections::{BTreeSet, VecDeque};

use crate::config::OptimizationLevel;
use crate::control_flow::{
    BlockId, CfgFunction, CfgInstruction, CfgTerminator, CfgUnit, CfgValue, EdgeKind,
    successor_blocks, successor_edges, successor_edges_mut,
};
use crate::semantic::{DeclarationId, HirExpr, HirExprKind};
use crate::syntax::{Diagnostic, DiagnosticSeverity, SourceSpan};

/// Verification output for one CFG unit.
#[derive(Debug, Clone)]
pub struct CfgVerification {
    pub diagnostics: Vec<Diagnostic>,
}

impl CfgVerification {
    /// Returns true when the verifier found no structural error.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != DiagnosticSeverity::Error)
    }
}

/// Structural evidence recorded for one attempted CFG pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CfgPassReport {
    pub pass: &'static str,
    pub input_blocks: usize,
    pub output_blocks: usize,
    pub changed: bool,
}

/// A verified candidate graph or the diagnostics that rejected it.
#[derive(Debug, Clone)]
pub struct CfgOptimizationResult {
    pub unit: Option<CfgUnit>,
    pub diagnostics: Vec<Diagnostic>,
    pub reports: Vec<CfgPassReport>,
}

/// Verifies every function without modifying the input graph.
#[must_use]
pub fn verify_cfg(unit: &CfgUnit) -> CfgVerification {
    let mut diagnostics = Vec::new();
    for function in &unit.functions {
        verify_function(function, &mut diagnostics);
    }
    CfgVerification { diagnostics }
}

/// Runs the currently enabled verified pass set for one optimization level.
#[must_use]
pub fn optimize_coroutine_cfg(
    unit: &CfgUnit,
    optimization: OptimizationLevel,
) -> CfgOptimizationResult {
    match optimization {
        OptimizationLevel::None => {
            run_verified_pass(unit, "verify-none", |input| (input.clone(), false))
        }
        OptimizationLevel::Speed | OptimizationLevel::Size | OptimizationLevel::Aggressive => {
            let result = run_verified_pass(unit, "remove-unreachable", remove_unreachable_blocks);
            let result = continue_verified_pass(result, "thread-trivial-jumps", |input| {
                canonicalize_with_remap(input, thread_trivial_jumps)
            });
            continue_verified_pass(result, "merge-linear-blocks", |input| {
                canonicalize_with_remap(input, merge_linear_blocks)
            })
        }
    }
}

fn continue_verified_pass(
    mut previous: CfgOptimizationResult,
    pass: &'static str,
    transform: impl FnOnce(&CfgUnit) -> (CfgUnit, bool),
) -> CfgOptimizationResult {
    let Some(unit) = previous.unit.take() else {
        return previous;
    };
    let mut next = run_verified_pass(&unit, pass, transform);
    previous.reports.append(&mut next.reports);
    if next.unit.is_none() {
        previous.diagnostics.append(&mut next.diagnostics);
        previous.unit = None;
        previous
    } else {
        previous.unit = next.unit;
        previous
    }
}

fn canonicalize_with_remap(
    input: &CfgUnit,
    transform: impl FnOnce(&mut CfgUnit) -> bool,
) -> (CfgUnit, bool) {
    let mut candidate = input.clone();
    let changed = transform(&mut candidate);
    if !changed {
        return (candidate, false);
    }
    let (candidate, removed) = remove_unreachable_blocks(&candidate);
    (candidate, changed || removed)
}

fn thread_trivial_jumps(unit: &mut CfgUnit) -> bool {
    let mut changed = false;
    for function in &mut unit.functions {
        changed |= thread_trivial_jumps_in_function(function);
    }
    changed
}

fn thread_trivial_jumps_in_function(function: &mut CfgFunction) -> bool {
    let mut changed = false;
    while let Some((block, target, target_scopes)) = find_trivial_jump(function) {
        for source in &mut function.blocks {
            for edge in successor_edges_mut(&mut source.terminator) {
                if edge.target == block {
                    edge.target = target;
                    edge.target_scopes.clone_from(&target_scopes);
                }
            }
        }
        changed = true;
    }
    changed
}

fn find_trivial_jump(
    function: &CfgFunction,
) -> Option<(BlockId, BlockId, Vec<crate::semantic::ScopeId>)> {
    for block in &function.blocks {
        if block.id == function.entry || !block.instructions.is_empty() {
            continue;
        }
        let CfgTerminator::Goto(outgoing) = &block.terminator else {
            continue;
        };
        if outgoing.kind != EdgeKind::Fallthrough || outgoing.target == block.id {
            continue;
        }
        let target = &function.blocks[outgoing.target.0 as usize];
        if block.scope_stack != target.scope_stack
            || outgoing.source_scopes != block.scope_stack
            || outgoing.target_scopes != target.scope_stack
        {
            continue;
        }
        let incoming: Vec<_> = successor_edges_for_target(function, block.id).collect();
        if incoming.is_empty()
            || incoming.iter().any(|edge| {
                matches!(
                    edge.kind,
                    EdgeKind::UserGoto | EdgeKind::Resume | EdgeKind::Cleanup
                )
            })
        {
            continue;
        }
        return Some((block.id, outgoing.target, target.scope_stack.clone()));
    }
    None
}

fn successor_edges_for_target(
    function: &CfgFunction,
    target: BlockId,
) -> impl Iterator<Item = &crate::control_flow::CfgEdge> {
    function
        .blocks
        .iter()
        .flat_map(|block| successor_edges(&block.terminator))
        .filter(move |edge| edge.target == target)
}

fn merge_linear_blocks(unit: &mut CfgUnit) -> bool {
    let mut changed = false;
    for function in &mut unit.functions {
        changed |= merge_linear_blocks_in_function(function);
    }
    changed
}

fn merge_linear_blocks_in_function(function: &mut CfgFunction) -> bool {
    let mut changed = false;
    loop {
        let predecessor_counts = predecessor_counts(function);
        let candidate = function.blocks.iter().find_map(|block| {
            let CfgTerminator::Goto(edge) = &block.terminator else {
                return None;
            };
            let target = &function.blocks[edge.target.0 as usize];
            (edge.kind == EdgeKind::Fallthrough
                && block.id < target.id
                && target.id != function.entry
                && predecessor_counts[target.id.0 as usize] == 1
                && block.scope_stack == target.scope_stack
                && edge.source_scopes == block.scope_stack
                && edge.target_scopes == target.scope_stack
                && !has_lifecycle_boundary(block)
                && !has_lifecycle_boundary(target)
                && !has_terminator_boundary(target))
            .then_some((block.id, target.id))
        });
        let Some((source, target)) = candidate else {
            break;
        };

        let target_block = function.blocks[target.0 as usize].clone();
        let source_block = &mut function.blocks[source.0 as usize];
        source_block.instructions.extend(target_block.instructions);
        source_block.terminator = target_block.terminator;
        changed = true;
    }
    changed
}

fn predecessor_counts(function: &CfgFunction) -> Vec<usize> {
    let mut counts = vec![0; function.blocks.len()];
    for block in &function.blocks {
        for edge in successor_edges(&block.terminator) {
            counts[edge.target.0 as usize] += 1;
        }
    }
    counts
}

fn has_lifecycle_boundary(block: &crate::control_flow::BasicBlock) -> bool {
    block.instructions.iter().any(|instruction| {
        matches!(
            instruction,
            CfgInstruction::RegisterDefer(_)
                | CfgInstruction::PushCleanup(_)
                | CfgInstruction::RunCleanups { .. }
                | CfgInstruction::Declaration(crate::semantic::HirDeclaration {
                    is_task: true,
                    ..
                })
        )
    })
}

fn has_terminator_boundary(block: &crate::control_flow::BasicBlock) -> bool {
    matches!(
        block.terminator,
        CfgTerminator::Suspend { .. } | CfgTerminator::Yield { .. }
    ) || successor_edges(&block.terminator).into_iter().any(|edge| {
        matches!(
            edge.kind,
            EdgeKind::UserGoto | EdgeKind::Resume | EdgeKind::Cleanup
        )
    })
}

fn remove_unreachable_blocks(input: &CfgUnit) -> (CfgUnit, bool) {
    let mut candidate = input.clone();
    let mut changed = false;
    for function in &mut candidate.functions {
        changed |= remove_unreachable_function(function);
    }
    (candidate, changed)
}

fn remove_unreachable_function(function: &mut CfgFunction) -> bool {
    let mut reachable = vec![false; function.blocks.len()];
    extend_reachability(function, &mut reachable, [function.entry]);

    let task_references = reachable_task_references(function, &reachable);
    let metadata_roots: Vec<_> = function
        .blocks
        .iter()
        .filter(|block| !reachable[block.id.0 as usize])
        .filter(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    CfgInstruction::Declaration(declaration)
                        if declaration.is_task && task_references.contains(&declaration.id)
                )
            })
        })
        .map(|block| block.id)
        .collect();
    extend_reachability(function, &mut reachable, metadata_roots);

    if reachable.iter().all(|reachable| *reachable) {
        return false;
    }

    let mut remap = vec![None; function.blocks.len()];
    let mut blocks = Vec::with_capacity(reachable.iter().filter(|item| **item).count());
    for (old_index, block) in function.blocks.iter().enumerate() {
        if !reachable[old_index] {
            continue;
        }
        let new_id = BlockId(blocks.len() as u32);
        remap[old_index] = Some(new_id);
        let mut block = block.clone();
        block.id = new_id;
        blocks.push(block);
    }

    function.entry = remap[function.entry.0 as usize].expect("reachable entry block");
    for block in &mut blocks {
        for edge in successor_edges_mut(&mut block.terminator) {
            edge.target = remap[edge.target.0 as usize].expect("reachable successor block");
        }
    }
    function.blocks = blocks;
    true
}

fn extend_reachability(
    function: &CfgFunction,
    reachable: &mut [bool],
    roots: impl IntoIterator<Item = BlockId>,
) {
    let mut pending: VecDeque<_> = roots.into_iter().collect();
    while let Some(block) = pending.pop_front() {
        let index = block.0 as usize;
        if reachable[index] {
            continue;
        }
        reachable[index] = true;
        pending.extend(successor_blocks(&function.blocks[index].terminator));
    }
}

fn reachable_task_references(
    function: &CfgFunction,
    reachable: &[bool],
) -> BTreeSet<DeclarationId> {
    let mut declarations = BTreeSet::new();
    for block in function
        .blocks
        .iter()
        .filter(|block| reachable[block.id.0 as usize])
    {
        for instruction in &block.instructions {
            match instruction {
                CfgInstruction::Declaration(declaration) => {
                    if let Some(initializer) = &declaration.initializer {
                        collect_task_references(initializer, &mut declarations);
                    }
                }
                CfgInstruction::AssignExpression { expression, .. }
                | CfgInstruction::AssignExpressionSlot { expression, .. }
                | CfgInstruction::Evaluate(expression) => {
                    collect_task_references(expression, &mut declarations);
                }
                CfgInstruction::RegisterDefer(defer) => {
                    collect_task_references(&defer.call, &mut declarations);
                }
                CfgInstruction::PushCleanup(registration) => {
                    for argument in &registration.arguments {
                        collect_task_references(argument, &mut declarations);
                    }
                }
                CfgInstruction::Source(_)
                | CfgInstruction::AssignAwaitResult { .. }
                | CfgInstruction::RunCleanups { .. } => {}
            }
        }
        match &block.terminator {
            CfgTerminator::Branch { condition, .. } => {
                collect_task_references(condition, &mut declarations);
            }
            CfgTerminator::Switch {
                expression, cases, ..
            } => {
                collect_task_references(expression, &mut declarations);
                for case in cases {
                    collect_task_references(&case.value, &mut declarations);
                }
            }
            CfgTerminator::Suspend { operand, .. } => {
                collect_task_references(operand, &mut declarations);
            }
            CfgTerminator::Yield { value, .. } | CfgTerminator::Return { value, .. } => {
                if let Some(CfgValue::Expression(expression)) = value {
                    collect_task_references(expression, &mut declarations);
                }
            }
            CfgTerminator::Open | CfgTerminator::Goto(_) | CfgTerminator::Unreachable => {}
        }
    }
    declarations
}

fn collect_task_references(expression: &HirExpr, declarations: &mut BTreeSet<DeclarationId>) {
    match &expression.kind {
        HirExprKind::TaskRef { declaration, .. } => {
            declarations.insert(*declaration);
        }
        HirExprKind::Await(operand) => collect_task_references(operand, declarations),
        HirExprKind::Unary { operand, .. } => collect_task_references(operand, declarations),
        HirExprKind::Yield(value) => {
            if let Some(value) = value {
                collect_task_references(value, declarations);
            }
        }
        HirExprKind::AsyncCall { arguments, .. } => {
            for argument in arguments {
                collect_task_references(argument, declarations);
            }
        }
        HirExprKind::Binary { left, right, .. }
        | HirExprKind::Comma { left, right }
        | HirExprKind::Assignment { left, right, .. } => {
            collect_task_references(left, declarations);
            collect_task_references(right, declarations);
        }
        HirExprKind::Conditional {
            condition,
            consequence,
            alternative,
        } => {
            collect_task_references(condition, declarations);
            collect_task_references(consequence, declarations);
            collect_task_references(alternative, declarations);
        }
        HirExprKind::Call {
            function,
            arguments,
        } => {
            collect_task_references(function, declarations);
            for argument in arguments {
                collect_task_references(argument, declarations);
            }
        }
        HirExprKind::Composite { extensions, .. } => {
            for extension in extensions {
                collect_task_references(extension, declarations);
            }
        }
        HirExprKind::Source(_) | HirExprKind::AwaitResultRef(_) => {}
    }
}

fn run_verified_pass(
    input: &CfgUnit,
    pass: &'static str,
    transform: impl FnOnce(&CfgUnit) -> (CfgUnit, bool),
) -> CfgOptimizationResult {
    let input_verification = verify_cfg(input);
    if !input_verification.is_valid() {
        return CfgOptimizationResult {
            unit: None,
            diagnostics: input_verification.diagnostics,
            reports: Vec::new(),
        };
    }

    let input_blocks = block_count(input);
    let (candidate, changed) = transform(input);
    let output_blocks = block_count(&candidate);
    let report = CfgPassReport {
        pass,
        input_blocks,
        output_blocks,
        changed,
    };
    let candidate_verification = verify_cfg(&candidate);
    if !candidate_verification.is_valid() {
        return CfgOptimizationResult {
            unit: None,
            diagnostics: candidate_verification.diagnostics,
            reports: vec![report],
        };
    }

    CfgOptimizationResult {
        unit: Some(candidate),
        diagnostics: Vec::new(),
        reports: vec![report],
    }
}

fn block_count(unit: &CfgUnit) -> usize {
    unit.functions
        .iter()
        .map(|function| function.blocks.len())
        .sum()
}

fn verify_function(function: &CfgFunction, diagnostics: &mut Vec<Diagnostic>) {
    let block_count = function.blocks.len();
    if block_count == 0 || function.entry.0 as usize >= block_count {
        diagnostics.push(internal_diagnostic(
            "CRC7002",
            format!(
                "CFG function `{}` has invalid entry block {}",
                function.name, function.entry.0
            ),
            &function.span,
        ));
    }

    let mut await_edges = BTreeSet::new();
    let mut cleanup_ids = BTreeSet::new();
    for (index, block) in function.blocks.iter().enumerate() {
        if block.id != BlockId(index as u32) {
            diagnostics.push(internal_diagnostic(
                "CRC7001",
                format!(
                    "CFG function `{}` expected block {index} at index {index}, found {}",
                    function.name, block.id.0
                ),
                &function.span,
            ));
        }

        if matches!(block.terminator, CfgTerminator::Open) {
            diagnostics.push(internal_diagnostic(
                "CRC7003",
                format!(
                    "CFG function `{}` contains open terminator in block {}",
                    function.name, block.id.0
                ),
                &function.span,
            ));
        }

        for instruction in &block.instructions {
            if let CfgInstruction::PushCleanup(registration) = instruction
                && !cleanup_ids.insert(registration.id)
            {
                diagnostics.push(internal_diagnostic(
                    "CRC7008",
                    format!(
                        "CFG function `{}` contains duplicate cleanup identity {}",
                        function.name, registration.id.0
                    ),
                    &registration.span,
                ));
            }
        }

        if let CfgTerminator::Suspend {
            edge,
            continuation,
            span,
            ..
        } = &block.terminator
        {
            if !await_edges.insert(*edge) {
                diagnostics.push(internal_diagnostic(
                    "CRC7009",
                    format!(
                        "CFG function `{}` contains duplicate await edge identity {}",
                        function.name, edge.0
                    ),
                    span,
                ));
            }
            verify_resume_edge(function, block.id, continuation, span, diagnostics);
        }
        if let CfgTerminator::Yield {
            continuation, span, ..
        } = &block.terminator
        {
            verify_resume_edge(function, block.id, continuation, span, diagnostics);
        }

        for edge in successor_edges(&block.terminator) {
            if edge.source_scopes != block.scope_stack {
                diagnostics.push(internal_diagnostic(
                    "CRC7005",
                    format!(
                        "CFG edge from block {} in `{}` has inconsistent source scopes",
                        block.id.0, function.name
                    ),
                    &edge.span,
                ));
            }
            let Some(target) = function.blocks.get(edge.target.0 as usize) else {
                diagnostics.push(internal_diagnostic(
                    "CRC7004",
                    format!(
                        "CFG edge from block {} in `{}` targets missing block {}",
                        block.id.0, function.name, edge.target.0
                    ),
                    &edge.span,
                ));
                continue;
            };
            if target.id != edge.target {
                diagnostics.push(internal_diagnostic(
                    "CRC7004",
                    format!(
                        "CFG edge from block {} in `{}` targets mismatched block identity {}",
                        block.id.0, function.name, edge.target.0
                    ),
                    &edge.span,
                ));
            }
            if edge.target_scopes != target.scope_stack {
                diagnostics.push(internal_diagnostic(
                    "CRC7006",
                    format!(
                        "CFG edge to block {} in `{}` has inconsistent target scopes",
                        edge.target.0, function.name
                    ),
                    &edge.span,
                ));
            }
        }
    }
}

fn verify_resume_edge(
    function: &CfgFunction,
    source: BlockId,
    continuation: &crate::control_flow::CfgEdge,
    span: &SourceSpan,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if continuation.kind != EdgeKind::Resume {
        diagnostics.push(internal_diagnostic(
            "CRC7007",
            format!(
                "CFG suspension in block {} of `{}` has a non-resume continuation",
                source.0, function.name
            ),
            span,
        ));
    }
}

fn internal_diagnostic(code: &'static str, message: String, span: &SourceSpan) -> Diagnostic {
    Diagnostic {
        code,
        severity: DiagnosticSeverity::Error,
        message,
        primary_span: span.clone(),
        related: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::control_flow::{
        AwaitEdgeId, BasicBlock, CfgEdge, CfgInstruction, CleanupId, build_cfg, successor_blocks,
        successor_edges, successor_edges_mut,
    };
    use crate::scope_exit::lower_scope_exits;
    use crate::semantic::build_hir;
    use crate::syntax::SyntaxParser;

    use super::*;

    fn lowered(source: &str) -> CfgUnit {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("optimizer.cr"), source)
            .expect("source parses");
        lower_scope_exits(&build_cfg(&build_hir(&syntax)))
    }

    fn valid_unit() -> CfgUnit {
        lowered(
            r#"
__async int verified(int task, int value) {
    __defer release(value);
    if (value) {
        __yield value;
    }
    return __await task;
}
"#,
        )
    }

    fn first_edge_mut(function: &mut CfgFunction) -> &mut CfgEdge {
        for block in &mut function.blocks {
            match &mut block.terminator {
                CfgTerminator::Goto(edge) => return edge,
                CfgTerminator::Branch { consequence, .. } => return consequence,
                CfgTerminator::Switch { default, .. } => return default,
                CfgTerminator::Suspend { continuation, .. }
                | CfgTerminator::Yield { continuation, .. } => return continuation,
                CfgTerminator::Open | CfgTerminator::Return { .. } | CfgTerminator::Unreachable => {
                }
            }
        }
        panic!("fixture has a successor edge")
    }

    fn insert_unreachable_block(function: &mut CfgFunction, index: usize) {
        assert!(index <= function.blocks.len());
        for block in &mut function.blocks {
            if block.id.0 as usize >= index {
                block.id.0 += 1;
            }
            for edge in successor_edges_mut(&mut block.terminator) {
                if edge.target.0 as usize >= index {
                    edge.target.0 += 1;
                }
            }
        }
        if function.entry.0 as usize >= index {
            function.entry.0 += 1;
        }
        let scope_stack = function
            .blocks
            .first()
            .map(|block| block.scope_stack.clone())
            .unwrap_or_default();
        function.blocks.insert(
            index,
            BasicBlock {
                id: BlockId(index as u32),
                scope_stack,
                instructions: Vec::new(),
                terminator: CfgTerminator::Unreachable,
            },
        );
    }

    fn append_disconnected_suspend_and_yield(function: &mut CfgFunction) {
        let mut suspend = function
            .blocks
            .iter()
            .find(|block| matches!(block.terminator, CfgTerminator::Suspend { .. }))
            .expect("suspend block")
            .clone();
        suspend.id = BlockId(function.blocks.len() as u32);
        if let CfgTerminator::Suspend { edge, .. } = &mut suspend.terminator {
            let next_edge = function
                .blocks
                .iter()
                .filter_map(|block| match block.terminator {
                    CfgTerminator::Suspend { edge, .. } => Some(edge.0),
                    _ => None,
                })
                .max()
                .unwrap_or_default()
                + 1;
            *edge = AwaitEdgeId(next_edge);
        }
        function.blocks.push(suspend);

        let mut yielded = function
            .blocks
            .iter()
            .find(|block| matches!(block.terminator, CfgTerminator::Yield { .. }))
            .expect("yield block")
            .clone();
        yielded.id = BlockId(function.blocks.len() as u32);
        function.blocks.push(yielded);
    }

    fn insert_trivial_block_on_matching_edge(
        function: &mut CfgFunction,
        predicate: impl Fn(&CfgEdge) -> bool,
        use_source_scope: bool,
    ) -> BlockId {
        let (block_index, edge_index, target, span, source_scopes, target_scopes) = function
            .blocks
            .iter()
            .enumerate()
            .find_map(|(block_index, block)| {
                successor_edges(&block.terminator)
                    .into_iter()
                    .enumerate()
                    .find(|(_, edge)| predicate(edge))
                    .map(|(edge_index, edge)| {
                        (
                            block_index,
                            edge_index,
                            edge.target,
                            edge.span.clone(),
                            edge.source_scopes.clone(),
                            edge.target_scopes.clone(),
                        )
                    })
            })
            .expect("matching edge");
        let block_scope = if use_source_scope {
            source_scopes
        } else {
            target_scopes.clone()
        };
        let block = BlockId(function.blocks.len() as u32);
        function.blocks.push(BasicBlock {
            id: block,
            scope_stack: block_scope.clone(),
            instructions: Vec::new(),
            terminator: CfgTerminator::Goto(CfgEdge {
                target,
                kind: EdgeKind::Fallthrough,
                span,
                source_scopes: block_scope.clone(),
                target_scopes,
            }),
        });
        let incoming = successor_edges_mut(&mut function.blocks[block_index].terminator)
            .into_iter()
            .nth(edge_index)
            .expect("matching mutable edge");
        incoming.target = block;
        incoming.target_scopes = block_scope;
        block
    }

    fn split_entry_for_linear_merge(function: &mut CfgFunction) -> Vec<&'static str> {
        let entry = function.entry.0 as usize;
        let scope_stack = function.blocks[entry].scope_stack.clone();
        let instructions = std::mem::take(&mut function.blocks[entry].instructions);
        let kinds = instruction_kinds(&instructions);
        let terminator = std::mem::replace(
            &mut function.blocks[entry].terminator,
            CfgTerminator::Unreachable,
        );
        let target = BlockId(function.blocks.len() as u32);
        function.blocks[entry].terminator = CfgTerminator::Goto(CfgEdge {
            target,
            kind: EdgeKind::Fallthrough,
            span: function.span.clone(),
            source_scopes: scope_stack.clone(),
            target_scopes: scope_stack.clone(),
        });
        function.blocks.push(BasicBlock {
            id: target,
            scope_stack,
            instructions,
            terminator,
        });
        kinds
    }

    fn instruction_kinds(instructions: &[CfgInstruction]) -> Vec<&'static str> {
        instructions
            .iter()
            .map(|instruction| match instruction {
                CfgInstruction::Source(_) => "source",
                CfgInstruction::Declaration(_) => "declaration",
                CfgInstruction::AssignAwaitResult { .. } => "assign-await-result",
                CfgInstruction::AssignExpression { .. } => "assign-expression",
                CfgInstruction::AssignExpressionSlot { .. } => "assign-expression-slot",
                CfgInstruction::Evaluate(_) => "evaluate",
                CfgInstruction::RegisterDefer(_) => "register-defer",
                CfgInstruction::PushCleanup(_) => "push-cleanup",
                CfgInstruction::RunCleanups { .. } => "run-cleanups",
            })
            .collect()
    }

    fn graph_shape(function: &CfgFunction) -> Vec<(BlockId, Vec<BlockId>)> {
        function
            .blocks
            .iter()
            .map(|block| (block.id, successor_blocks(&block.terminator)))
            .collect()
    }

    fn logical_identity_shape(
        function: &CfgFunction,
    ) -> (Vec<AwaitEdgeId>, Vec<CleanupId>, Vec<(usize, usize)>) {
        let await_edges = function
            .blocks
            .iter()
            .filter_map(|block| match block.terminator {
                CfgTerminator::Suspend { edge, .. } => Some(edge),
                _ => None,
            })
            .collect();
        let cleanup_ids = function
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .filter_map(|instruction| match instruction {
                CfgInstruction::PushCleanup(registration) => Some(registration.id),
                _ => None,
            })
            .collect();
        let edge_spans = function
            .blocks
            .iter()
            .flat_map(|block| successor_edges(&block.terminator))
            .map(|edge| (edge.span.start_byte, edge.span.end_byte))
            .collect();
        (await_edges, cleanup_ids, edge_spans)
    }

    fn assert_has_code(unit: &CfgUnit, code: &str) {
        assert!(
            verify_cfg(unit)
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == code),
            "missing {code}"
        );
    }

    #[test]
    fn accepts_real_scope_exit_suspend_and_yield_graphs() {
        let verification = verify_cfg(&valid_unit());
        assert!(verification.is_valid(), "{:?}", verification.diagnostics);
    }

    #[test]
    fn rejects_sparse_or_duplicate_block_identity() {
        let mut sparse = valid_unit();
        sparse.functions[0].blocks[1].id = BlockId(99);
        assert_has_code(&sparse, "CRC7001");

        let mut duplicate = valid_unit();
        duplicate.functions[0].blocks[1].id = BlockId(0);
        assert_has_code(&duplicate, "CRC7001");
    }

    #[test]
    fn rejects_invalid_entry_and_open_terminator() {
        let mut entry = valid_unit();
        entry.functions[0].entry = BlockId(u32::MAX);
        assert_has_code(&entry, "CRC7002");

        let mut open = valid_unit();
        open.functions[0].blocks[0].terminator = CfgTerminator::Open;
        assert_has_code(&open, "CRC7003");
    }

    #[test]
    fn rejects_missing_successor_and_scope_metadata() {
        let mut missing = valid_unit();
        first_edge_mut(&mut missing.functions[0]).target = BlockId(u32::MAX);
        assert_has_code(&missing, "CRC7004");

        let mut source_scope = valid_unit();
        first_edge_mut(&mut source_scope.functions[0])
            .source_scopes
            .clear();
        assert_has_code(&source_scope, "CRC7005");

        let mut target_scope = valid_unit();
        first_edge_mut(&mut target_scope.functions[0])
            .target_scopes
            .clear();
        assert_has_code(&target_scope, "CRC7006");
    }

    #[test]
    fn rejects_non_resume_suspend_or_yield_continuation() {
        let mut unit = valid_unit();
        let continuation = unit.functions[0]
            .blocks
            .iter_mut()
            .find_map(|block| match &mut block.terminator {
                CfgTerminator::Suspend { continuation, .. }
                | CfgTerminator::Yield { continuation, .. } => Some(continuation),
                _ => None,
            })
            .expect("suspension continuation");
        continuation.kind = EdgeKind::Branch;
        assert_has_code(&unit, "CRC7007");
    }

    #[test]
    fn transactional_pass_rejects_an_invalid_candidate() {
        let unit = valid_unit();
        let result = run_verified_pass(&unit, "corrupt", |input| {
            let mut candidate = input.clone();
            first_edge_mut(&mut candidate.functions[0]).target = BlockId(u32::MAX);
            (candidate, true)
        });

        assert!(result.unit.is_none());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CRC7004")
        );
        assert_eq!(result.reports[0].pass, "corrupt");
        assert!(result.reports[0].changed);
    }

    #[test]
    fn every_optimization_level_returns_a_verified_graph() {
        let unit = valid_unit();
        let input_blocks = block_count(&unit);
        for optimization in [
            OptimizationLevel::None,
            OptimizationLevel::Speed,
            OptimizationLevel::Size,
            OptimizationLevel::Aggressive,
        ] {
            let result = optimize_coroutine_cfg(&unit, optimization);
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
            let output = result.unit.expect("verified unit");
            assert!(verify_cfg(&output).is_valid());
            if optimization == OptimizationLevel::None {
                assert_eq!(block_count(&output), input_blocks);
                assert_eq!(result.reports.len(), 1);
                assert!(!result.reports[0].changed);
            } else {
                assert!(block_count(&output) <= input_blocks);
                assert_eq!(result.reports.len(), 3);
            }
        }
    }

    #[test]
    fn optimized_levels_remove_an_unreachable_middle_block_and_remap_edges() {
        let original = valid_unit();
        let canonical = optimize_coroutine_cfg(&original, OptimizationLevel::Speed)
            .unit
            .expect("canonical original");
        let canonical_shape = graph_shape(&canonical.functions[0]);
        let canonical_identities = logical_identity_shape(&canonical.functions[0]);
        let original_blocks = original.functions[0].blocks.len();
        let mut with_unreachable = original.clone();
        insert_unreachable_block(&mut with_unreachable.functions[0], 1);
        assert!(verify_cfg(&with_unreachable).is_valid());

        let none = optimize_coroutine_cfg(&with_unreachable, OptimizationLevel::None)
            .unit
            .expect("None graph");
        assert_eq!(none.functions[0].blocks.len(), original_blocks + 1);

        for optimization in [
            OptimizationLevel::Speed,
            OptimizationLevel::Size,
            OptimizationLevel::Aggressive,
        ] {
            let result = optimize_coroutine_cfg(&with_unreachable, optimization);
            assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
            let optimized = result.unit.expect("optimized graph");
            assert_eq!(graph_shape(&optimized.functions[0]), canonical_shape);
            assert_eq!(
                logical_identity_shape(&optimized.functions[0]),
                canonical_identities
            );
            assert_eq!(result.reports[0].pass, "remove-unreachable");
            assert_eq!(result.reports[0].input_blocks, original_blocks + 1);
            assert_eq!(result.reports[0].output_blocks, original_blocks);
            assert!(result.reports[0].changed);
        }
    }

    #[test]
    fn removes_disconnected_label_cleanup_suspend_and_yield_blocks() {
        let mut unit = lowered(
            r#"
__async int unreachable_kinds(int task, int value) {
    __defer release(value);
    if (value) __yield value;
    return __await task;
unused:
    value++;
    return value;
}
"#,
        );
        let function = &mut unit.functions[0];
        append_disconnected_suspend_and_yield(function);
        assert_eq!(
            function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| matches!(instruction, CfgInstruction::RunCleanups { .. }))
                .count(),
            2
        );
        assert_eq!(
            function
                .blocks
                .iter()
                .filter(|block| matches!(block.terminator, CfgTerminator::Suspend { .. }))
                .count(),
            2
        );
        assert_eq!(
            function
                .blocks
                .iter()
                .filter(|block| matches!(block.terminator, CfgTerminator::Yield { .. }))
                .count(),
            2
        );
        let input_blocks = function.blocks.len();

        let optimized = optimize_coroutine_cfg(&unit, OptimizationLevel::Speed)
            .unit
            .expect("optimized graph");
        let function = &optimized.functions[0];
        assert!(function.blocks.len() < input_blocks);
        assert_eq!(
            function
                .blocks
                .iter()
                .flat_map(|block| &block.instructions)
                .filter(|instruction| matches!(instruction, CfgInstruction::RunCleanups { .. }))
                .count(),
            1
        );
        assert_eq!(
            function
                .blocks
                .iter()
                .filter(|block| matches!(block.terminator, CfgTerminator::Suspend { .. }))
                .count(),
            1
        );
        assert_eq!(
            function
                .blocks
                .iter()
                .filter(|block| matches!(block.terminator, CfgTerminator::Yield { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn unreachable_removal_is_idempotent_and_function_order_independent() {
        let mut first = valid_unit();
        first.functions[0].name = "first".to_owned();
        insert_unreachable_block(&mut first.functions[0], 1);
        let mut second = valid_unit();
        second.functions[0].name = "second".to_owned();
        insert_unreachable_block(&mut second.functions[0], 2);

        let forward = CfgUnit {
            functions: vec![first.functions[0].clone(), second.functions[0].clone()],
            diagnostics: Vec::new(),
        };
        let reverse = CfgUnit {
            functions: vec![second.functions[0].clone(), first.functions[0].clone()],
            diagnostics: Vec::new(),
        };
        let forward = optimize_coroutine_cfg(&forward, OptimizationLevel::Speed)
            .unit
            .expect("forward graph");
        let reverse = optimize_coroutine_cfg(&reverse, OptimizationLevel::Speed)
            .unit
            .expect("reverse graph");

        for name in ["first", "second"] {
            let forward_function = forward
                .functions
                .iter()
                .find(|function| function.name == name)
                .expect("forward function");
            let reverse_function = reverse
                .functions
                .iter()
                .find(|function| function.name == name)
                .expect("reverse function");
            assert_eq!(graph_shape(forward_function), graph_shape(reverse_function));
        }

        let repeated = optimize_coroutine_cfg(&forward, OptimizationLevel::Speed)
            .unit
            .expect("repeated graph");
        for (once, twice) in forward.functions.iter().zip(&repeated.functions) {
            assert_eq!(graph_shape(once), graph_shape(twice));
        }
    }

    #[test]
    fn preserves_skipped_task_declaration_metadata_for_a_reachable_task_ref() {
        let unit = lowered(
            r#"
__async int binding_child(int value) {
    return value;
}

__async int inactive_binding(void) {
    goto skipped;
    __async int task = binding_child(9);
skipped:
    return __await task;
}
"#,
        );
        let optimized = optimize_coroutine_cfg(&unit, OptimizationLevel::Speed)
            .unit
            .expect("optimized graph");

        let coroutine = crate::coroutine::lower_coroutines(&optimized, "cr_");
        assert!(
            coroutine.diagnostics.is_empty(),
            "{:?}",
            coroutine.diagnostics
        );
        let inactive = coroutine
            .functions
            .iter()
            .find(|function| function.cfg.name == "inactive_binding")
            .expect("inactive binding function");
        assert_eq!(inactive.await_plan.children.len(), 1);
        assert!(matches!(
            inactive.await_plan.children[0].origin,
            crate::await_plan::ChildOrigin::Binding(_)
        ));
    }

    #[test]
    fn threads_transitive_empty_fallthrough_blocks() {
        let original = valid_unit();
        let canonical = optimize_coroutine_cfg(&original, OptimizationLevel::Speed)
            .unit
            .expect("canonical graph");
        let mut with_trivial = original.clone();
        insert_trivial_block_on_matching_edge(
            &mut with_trivial.functions[0],
            |edge| edge.kind == EdgeKind::Branch && edge.source_scopes == edge.target_scopes,
            false,
        );
        insert_trivial_block_on_matching_edge(
            &mut with_trivial.functions[0],
            |edge| edge.kind == EdgeKind::Branch && edge.source_scopes == edge.target_scopes,
            false,
        );

        let result = optimize_coroutine_cfg(&with_trivial, OptimizationLevel::Speed);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(result.reports[1].changed);
        let optimized = result.unit.expect("optimized graph");
        assert_eq!(
            graph_shape(&optimized.functions[0]),
            graph_shape(&canonical.functions[0])
        );
    }

    #[test]
    fn trivial_threading_respects_user_resume_cleanup_and_scope_boundaries() {
        let mut user = valid_unit();
        let user_block = insert_trivial_block_on_matching_edge(
            &mut user.functions[0],
            |edge| edge.kind == EdgeKind::Branch,
            false,
        );
        let incoming = successor_edges_mut(&mut user.functions[0].blocks[0].terminator)
            .into_iter()
            .find(|edge| edge.target == user_block)
            .expect("user incoming edge");
        incoming.kind = EdgeKind::UserGoto;
        thread_trivial_jumps_in_function(&mut user.functions[0]);
        assert!(
            successor_edges_for_target(&user.functions[0], user_block)
                .next()
                .is_some()
        );

        let mut resume = valid_unit();
        let resume_block = insert_trivial_block_on_matching_edge(
            &mut resume.functions[0],
            |edge| edge.kind == EdgeKind::Resume,
            false,
        );
        thread_trivial_jumps_in_function(&mut resume.functions[0]);
        assert!(
            successor_edges_for_target(&resume.functions[0], resume_block)
                .next()
                .is_some()
        );

        let mut cleanup = lowered(
            r#"
__async int cleanup_jump(int value) {
done:
    if (value) {
        __defer release(value);
        goto done;
    }
    return 0;
}
"#,
        );
        let cleanup_block = insert_trivial_block_on_matching_edge(
            &mut cleanup.functions[0],
            |edge| edge.kind == EdgeKind::Cleanup,
            false,
        );
        thread_trivial_jumps_in_function(&mut cleanup.functions[0]);
        assert!(
            successor_edges_for_target(&cleanup.functions[0], cleanup_block)
                .next()
                .is_some()
        );

        let mut scoped = lowered(
            r#"
__async int scoped_branch(int value) {
    if (value) {
        value++;
    }
    return value;
}
"#,
        );
        let scope_block = insert_trivial_block_on_matching_edge(
            &mut scoped.functions[0],
            |edge| edge.source_scopes != edge.target_scopes,
            true,
        );
        thread_trivial_jumps_in_function(&mut scoped.functions[0]);
        assert!(
            successor_edges_for_target(&scoped.functions[0], scope_block)
                .next()
                .is_some()
        );
    }

    #[test]
    fn merges_safe_linear_blocks_and_preserves_instruction_order() {
        let mut unit = lowered(
            r#"
__async int linear(int value) {
    int result = value + 1;
    result *= 2;
    return result;
}
"#,
        );
        let expected = split_entry_for_linear_merge(&mut unit.functions[0]);
        let input_blocks = unit.functions[0].blocks.len();
        let result = optimize_coroutine_cfg(&unit, OptimizationLevel::Speed);
        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(result.reports[2].changed);
        let optimized = result.unit.expect("optimized graph");
        assert!(optimized.functions[0].blocks.len() < input_blocks);
        assert_eq!(
            instruction_kinds(&optimized.functions[0].blocks[0].instructions),
            expected
        );
    }

    #[test]
    fn linear_merge_rejects_cleanup_and_suspend_boundaries() {
        let mut cleanup = lowered(
            r#"
__async int cleanup_boundary(int value) {
    __defer release(value);
    return value;
}
"#,
        );
        split_entry_for_linear_merge(&mut cleanup.functions[0]);
        assert!(!merge_linear_blocks_in_function(&mut cleanup.functions[0]));

        let mut user_goto = lowered(
            r#"
__async int user_goto_boundary(int value) {
    goto done;
done:
    return value;
}
"#,
        );
        split_entry_for_linear_merge(&mut user_goto.functions[0]);
        assert!(!merge_linear_blocks_in_function(
            &mut user_goto.functions[0]
        ));

        let mut suspend = lowered(
            r#"
__async int suspend_boundary(int task) {
    return __await task;
}
"#,
        );
        split_entry_for_linear_merge(&mut suspend.functions[0]);
        merge_linear_blocks_in_function(&mut suspend.functions[0]);
        let suspend_block = suspend.functions[0]
            .blocks
            .iter()
            .find(|block| matches!(block.terminator, CfgTerminator::Suspend { .. }))
            .expect("suspend block");
        assert!(
            successor_edges_for_target(&suspend.functions[0], suspend_block.id)
                .next()
                .is_some(),
            "the suspend repoll block must remain a separate target"
        );
    }
}
