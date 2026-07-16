//! Declaration-identity liveness analysis across coroutine suspension points.

use std::collections::{BTreeMap, BTreeSet};

use crate::control_flow::{BlockId, CfgInstruction, CfgTerminator, CfgValue, successor_blocks};
use crate::coroutine::{CoroutineFunction, CoroutineUnit};
use crate::semantic::{DeclarationId, HirExpr, HirExprKind, ScopeId, SourceFragment};
use crate::slot_liveness::ProgramPoint;
use crate::syntax::Diagnostic;

/// A context field required after one or more pending polls.
#[derive(Debug, Clone)]
pub struct LiftedField {
    pub declaration: DeclarationId,
    pub field_name: String,
    pub source_name: String,
    pub ty: SourceFragment,
    pub scope: ScopeId,
    pub declaration_start: usize,
    pub is_parameter: bool,
    pub is_task: bool,
    pub address_taken: bool,
    pub cleanup_retained: bool,
    pub volatile_or_atomic: bool,
}

/// Data-flow results for one coroutine function.
#[derive(Debug, Clone)]
pub struct LivenessFunction {
    pub coroutine: CoroutineFunction,
    pub lifted_fields: Vec<LiftedField>,
    pub live_in: BTreeMap<BlockId, BTreeSet<DeclarationId>>,
    pub live_out: BTreeMap<BlockId, BTreeSet<DeclarationId>>,
    pub live_points: BTreeMap<ProgramPoint, BTreeSet<DeclarationId>>,
}

