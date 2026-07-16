//! Instruction-level liveness for identity-bearing await-result slots.

use std::collections::{BTreeMap, BTreeSet};

use crate::await_plan::{ChildInstanceId, ChildOrigin, ResultLayout};
use crate::control_flow::{AwaitEdgeId, BlockId, CfgInstruction, CfgTerminator, CfgValue};
use crate::coroutine::{CoroutineFunction, CoroutineUnit};
use crate::semantic::{AwaitSlotId, HirExpr, HirExprKind};

/// A stable observation point within one coroutine CFG function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ProgramPoint {
    BlockEntry(BlockId),
    BeforeInstruction { block: BlockId, instruction: u32 },
    AfterInstruction { block: BlockId, instruction: u32 },
    BeforeTerminator(BlockId),
    Suspension { block: BlockId, edge: AwaitEdgeId },
    ContinuationEntry { block: BlockId, edge: AwaitEdgeId },
}

/// A status-specific observation while one direct child remains active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChildLifecyclePhase {
    Activated,
    Pending,
    Yielded,
    ReadyResultCopied,
    ErrorCopied,
    Canceled,
    InvalidStatus,
    ParentDrop,
}

/// One deterministic observation point in a direct child's ownership path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnershipPoint {
    pub block: BlockId,
    pub edge: AwaitEdgeId,
    pub phase: ChildLifecyclePhase,
}

/// A logical payload that can participate in future physical storage reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LogicalStorage {
    AwaitResult(AwaitSlotId),
    DirectChild(ChildInstanceId),
}

/// One canonical undirected ownership-interference edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OwnershipInterference {
    pub first: LogicalStorage,
    pub second: LogicalStorage,
}

impl OwnershipInterference {
    #[must_use]
    pub fn new(first: LogicalStorage, second: LogicalStorage) -> Option<Self> {
        (first != second).then(|| {
            if first < second {
                Self { first, second }
            } else {
                Self {
                    first: second,
                    second: first,
                }
            }
        })
    }

    #[must_use]
    pub fn contains(self, storage: LogicalStorage) -> bool {
        self.first == storage || self.second == storage
    }
}

/// Why one child isn't eligible for payload reuse in Stage 4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChildReuseExclusion {
    DeclarationOwnedBinding,
    CleanupRetained,
}

/// Ownership facts for one direct child payload and its independent flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectChildBundle {
    pub child: ChildInstanceId,
    pub edge: AwaitEdgeId,
    pub result_slot: AwaitSlotId,
    pub active_flag_independent: bool,
    pub live_points: BTreeSet<OwnershipPoint>,
}

/// Await-result liveness for one function in translation-unit order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotLivenessFunction {
    pub function_index: usize,
    pub function_name: String,
    pub live: BTreeMap<ProgramPoint, BTreeSet<AwaitSlotId>>,
    pub ownership_live: BTreeMap<OwnershipPoint, BTreeSet<LogicalStorage>>,
    pub direct_children: BTreeMap<ChildInstanceId, DirectChildBundle>,
    pub excluded_children: BTreeMap<ChildInstanceId, BTreeSet<ChildReuseExclusion>>,
    pub ownership_interference: BTreeSet<OwnershipInterference>,
}

impl SlotLivenessFunction {
    /// Returns the slots live at one stable program point.
    #[must_use]
    pub fn live_at(&self, point: ProgramPoint) -> Option<&BTreeSet<AwaitSlotId>> {
        self.live.get(&point)
    }

    /// Returns true when two logical payloads can't share physical storage.
    #[must_use]
    pub fn interferes(&self, first: LogicalStorage, second: LogicalStorage) -> bool {
        OwnershipInterference::new(first, second)
            .is_some_and(|edge| self.ownership_interference.contains(&edge))
    }
}

/// Deterministic slot-liveness results for one translation unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotLivenessUnit {
    pub functions: Vec<SlotLivenessFunction>,
}

/// Computes backward await-result liveness to a fixed point.
#[must_use]
pub fn analyze_slot_liveness(unit: &CoroutineUnit) -> SlotLivenessUnit {
    SlotLivenessUnit {
        functions: unit
            .functions
            .iter()
            .enumerate()
            .map(|(function_index, function)| analyze_function(function_index, function))
            .collect(),
    }
}