/// Liveness-lowered translation unit.
#[derive(Debug, Clone)]
pub struct LivenessUnit {
    pub functions: Vec<LivenessFunction>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Computes backwards liveness and the minimal persistent declaration set.
#[must_use]
pub fn analyze_liveness(unit: &CoroutineUnit) -> LivenessUnit {
    let functions = unit.functions.iter().map(analyze_function).collect();
    LivenessUnit {
        functions,
        diagnostics: unit.diagnostics.clone(),
    }
}

#[derive(Clone)]
struct DeclarationMeta {
    id: DeclarationId,
    name: String,
    ty: SourceFragment,
    scope: ScopeId,
    start_byte: usize,
    is_parameter: bool,
    is_task: bool,
}

#[derive(Default)]
struct BlockFacts {
    uses: BTreeSet<DeclarationId>,
    definitions: BTreeSet<DeclarationId>,
    address_taken: BTreeSet<DeclarationId>,
    cleanup_retained: BTreeSet<DeclarationId>,
}

fn analyze_function(function: &CoroutineFunction) -> LivenessFunction {
    let declarations = collect_declarations(function);
    let mut facts = BTreeMap::new();
    for block in &function.cfg.blocks {
        facts.insert(block.id, block_facts(function, block.id, &declarations));
    }

    let mut live_in: BTreeMap<_, BTreeSet<_>> = function
        .cfg
        .blocks
        .iter()
        .map(|block| (block.id, BTreeSet::new()))
        .collect();
    let mut live_out = live_in.clone();
    loop {
        let mut changed = false;
        for block in function.cfg.blocks.iter().rev() {
            let mut next_out = BTreeSet::new();
            for successor in successor_blocks(&block.terminator) {
                if let Some(successor_live) = live_in.get(&successor) {
                    next_out.extend(successor_live);
                }
            }
            let block_facts = &facts[&block.id];
            let mut next_in = block_facts.uses.clone();
            next_in.extend(
                next_out
                    .iter()
                    .filter(|id| !block_facts.definitions.contains(id)),
            );
            changed |= live_out[&block.id] != next_out || live_in[&block.id] != next_in;
            live_out.insert(block.id, next_out);
            live_in.insert(block.id, next_in);
        }
        if !changed {
            break;
        }
    }

    let mut lifted = BTreeSet::new();
    let mut address_taken = BTreeSet::new();
    let mut cleanup_retained = BTreeSet::<DeclarationId>::new();
    let has_suspend = function.cfg.blocks.iter().any(|block| {
        matches!(
            block.terminator,
            CfgTerminator::Suspend { .. } | CfgTerminator::Yield { .. }
        )
    });
    if function.cfg.is_async {
        lifted.extend(
            declarations
                .iter()
                .filter(|declaration| declaration.is_parameter)
                .map(|declaration| declaration.id),
        );
        lifted.extend(
            declarations
                .iter()
                .filter(|declaration| declaration.is_task)
                .map(|declaration| declaration.id),
        );
    }
    for block in &function.cfg.blocks {
        address_taken.extend(&facts[&block.id].address_taken);
        cleanup_retained.extend(&facts[&block.id].cleanup_retained);
        for instruction in &block.instructions {
            if let CfgInstruction::AssignAwaitResult { destination, .. } = instruction {
                lifted.insert(*destination);
            }
        }
        if matches!(
            block.terminator,
            CfgTerminator::Suspend { .. } | CfgTerminator::Yield { .. }
        ) {
            lifted.extend(&live_out[&block.id]);
            lifted.extend(&facts[&block.id].uses);
        }
    }
    if has_suspend {
        lifted.extend(&address_taken);
    }

    let live_points = declaration_program_points(function, &declarations, &live_in, &live_out);
    let lifted_fields =
        declarations
            .into_iter()
            .filter(|declaration| lifted.contains(&declaration.id))
            .map(|declaration| LiftedField {
                field_name: format!(
                    "cr_v_{}_{}",
                    declaration.id.0,
                    c_identifier(&declaration.name)
                ),
                address_taken: address_taken.contains(&declaration.id),
                cleanup_retained: cleanup_retained.contains(&declaration.id),
                volatile_or_atomic: declaration.ty.text.split_whitespace().any(|word| {
                    matches!(word, "volatile" | "_Atomic") || word.starts_with("_Atomic(")
                }),
                declaration: declaration.id,
                source_name: declaration.name,
                ty: declaration.ty,
                scope: declaration.scope,
                declaration_start: declaration.start_byte,
                is_parameter: declaration.is_parameter,
                is_task: declaration.is_task,
            })
            .collect();

    LivenessFunction {
        coroutine: function.clone(),
        lifted_fields,
        live_in,
        live_out,
        live_points,
    }
}

fn collect_declarations(function: &CoroutineFunction) -> Vec<DeclarationMeta> {
    let mut declarations: Vec<_> = function
        .cfg
        .parameters
        .iter()
        .map(|parameter| DeclarationMeta {
            id: parameter.id,
            name: parameter.name.clone(),
            ty: parameter.ty.clone(),
            scope: parameter.scope,
            start_byte: parameter.span.start_byte,
            is_parameter: true,
            is_task: false,
        })
        .collect();
    for block in &function.cfg.blocks {
        for instruction in &block.instructions {
            if let CfgInstruction::Declaration(declaration) = instruction {
                let mut ty = declaration.ty.clone();
                if declaration.is_task {
                    ty.text = "cr_awaitable".to_owned();
                }
                declarations.push(DeclarationMeta {
                    id: declaration.id,
                    name: declaration.name.clone(),
                    ty,
                    scope: declaration.scope,
                    start_byte: declaration.span.start_byte,
                    is_parameter: false,
                    is_task: declaration.is_task,
                });
            }
        }
    }
    declarations.sort_by_key(|declaration| declaration.id);
    declarations.dedup_by_key(|declaration| declaration.id);
    declarations
}

fn block_facts(
    function: &CoroutineFunction,
    block_id: BlockId,
    declarations: &[DeclarationMeta],
) -> BlockFacts {
    let block = &function.cfg.blocks[block_id.0 as usize];
    let mut facts = BlockFacts::default();
    for instruction in &block.instructions {
        match instruction {
            CfgInstruction::Source(fragment) => {
                add_fragment_uses(fragment, block, declarations, &mut facts);
            }
            CfgInstruction::Declaration(declaration) => {
                if let Some(initializer) = &declaration.initializer {
                    add_expression_uses(initializer, block, declarations, &mut facts);
                }
                facts.definitions.insert(declaration.id);
            }
            CfgInstruction::AssignAwaitResult { destination, .. } => {
                facts.definitions.insert(*destination);
            }
            CfgInstruction::AssignExpression {
                destination,
                expression,
                ..
            } => {
                add_expression_uses(expression, block, declarations, &mut facts);
                facts.definitions.insert(*destination);
            }
            CfgInstruction::AssignExpressionSlot { expression, .. } => {
                add_expression_uses(expression, block, declarations, &mut facts);
            }
            CfgInstruction::Evaluate(expression) => {
                add_expression_uses(expression, block, declarations, &mut facts);
            }
            CfgInstruction::RegisterDefer(defer) => {
                add_expression_uses(&defer.call, block, declarations, &mut facts);
            }
            CfgInstruction::PushCleanup(registration) => {
                for argument in &registration.arguments {
                    let mut argument_facts = BlockFacts::default();
                    add_expression_uses(argument, block, declarations, &mut argument_facts);
                    facts.uses.extend(&argument_facts.uses);
                    facts.address_taken.extend(&argument_facts.address_taken);
                    facts.cleanup_retained.extend(&argument_facts.address_taken);
                }
            }
            CfgInstruction::RunCleanups { .. } => {}
        }
    }
    match &block.terminator {
        CfgTerminator::Branch { condition, .. } => {
            add_expression_uses(condition, block, declarations, &mut facts);
        }
        CfgTerminator::Switch {
            expression, cases, ..
        } => {
            add_expression_uses(expression, block, declarations, &mut facts);
            for case in cases {
                add_expression_uses(&case.value, block, declarations, &mut facts);
            }
        }
        CfgTerminator::Suspend { operand, .. } => {
            add_expression_uses(operand, block, declarations, &mut facts);
        }
        CfgTerminator::Yield { value, .. } | CfgTerminator::Return { value, .. } => {
            if let Some(CfgValue::Expression(expression)) = value {
                add_expression_uses(expression, block, declarations, &mut facts);
            }
        }
        CfgTerminator::Open | CfgTerminator::Goto(_) | CfgTerminator::Unreachable => {}
    }
    facts
}

fn declaration_program_points(
    function: &CoroutineFunction,
    declarations: &[DeclarationMeta],
    live_in: &BTreeMap<BlockId, BTreeSet<DeclarationId>>,
    live_out: &BTreeMap<BlockId, BTreeSet<DeclarationId>>,
) -> BTreeMap<ProgramPoint, BTreeSet<DeclarationId>> {
    let mut points = BTreeMap::new();
    for block in &function.cfg.blocks {
        let mut live = live_out.get(&block.id).cloned().unwrap_or_default();
        let mut terminator_facts = BlockFacts::default();
        add_terminator_facts(
            &block.terminator,
            block,
            declarations,
            &mut terminator_facts,
        );
        live.extend(terminator_facts.uses);
        points.insert(ProgramPoint::BeforeTerminator(block.id), live.clone());
        if let CfgTerminator::Suspend {
            edge, continuation, ..
        } = &block.terminator
        {
            points.insert(
                ProgramPoint::Suspension {
                    block: block.id,
                    edge: *edge,
                },
                live.clone(),
            );
            points.insert(
                ProgramPoint::ContinuationEntry {
                    block: continuation.target,
                    edge: *edge,
                },
                live_in
                    .get(&continuation.target)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        for (index, instruction) in block.instructions.iter().enumerate().rev() {
            let index = index as u32;
            points.insert(
                ProgramPoint::AfterInstruction {
                    block: block.id,
                    instruction: index,
                },
                live.clone(),
            );
            let instruction_facts = instruction_facts(instruction, block, declarations);
            live.retain(|declaration| !instruction_facts.definitions.contains(declaration));
            live.extend(instruction_facts.uses);
            points.insert(
                ProgramPoint::BeforeInstruction {
                    block: block.id,
                    instruction: index,
                },
                live.clone(),
            );
        }
        points.insert(ProgramPoint::BlockEntry(block.id), live);
    }
    points
}

fn instruction_facts(
    instruction: &CfgInstruction,
    block: &crate::control_flow::BasicBlock,
    declarations: &[DeclarationMeta],
) -> BlockFacts {
    let mut facts = BlockFacts::default();
    match instruction {
        CfgInstruction::Source(fragment) => {
            add_fragment_uses(fragment, block, declarations, &mut facts);
        }
        CfgInstruction::Declaration(declaration) => {
            if let Some(initializer) = &declaration.initializer {
                add_expression_uses(initializer, block, declarations, &mut facts);
            }
            facts.definitions.insert(declaration.id);
        }
        CfgInstruction::AssignAwaitResult { destination, .. } => {
            facts.definitions.insert(*destination);
        }
        CfgInstruction::AssignExpression {
            destination,
            expression,
            ..
        } => {
            add_expression_uses(expression, block, declarations, &mut facts);
            facts.definitions.insert(*destination);
        }
        CfgInstruction::AssignExpressionSlot { expression, .. }
        | CfgInstruction::Evaluate(expression) => {
            add_expression_uses(expression, block, declarations, &mut facts);
        }
        CfgInstruction::RegisterDefer(defer) => {
            add_expression_uses(&defer.call, block, declarations, &mut facts);
        }
        CfgInstruction::PushCleanup(registration) => {
            for argument in &registration.arguments {
                add_expression_uses(argument, block, declarations, &mut facts);
            }
        }
        CfgInstruction::RunCleanups { .. } => {}
    }
    facts
}

fn add_terminator_facts(
    terminator: &CfgTerminator,
    block: &crate::control_flow::BasicBlock,
    declarations: &[DeclarationMeta],
    facts: &mut BlockFacts,
) {
    match terminator {
        CfgTerminator::Branch { condition, .. } => {
            add_expression_uses(condition, block, declarations, facts);
        }
        CfgTerminator::Switch {
            expression, cases, ..
        } => {
            add_expression_uses(expression, block, declarations, facts);
            for case in cases {
                add_expression_uses(&case.value, block, declarations, facts);
            }
        }
        CfgTerminator::Suspend { operand, .. } => {
            add_expression_uses(operand, block, declarations, facts);
        }
        CfgTerminator::Yield { value, .. } | CfgTerminator::Return { value, .. } => {
            if let Some(CfgValue::Expression(expression)) = value {
                add_expression_uses(expression, block, declarations, facts);
            }
        }
        CfgTerminator::Open | CfgTerminator::Goto(_) | CfgTerminator::Unreachable => {}
    }
}

fn add_fragment_uses(
    fragment: &SourceFragment,
    block: &crate::control_flow::BasicBlock,
    declarations: &[DeclarationMeta],
    facts: &mut BlockFacts,
) {
    add_identifier_uses(
        &fragment.text,
        fragment.span.start_byte,
        block,
        declarations,
        facts,
    );
}

fn add_expression_uses(
    expression: &HirExpr,
    block: &crate::control_flow::BasicBlock,
    declarations: &[DeclarationMeta],
    facts: &mut BlockFacts,
) {
    match &expression.kind {
        HirExprKind::Source(source) => add_identifier_uses(
            source,
            expression.span.start_byte,
            block,
            declarations,
            facts,
        ),
        HirExprKind::TaskRef { declaration, .. } => {
            facts.uses.insert(*declaration);
        }
        HirExprKind::Await(operand) => {
            add_expression_uses(operand, block, declarations, facts);
        }
        HirExprKind::AwaitResultRef(_) => {}
        HirExprKind::Unary { operand, .. } => {
            add_expression_uses(operand, block, declarations, facts);
        }
        HirExprKind::Yield(value) => {
            if let Some(value) = value {
                add_expression_uses(value, block, declarations, facts);
            }
        }
        HirExprKind::Call {
            function,
            arguments,
        } => {
            add_expression_uses(function, block, declarations, facts);
            for argument in arguments {
                add_expression_uses(argument, block, declarations, facts);
            }
        }
        HirExprKind::AsyncCall { arguments, .. } => {
            for argument in arguments {
                add_expression_uses(argument, block, declarations, facts);
            }
        }
        HirExprKind::Binary { left, right, .. } => {
            add_expression_uses(left, block, declarations, facts);
            add_expression_uses(right, block, declarations, facts);
        }
        HirExprKind::Conditional {
            condition,
            consequence,
            alternative,
        } => {
            add_expression_uses(condition, block, declarations, facts);
            add_expression_uses(consequence, block, declarations, facts);
            add_expression_uses(alternative, block, declarations, facts);
        }
        HirExprKind::Comma { left, right } => {
            add_expression_uses(left, block, declarations, facts);
            add_expression_uses(right, block, declarations, facts);
        }
        HirExprKind::Assignment { left, right, .. } => {
            add_expression_uses(left, block, declarations, facts);
            add_expression_uses(right, block, declarations, facts);
        }
        HirExprKind::Composite { source, .. } => add_identifier_uses(
            source,
            expression.span.start_byte,
            block,
            declarations,
            facts,
        ),
    }
}

fn add_identifier_uses(
    source: &str,
    expression_start: usize,
    block: &crate::control_flow::BasicBlock,
    declarations: &[DeclarationMeta],
    facts: &mut BlockFacts,
) {
    for identifier in scan_identifiers(source) {
        let Some(declaration) = resolve_declaration(
            &identifier.name,
            expression_start,
            &block.scope_stack,
            declarations,
        ) else {
            continue;
        };
        if !facts.definitions.contains(&declaration.id) {
            facts.uses.insert(declaration.id);
        }
        if identifier.address_taken {
            facts.address_taken.insert(declaration.id);
        }
    }
}

fn resolve_declaration<'a>(
    name: &str,
    expression_start: usize,
    scope_stack: &[ScopeId],
    declarations: &'a [DeclarationMeta],
) -> Option<&'a DeclarationMeta> {
    declarations
        .iter()
        .filter(|declaration| {
            declaration.name == name
                && declaration.start_byte <= expression_start
                && scope_stack.contains(&declaration.scope)
        })
        .max_by_key(|declaration| {
            (
                scope_stack
                    .iter()
                    .position(|scope| *scope == declaration.scope)
                    .unwrap_or_default(),
                declaration.start_byte,
            )
        })
}

struct IdentifierUse {
    name: String,
    address_taken: bool,
}

fn scan_identifiers(source: &str) -> Vec<IdentifierUse> {
    let bytes = source.as_bytes();
    let mut identifiers = Vec::new();
    let mut index = 0;
    let mut previous_token = None;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                    } else if bytes[index] == quote {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            byte if byte == b'_' || byte.is_ascii_alphabetic() => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index] == b'_' || bytes[index].is_ascii_alphanumeric())
                {
                    index += 1;
                }
                identifiers.push(IdentifierUse {
                    name: source[start..index].to_owned(),
                    address_taken: previous_token == Some(b'&'),
                });
                previous_token = Some(b'i');
            }
            byte if byte.is_ascii_whitespace() => index += 1,
            byte => {
                previous_token = Some(byte);
                index += 1;
            }
        }
    }
    identifiers
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

    use crate::control_flow::EdgeKind;
    use crate::control_flow::build_cfg;
    use crate::coroutine::lower_coroutines;
    use crate::scope_exit::lower_scope_exits;
    use crate::semantic::build_hir;
    use crate::syntax::SyntaxParser;

    use super::*;

    fn analyzed(source: &str) -> LivenessUnit {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("liveness.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);
        let cfg = build_cfg(&hir);
        let scoped = lower_scope_exits(&cfg);
        analyze_liveness(&lower_coroutines(&scoped, "cr_"))
    }

    #[test]
    fn lifts_only_values_required_across_suspend() {
        let unit = analyzed(
            r#"
__async int run(int parameter) {
    int live = parameter;
    int dead = live + 1;
    __await task;
    return live;
}
"#,
        );

        assert!(unit.diagnostics.is_empty(), "{:?}", unit.diagnostics);
        let fields = &unit.functions[0].lifted_fields;
        assert!(fields.iter().any(|field| field.source_name == "parameter"));
        assert!(fields.iter().any(|field| field.source_name == "live"));
        assert!(!fields.iter().any(|field| field.source_name == "dead"));
    }

    #[test]
    fn keeps_shadowed_declarations_distinct() {
        let unit = analyzed(
            r#"
__async int run(int task) {
    int value = 1;
    {
        int value = 2;
        __await task;
        consume(value);
    }
    return value;
}
"#,
        );

        assert!(unit.diagnostics.is_empty(), "{:?}", unit.diagnostics);
        let value_fields: Vec<_> = unit.functions[0]
            .lifted_fields
            .iter()
            .filter(|field| field.source_name == "value")
            .collect();
        assert_eq!(value_fields.len(), 2);
        assert_ne!(value_fields[0].declaration, value_fields[1].declaration);
        assert_ne!(value_fields[0].field_name, value_fields[1].field_name);
    }

    #[test]
    fn conservatively_lifts_address_taken_value() {
        let unit = analyzed(
            r#"
__async int run(int task) {
    int value = 1;
    register_pointer(&value);
    __await task;
    return 0;
}
"#,
        );

        assert!(
            unit.functions[0]
                .lifted_fields
                .iter()
                .any(|field| field.source_name == "value" && field.address_taken)
        );
    }

    #[test]
    fn sync_defer_does_not_create_coroutine_fields() {
        let unit = analyzed(
            r#"
int guarded(int handle) {
    __defer close_handle(handle);
    return handle;
}
"#,
        );

        assert!(unit.functions[0].lifted_fields.is_empty());
    }

    #[test]
    fn cleanup_edges_remain_normal_successors() {
        let unit = analyzed(
            r#"
__async int run(int task) {
    {
        __defer close_task(task);
        __await task;
    }
    return 0;
}
"#,
        );
        assert!(
            unit.functions[0]
                .coroutine
                .cfg
                .blocks
                .iter()
                .flat_map(|block| successor_blocks(&block.terminator))
                .all(|successor| successor.0 < unit.functions[0].coroutine.cfg.blocks.len() as u32)
        );
        assert!(
            unit.functions[0]
                .coroutine
                .cfg
                .blocks
                .iter()
                .filter_map(|block| match &block.terminator {
                    CfgTerminator::Goto(edge) => Some(edge.kind),
                    _ => None,
                })
                .any(|kind| kind == EdgeKind::Cleanup)
        );
    }

    #[test]
    fn await_result_identity_does_not_scan_placeholder_shaped_declarations() {
        let unit = analyzed(
            r#"
__async int next(void) { return 1; }
__async int run(void) {
    int __cr_await_result_0 = 41;
    int value = (__await next()) + 1;
    return value;
}
"#,
        );

        assert!(unit.diagnostics.is_empty(), "{:?}", unit.diagnostics);
        let run = unit
            .functions
            .iter()
            .find(|function| function.coroutine.cfg.name == "run")
            .expect("run liveness");
        assert!(
            !run.lifted_fields
                .iter()
                .any(|field| field.source_name == "__cr_await_result_0")
        );
    }

    #[test]
    fn instruction_points_distinguish_nonoverlapping_lifted_lifetimes() {
        let unit = analyzed(
            r#"
static void consume(int value) { (void)value; }
__async int run(void) {
    int first = 1;
    __yield 0;
    consume(first);
    int second = 2;
    __yield 0;
    return second;
}
"#,
        );
        let run = unit
            .functions
            .iter()
            .find(|function| function.coroutine.cfg.name == "run")
            .expect("run liveness");
        let declaration = |name: &str| {
            run.lifted_fields
                .iter()
                .find(|field| field.source_name == name)
                .map(|field| field.declaration)
                .expect("lifted declaration")
        };
        let first = declaration("first");
        let second = declaration("second");
        assert!(run.live_points.values().any(|live| live.contains(&first)));
        assert!(run.live_points.values().any(|live| live.contains(&second)));
        assert!(
            run.live_points
                .values()
                .all(|live| !(live.contains(&first) && live.contains(&second)))
        );
    }

    #[test]
    fn records_cleanup_retention_and_qualified_type_exclusions() {
        let unit = analyzed(
            r#"
static void retain(int *value) { (void)value; }
__async int run(void) {
    int cleanup = 1;
    volatile int watched = 2;
    _Atomic int atomic = 3;
    __defer retain(&cleanup);
    __yield 0;
    return cleanup + watched + atomic;
}
"#,
        );
        let run = unit
            .functions
            .iter()
            .find(|function| function.coroutine.cfg.name == "run")
            .expect("run liveness");
        let field = |name: &str| {
            run.lifted_fields
                .iter()
                .find(|field| field.source_name == name)
                .expect("lifted field")
        };
        assert!(field("cleanup").cleanup_retained);
        assert!(field("cleanup").address_taken);
        assert!(field("watched").volatile_or_atomic);
        assert!(field("watched").ty.text.contains("volatile"));
        assert!(field("atomic").volatile_or_atomic);
        assert!(field("atomic").ty.text.contains("_Atomic"));
    }
}