fn analyze_function(function_index: usize, function: &CoroutineFunction) -> SlotLivenessFunction {
    let blocks = &function.cfg.blocks;
    let mut live_in = vec![BTreeSet::new(); blocks.len()];
    loop {
        let mut changed = false;
        for block in blocks.iter().rev() {
            let mut live = live_before_terminator(&block.terminator, &live_in);
            for instruction in block.instructions.iter().rev() {
                live = live_before_instruction(instruction, live);
            }
            let entry = &mut live_in[block.id.0 as usize];
            if *entry != live {
                *entry = live;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut points = BTreeMap::new();
    for block in blocks {
        let before_terminator = live_before_terminator(&block.terminator, &live_in);
        points.insert(
            ProgramPoint::BeforeTerminator(block.id),
            before_terminator.clone(),
        );
        if let CfgTerminator::Suspend {
            edge,
            slot,
            continuation,
            ..
        } = &block.terminator
        {
            let mut suspension = live_in[continuation.target.0 as usize].clone();
            suspension.remove(slot);
            points.insert(
                ProgramPoint::Suspension {
                    block: block.id,
                    edge: *edge,
                },
                suspension,
            );
            points.insert(
                ProgramPoint::ContinuationEntry {
                    block: continuation.target,
                    edge: *edge,
                },
                live_in[continuation.target.0 as usize].clone(),
            );
        }

        let mut live = before_terminator;
        for (index, instruction) in block.instructions.iter().enumerate().rev() {
            let instruction_index = index as u32;
            points.insert(
                ProgramPoint::AfterInstruction {
                    block: block.id,
                    instruction: instruction_index,
                },
                live.clone(),
            );
            live = live_before_instruction(instruction, live);
            points.insert(
                ProgramPoint::BeforeInstruction {
                    block: block.id,
                    instruction: instruction_index,
                },
                live.clone(),
            );
        }
        points.insert(ProgramPoint::BlockEntry(block.id), live);
    }

    let ownership = analyze_child_ownership(function, &points);
    SlotLivenessFunction {
        function_index,
        function_name: function.cfg.name.clone(),
        live: points,
        ownership_live: ownership.live,
        direct_children: ownership.direct_children,
        excluded_children: ownership.excluded_children,
        ownership_interference: ownership.interference,
    }
}

struct ChildOwnershipAnalysis {
    live: BTreeMap<OwnershipPoint, BTreeSet<LogicalStorage>>,
    direct_children: BTreeMap<ChildInstanceId, DirectChildBundle>,
    excluded_children: BTreeMap<ChildInstanceId, BTreeSet<ChildReuseExclusion>>,
    interference: BTreeSet<OwnershipInterference>,
}

fn analyze_child_ownership(
    function: &CoroutineFunction,
    slot_live: &BTreeMap<ProgramPoint, BTreeSet<AwaitSlotId>>,
) -> ChildOwnershipAnalysis {
    let mut live = BTreeMap::<OwnershipPoint, BTreeSet<LogicalStorage>>::new();
    let mut direct_children = BTreeMap::new();
    let mut excluded_children = BTreeMap::new();

    for child in &function.await_plan.children {
        let ChildOrigin::Direct(edge_id) = child.origin else {
            excluded_children.insert(
                child.id,
                BTreeSet::from([
                    ChildReuseExclusion::DeclarationOwnedBinding,
                    ChildReuseExclusion::CleanupRetained,
                ]),
            );
            continue;
        };
        let edge = function
            .await_plan
            .edges
            .iter()
            .find(|edge| edge.id == edge_id && edge.instance == child.id)
            .expect("direct child must own one await edge");
        let retained = slot_live
            .get(&ProgramPoint::Suspension {
                block: edge.block,
                edge: edge.id,
            })
            .cloned()
            .unwrap_or_default();
        let mut continuation = slot_live
            .get(&ProgramPoint::ContinuationEntry {
                block: edge.continuation,
                edge: edge.id,
            })
            .cloned()
            .unwrap_or_default();
        if matches!(
            &child.result,
            ResultLayout::KnownType(result) if result.text.trim() != "void"
        ) {
            continuation.insert(edge.slot);
        }
        let mut live_points = BTreeSet::new();
        for phase in [
            ChildLifecyclePhase::Activated,
            ChildLifecyclePhase::Pending,
            ChildLifecyclePhase::Yielded,
            ChildLifecyclePhase::ErrorCopied,
            ChildLifecyclePhase::Canceled,
            ChildLifecyclePhase::InvalidStatus,
            ChildLifecyclePhase::ParentDrop,
        ] {
            insert_ownership_point(
                &mut live,
                &mut live_points,
                edge.block,
                edge.id,
                phase,
                child.id,
                &retained,
            );
        }
        insert_ownership_point(
            &mut live,
            &mut live_points,
            edge.block,
            edge.id,
            ChildLifecyclePhase::ReadyResultCopied,
            child.id,
            &continuation,
        );
        direct_children.insert(
            child.id,
            DirectChildBundle {
                child: child.id,
                edge: edge.id,
                result_slot: edge.slot,
                active_flag_independent: true,
                live_points,
            },
        );
    }

    let mut interference = BTreeSet::new();
    for storages in live.values() {
        let storages: Vec<_> = storages.iter().copied().collect();
        for (index, first) in storages.iter().enumerate() {
            for second in &storages[index + 1..] {
                if let Some(edge) = OwnershipInterference::new(*first, *second)
                    && (matches!(first, LogicalStorage::DirectChild(_))
                        || matches!(second, LogicalStorage::DirectChild(_)))
                {
                    interference.insert(edge);
                }
            }
        }
    }
    debug_assert!(live.values().all(|storages| {
        storages
            .iter()
            .filter(|storage| matches!(storage, LogicalStorage::DirectChild(_)))
            .count()
            <= 1
    }));

    ChildOwnershipAnalysis {
        live,
        direct_children,
        excluded_children,
        interference,
    }
}

fn insert_ownership_point(
    live: &mut BTreeMap<OwnershipPoint, BTreeSet<LogicalStorage>>,
    live_points: &mut BTreeSet<OwnershipPoint>,
    block: BlockId,
    edge: AwaitEdgeId,
    phase: ChildLifecyclePhase,
    child: ChildInstanceId,
    slots: &BTreeSet<AwaitSlotId>,
) {
    let point = OwnershipPoint { block, edge, phase };
    let mut storages: BTreeSet<_> = slots
        .iter()
        .copied()
        .map(LogicalStorage::AwaitResult)
        .collect();
    storages.insert(LogicalStorage::DirectChild(child));
    live.insert(point, storages);
    live_points.insert(point);
}

fn live_before_instruction(
    instruction: &CfgInstruction,
    mut live_after: BTreeSet<AwaitSlotId>,
) -> BTreeSet<AwaitSlotId> {
    if let CfgInstruction::AssignExpressionSlot { slot, .. } = instruction {
        live_after.remove(slot);
    }
    collect_instruction_uses(instruction, &mut live_after);
    live_after
}

fn live_before_terminator(
    terminator: &CfgTerminator,
    live_in: &[BTreeSet<AwaitSlotId>],
) -> BTreeSet<AwaitSlotId> {
    let mut live = BTreeSet::new();
    match terminator {
        CfgTerminator::Goto(edge) => live.extend(&live_in[edge.target.0 as usize]),
        CfgTerminator::Branch {
            consequence,
            alternative,
            ..
        } => {
            live.extend(&live_in[consequence.target.0 as usize]);
            live.extend(&live_in[alternative.target.0 as usize]);
        }
        CfgTerminator::Switch { cases, default, .. } => {
            for case in cases {
                live.extend(&live_in[case.edge.target.0 as usize]);
            }
            live.extend(&live_in[default.target.0 as usize]);
        }
        CfgTerminator::Suspend {
            slot, continuation, ..
        } => {
            live.extend(&live_in[continuation.target.0 as usize]);
            live.remove(slot);
        }
        CfgTerminator::Yield { continuation, .. } => {
            live.extend(&live_in[continuation.target.0 as usize]);
        }
        CfgTerminator::Open | CfgTerminator::Return { .. } | CfgTerminator::Unreachable => {}
    }
    collect_terminator_uses(terminator, &mut live);
    live
}

fn collect_instruction_uses(instruction: &CfgInstruction, slots: &mut BTreeSet<AwaitSlotId>) {
    match instruction {
        CfgInstruction::Source(_) | CfgInstruction::RunCleanups { .. } => {}
        CfgInstruction::Declaration(declaration) => {
            if let Some(initializer) = &declaration.initializer {
                collect_expression_uses(initializer, slots);
            }
        }
        CfgInstruction::AssignAwaitResult { slot, .. } => {
            slots.insert(*slot);
        }
        CfgInstruction::AssignExpression { expression, .. }
        | CfgInstruction::AssignExpressionSlot { expression, .. }
        | CfgInstruction::Evaluate(expression) => collect_expression_uses(expression, slots),
        CfgInstruction::RegisterDefer(defer) => collect_expression_uses(&defer.call, slots),
        CfgInstruction::PushCleanup(registration) => {
            for argument in &registration.arguments {
                collect_expression_uses(argument, slots);
            }
        }
    }
}

fn collect_terminator_uses(terminator: &CfgTerminator, slots: &mut BTreeSet<AwaitSlotId>) {
    match terminator {
        CfgTerminator::Branch { condition, .. } => collect_expression_uses(condition, slots),
        CfgTerminator::Switch {
            expression, cases, ..
        } => {
            collect_expression_uses(expression, slots);
            for case in cases {
                collect_expression_uses(&case.value, slots);
            }
        }
        CfgTerminator::Suspend { operand, .. } => collect_expression_uses(operand, slots),
        CfgTerminator::Yield { value, .. } | CfgTerminator::Return { value, .. } => {
            if let Some(value) = value {
                collect_value_uses(value, slots);
            }
        }
        CfgTerminator::Open | CfgTerminator::Goto(_) | CfgTerminator::Unreachable => {}
    }
}

fn collect_value_uses(value: &CfgValue, slots: &mut BTreeSet<AwaitSlotId>) {
    match value {
        CfgValue::Expression(expression) => collect_expression_uses(expression, slots),
        CfgValue::AwaitResult(slot) => {
            slots.insert(*slot);
        }
    }
}

fn collect_expression_uses(expression: &HirExpr, slots: &mut BTreeSet<AwaitSlotId>) {
    match &expression.kind {
        HirExprKind::Source(_) | HirExprKind::TaskRef { .. } => {}
        HirExprKind::AwaitResultRef(slot) => {
            slots.insert(*slot);
        }
        HirExprKind::Await(operand) | HirExprKind::Unary { operand, .. } => {
            collect_expression_uses(operand, slots);
        }
        HirExprKind::Yield(value) => {
            if let Some(value) = value {
                collect_expression_uses(value, slots);
            }
        }
        HirExprKind::AsyncCall { arguments, .. } => {
            for argument in arguments {
                collect_expression_uses(argument, slots);
            }
        }
        HirExprKind::Binary { left, right, .. }
        | HirExprKind::Comma { left, right }
        | HirExprKind::Assignment { left, right, .. } => {
            collect_expression_uses(left, slots);
            collect_expression_uses(right, slots);
        }
        HirExprKind::Conditional {
            condition,
            consequence,
            alternative,
        } => {
            collect_expression_uses(condition, slots);
            collect_expression_uses(consequence, slots);
            collect_expression_uses(alternative, slots);
        }
        HirExprKind::Call {
            function,
            arguments,
        } => {
            collect_expression_uses(function, slots);
            for argument in arguments {
                collect_expression_uses(argument, slots);
            }
        }
        HirExprKind::Composite { extensions, .. } => {
            for extension in extensions {
                collect_expression_uses(extension, slots);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::control_flow::build_cfg;
    use crate::coroutine::lower_coroutines;
    use crate::scope_exit::lower_scope_exits;
    use crate::semantic::{HirExpr, HirExprKind, build_hir};
    use crate::syntax::SyntaxParser;

    use super::*;

    fn analyze(source: &str) -> (CoroutineUnit, SlotLivenessUnit) {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("slot-liveness.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);
        let cfg = lower_scope_exits(&build_cfg(&hir));
        let coroutines = lower_coroutines(&cfg, "cr_");
        let liveness = analyze_slot_liveness(&coroutines);
        (coroutines, liveness)
    }

    #[test]
    fn source_text_that_looks_like_a_placeholder_is_not_a_slot_use() {
        let span = crate::syntax::SourceSpan {
            path: PathBuf::from("slot-liveness.cr"),
            start_byte: 0,
            end_byte: 20,
            start: crate::syntax::SourcePoint { row: 0, column: 0 },
            end: crate::syntax::SourcePoint { row: 0, column: 20 },
        };
        let source = HirExpr {
            kind: HirExprKind::Source("__cr_await_result_99".to_owned()),
            span: span.clone(),
        };
        let structured = HirExpr {
            kind: HirExprKind::AwaitResultRef(AwaitSlotId(7)),
            span,
        };
        let mut slots = BTreeSet::new();
        collect_expression_uses(&source, &mut slots);
        assert!(slots.is_empty());
        collect_expression_uses(&structured, &mut slots);
        assert_eq!(slots, BTreeSet::from([AwaitSlotId(7)]));
    }

    #[test]
    fn nested_await_results_are_live_across_later_suspension() {
        let (coroutines, analysis) = analyze(
            r#"
__async int next(int value) { return value; }
__async int combine(void) {
    return (__await next(1)) + (__await next(2));
}
"#,
        );
        let coroutine = coroutines
            .functions
            .iter()
            .find(|function| function.cfg.name == "combine")
            .expect("combine coroutine");
        let function = analysis
            .functions
            .iter()
            .find(|function| function.function_name == "combine")
            .expect("combine slot liveness");
        let suspensions: Vec<_> = coroutine
            .cfg
            .blocks
            .iter()
            .filter_map(|block| match &block.terminator {
                CfgTerminator::Suspend { edge, slot, .. } => Some((block.id, *edge, *slot)),
                _ => None,
            })
            .collect();
        assert_eq!(suspensions.len(), 2);
        let (block, edge, second_slot) = suspensions[1];
        let first_slot = suspensions[0].2;
        let live = function
            .live_at(ProgramPoint::Suspension { block, edge })
            .expect("second suspension point");
        assert!(live.contains(&first_slot));
        assert!(!live.contains(&second_slot));
    }

    #[test]
    fn branches_loops_short_circuit_and_comma_are_deterministic() {
        let (coroutines, first) = analyze(
            r#"
__async int next(int value) { return value; }
__async int flow(int flag) {
    int total = (__await next(1)) + (__await next(2));
    int selected = flag ? __await next(3) : __await next(4);
    int shorted = flag && __await next(5);
    int comma = (__await next(6), __await next(7));
    while (__await next(flag)) {
        total += selected + shorted + comma;
        if (total > 100) break;
    }
    return total;
}
"#,
        );
        let repeated = analyze_slot_liveness(&coroutines);
        assert_eq!(first, repeated);
        let flow = first
            .functions
            .iter()
            .find(|function| function.function_name == "flow")
            .expect("flow slot liveness");
        assert!(
            flow.live
                .keys()
                .any(|point| matches!(point, ProgramPoint::Suspension { .. }))
        );
        assert!(
            flow.live
                .keys()
                .any(|point| matches!(point, ProgramPoint::ContinuationEntry { .. }))
        );
        assert!(flow.live.values().any(|slots| slots.len() >= 2));
    }

    #[test]
    fn sequential_and_mutually_exclusive_direct_children_do_not_interfere() {
        let (_, sequential) = analyze(
            r#"
__async int child(int value) { return value; }
__async int sequential(void) {
    int first = __await child(1);
    int second = __await child(2);
    return first + second;
}
__async int exclusive(int flag) {
    return flag ? __await child(3) : __await child(4);
}
"#,
        );
        for name in ["sequential", "exclusive"] {
            let function = sequential
                .functions
                .iter()
                .find(|function| function.function_name == name)
                .expect("ownership function");
            let children: Vec<_> = function.direct_children.keys().copied().collect();
            assert_eq!(children.len(), 2, "{name}");
            assert!(!function.interferes(
                LogicalStorage::DirectChild(children[0]),
                LogicalStorage::DirectChild(children[1])
            ));
            assert!(function.direct_children.values().all(|child| {
                child.active_flag_independent
                    && child
                        .live_points
                        .iter()
                        .any(|point| point.phase == ChildLifecyclePhase::ParentDrop)
            }));
        }
    }

    #[test]
    fn later_child_interferes_with_earlier_live_result_and_own_copy() {
        let (_, analysis) = analyze(
            r#"
__async int child(int value) { return value; }
__async int nested(void) {
    return (__await child(1)) + (__await child(2));
}
__async int discarded(void) {
    __await child(3);
    return 0;
}
"#,
        );
        let function = analysis
            .functions
            .iter()
            .find(|function| function.function_name == "nested")
            .expect("nested ownership");
        let children: Vec<_> = function.direct_children.values().collect();
        assert_eq!(children.len(), 2);
        assert!(function.interferes(
            LogicalStorage::DirectChild(children[1].child),
            LogicalStorage::AwaitResult(children[0].result_slot)
        ));
        for child in children {
            assert!(function.interferes(
                LogicalStorage::DirectChild(child.child),
                LogicalStorage::AwaitResult(child.result_slot)
            ));
        }
        let discarded = analysis
            .functions
            .iter()
            .find(|function| function.function_name == "discarded")
            .expect("discarded ownership");
        let child = discarded
            .direct_children
            .values()
            .next()
            .expect("discarded direct child");
        assert!(discarded.interferes(
            LogicalStorage::DirectChild(child.child),
            LogicalStorage::AwaitResult(child.result_slot)
        ));
    }

    #[test]
    fn loop_carried_child_covers_every_active_terminal_observation() {
        let (_, analysis) = analyze(
            r#"
__async int child(int value) { return value; }
__async int repeated(int value) {
    while (__await child(value)) {
        value--;
    }
    return value;
}
"#,
        );
        let function = analysis
            .functions
            .iter()
            .find(|function| function.function_name == "repeated")
            .expect("repeated ownership");
        let child = function
            .direct_children
            .values()
            .next()
            .expect("loop child");
        let phases: BTreeSet<_> = child.live_points.iter().map(|point| point.phase).collect();
        assert_eq!(
            phases,
            BTreeSet::from([
                ChildLifecyclePhase::Activated,
                ChildLifecyclePhase::Pending,
                ChildLifecyclePhase::Yielded,
                ChildLifecyclePhase::ReadyResultCopied,
                ChildLifecyclePhase::ErrorCopied,
                ChildLifecyclePhase::Canceled,
                ChildLifecyclePhase::InvalidStatus,
                ChildLifecyclePhase::ParentDrop,
            ])
        );
        assert!(function.ownership_live.values().all(|storages| {
            storages
                .iter()
                .filter(|storage| matches!(storage, LogicalStorage::DirectChild(_)))
                .count()
                <= 1
        }));
    }

    #[test]
    fn declaration_owned_and_cleanup_retained_bindings_are_excluded() {
        let (_, analysis) = analyze(
            r#"
__async int child(int value) { return value; }
static void observe(cr_awaitable task) { (void)task; }
__async int bound(void) {
    __async int task = child(7);
    __defer observe(task);
    return __await task;
}
"#,
        );
        let function = analysis
            .functions
            .iter()
            .find(|function| function.function_name == "bound")
            .expect("bound ownership");
        assert!(function.direct_children.is_empty());
        assert_eq!(function.excluded_children.len(), 1);
        let reasons = function
            .excluded_children
            .values()
            .next()
            .expect("binding exclusions");
        assert_eq!(
            reasons,
            &BTreeSet::from([
                ChildReuseExclusion::DeclarationOwnedBinding,
                ChildReuseExclusion::CleanupRetained,
            ])
        );
    }
}
