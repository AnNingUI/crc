//! Validated logical-to-physical coroutine context layouts.

use std::collections::{BTreeMap, BTreeSet};

use crate::await_plan::{ChildInstanceId, ChildOrigin};
use crate::c_static_plan::{
    CChildPlan, CChildStorage, CChildTarget, CFunctionPlan, CStaticAwaitPlan,
};
use crate::config::{OptimizationLevel, TargetConfig};
use crate::control_flow::{BasicBlock, CfgInstruction, CfgTerminator, CfgValue};
use crate::liveness::{LivenessFunction, LivenessUnit};
use crate::semantic::{AwaitSlotId, DeclarationId, HirExpr, HirExprKind};
use crate::slot_liveness::{LogicalStorage, SlotLivenessFunction, SlotLivenessUnit};
use crate::syntax::{Diagnostic, DiagnosticSeverity, SourceSpan};
use crate::target_layout::{
    AggregateLayout, LayoutKnowledge, LayoutUnknownReason, TargetLayoutModel, TypeLayout,
};

macro_rules! layout_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);
    };
}

layout_id!(PhysicalSlotId);
layout_id!(UnionMemberId);

/// Stable identity for every private field in one coroutine context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LogicalFieldId {
    State,
    Status,
    Error,
    Cleanups,
    Lifted(DeclarationId),
    BindingActive(DeclarationId),
    BindingGeneration(DeclarationId),
    DirectChildPayload(ChildInstanceId),
    DirectChildActive(ChildInstanceId),
    Awaitable(AwaitSlotId),
    AwaitableActive(AwaitSlotId),
    AwaitResult(AwaitSlotId),
    Result,
    Yielded,
}

/// A validated C lvalue path relative to one task-context pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CAccessPath {
    Direct { field: String },
    UnionMember { slot: String, member: String },
}

impl CAccessPath {
    /// Renders one access through the supplied context pointer expression.
    #[must_use]
    pub fn render(&self, context: &str) -> String {
        match self {
            Self::Direct { field } => format!("{context}->{field}"),
            Self::UnionMember { slot, member } => format!("{context}->{slot}.{member}"),
        }
    }

    /// Returns the direct declaration name for an independent field.
    #[must_use]
    pub fn direct_field(&self) -> Option<&str> {
        match self {
            Self::Direct { field } => Some(field),
            Self::UnionMember { .. } => None,
        }
    }
}

/// One declarator-aware physical member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalMember {
    pub id: UnionMemberId,
    pub logical: LogicalFieldId,
    pub c_type: String,
    pub c_name: String,
    pub c_declaration: String,
}

/// The physical form of one context slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalSlotKind {
    Direct,
    Union,
}

/// One deterministic physical declaration in context order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalSlot {
    pub id: PhysicalSlotId,
    pub kind: PhysicalSlotKind,
    pub c_name: String,
    pub members: Vec<PhysicalMember>,
}

/// The complete placement of one logical field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldPlacement {
    pub slot: PhysicalSlotId,
    pub member: UnionMemberId,
    pub access_path: CAccessPath,
}

/// Why the identity planner retained one independent field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutDecisionReason {
    IdentityBaseline,
    RequiredIndependent,
    ReusedSameType,
    ReusedCrossType,
    ExcludedParameter,
    ExcludedAddressTaken,
    ExcludedCleanupRetained,
    ExcludedTaskBinding,
    ExcludedVolatileOrAtomic,
    RetainedSpeedUnknown,
    RetainedSpeedLarger,
}

/// One stable layout decision for future explain tooling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutDecision {
    pub field: LogicalFieldId,
    pub reason: LayoutDecisionReason,
}

/// The complete-function outcome of the `Size` placement pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SizeLayoutDecision {
    NotRequested,
    NoCandidate,
    Accepted {
        speed_size: u64,
        size_size: u64,
    },
    RetainedUnknown(LayoutUnknownReason),
    RetainedLarger {
        speed_size: u64,
        candidate_size: u64,
    },
}

/// The complete-function outcome of the bounded `Aggressive` placement pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggressiveLayoutDecision {
    NotRequested,
    RetainedSize {
        size_size: u64,
        explored_nodes: u64,
    },
    Accepted {
        size_size: u64,
        aggressive_size: u64,
        explored_nodes: u64,
    },
    BudgetExhausted {
        size_size: u64,
        explored_nodes: u64,
    },
    RetainedUnknown(LayoutUnknownReason),
}

/// A validated identity layout for one async function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionContextLayout {
    pub source_start: usize,
    pub function_name: String,
    pub task_type: String,
    pub fields: BTreeMap<LogicalFieldId, FieldPlacement>,
    pub slots: Vec<PhysicalSlot>,
    pub decisions: Vec<LayoutDecision>,
    pub await_result_types: BTreeMap<AwaitSlotId, String>,
    pub layout_knowledge: LayoutKnowledge<AggregateLayout>,
    pub size_decision: SizeLayoutDecision,
    pub aggressive_decision: AggressiveLayoutDecision,
}

impl FunctionContextLayout {
    /// Returns one required placement.
    #[must_use]
    pub fn placement(&self, field: LogicalFieldId) -> Option<&FieldPlacement> {
        self.fields.get(&field)
    }

    /// Renders one required logical field through a context pointer.
    #[must_use]
    pub fn access(&self, field: LogicalFieldId, context: &str) -> Option<String> {
        self.placement(field)
            .map(|placement| placement.access_path.render(context))
    }
}

/// All validated function layouts in one translation unit.
#[derive(Debug, Clone, Default)]
pub struct ContextLayoutPlan {
    pub functions: BTreeMap<usize, FunctionContextLayout>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Builds the physical context layout selected by one optimization level.
#[must_use]
pub fn build_context_layout(
    unit: &LivenessUnit,
    static_plan: Option<&CStaticAwaitPlan>,
    slot_liveness: &SlotLivenessUnit,
    optimization: OptimizationLevel,
    target: &TargetConfig,
    packing_barrier: bool,
) -> ContextLayoutPlan {
    let mut plan = build_identity_context_layout(unit, static_plan, slot_liveness);
    if !plan.diagnostics.is_empty() {
        return plan;
    }
    if optimization != OptimizationLevel::None {
        for (function_index, function) in unit.functions.iter().enumerate() {
            if !function.coroutine.cfg.is_async {
                continue;
            }
            let Some(layout) = plan
                .functions
                .get_mut(&function.coroutine.cfg.span.start_byte)
            else {
                continue;
            };
            let Some(slot_function) = slot_liveness
                .functions
                .iter()
                .find(|candidate| candidate.function_index == function_index)
            else {
                continue;
            };
            let function_plan = static_plan.and_then(|plan| {
                plan.functions.values().find(|candidate| {
                    candidate.source_start == function.coroutine.cfg.span.start_byte
                })
            });
            apply_speed_layout(layout, function_plan, slot_function);
        }
    }
    compute_layout_knowledge(&mut plan, target, packing_barrier);
    if matches!(
        optimization,
        OptimizationLevel::Size | OptimizationLevel::Aggressive
    ) {
        apply_size_layouts(
            &mut plan,
            unit,
            static_plan,
            slot_liveness,
            optimization,
            target,
            packing_barrier,
        );
        compute_layout_knowledge(&mut plan, target, packing_barrier);
    }
    plan
}

/// Builds a direct, one-slot-per-field identity layout.
#[must_use]
pub fn build_identity_context_layout(
    unit: &LivenessUnit,
    static_plan: Option<&CStaticAwaitPlan>,
    slot_liveness: &SlotLivenessUnit,
) -> ContextLayoutPlan {
    let mut plan = ContextLayoutPlan::default();
    for (function_index, function) in unit.functions.iter().enumerate() {
        if !function.coroutine.cfg.is_async {
            continue;
        }
        let Some(slot_function) = slot_liveness
            .functions
            .iter()
            .find(|candidate| candidate.function_index == function_index)
        else {
            plan.diagnostics.push(layout_diagnostic(
                "CRC8001",
                "async function is missing slot-liveness input",
                &function.coroutine.cfg.span,
            ));
            continue;
        };
        if slot_function.function_name != function.coroutine.cfg.name {
            plan.diagnostics.push(layout_diagnostic(
                "CRC8002",
                "slot-liveness function identity doesn't match layout input",
                &function.coroutine.cfg.span,
            ));
            continue;
        }
        let function_plan = static_plan.and_then(|plan| {
            plan.functions
                .values()
                .find(|candidate| candidate.source_start == function.coroutine.cfg.span.start_byte)
        });
        let layout = build_function_layout(function, function_plan);
        plan.functions.insert(layout.source_start, layout);
    }
    plan
}

/// Verifies complete placement and physical-slot consistency before emission.
#[must_use]
pub fn verify_context_layout(
    unit: &LivenessUnit,
    static_plan: Option<&CStaticAwaitPlan>,
    slot_liveness: &SlotLivenessUnit,
    plan: &ContextLayoutPlan,
) -> Vec<Diagnostic> {
    let expected = build_identity_context_layout(unit, static_plan, slot_liveness);
    let mut diagnostics = expected.diagnostics;
    for expected_function in expected.functions.values() {
        let Some(actual) = plan.functions.get(&expected_function.source_start) else {
            let function = unit
                .functions
                .iter()
                .find(|function| {
                    function.coroutine.cfg.span.start_byte == expected_function.source_start
                })
                .expect("expected layout function exists");
            diagnostics.push(layout_diagnostic(
                "CRC8003",
                "async function is missing a context layout",
                &function.coroutine.cfg.span,
            ));
            continue;
        };
        let function = unit
            .functions
            .iter()
            .find(|function| function.coroutine.cfg.span.start_byte == actual.source_start)
            .expect("layout function exists in liveness input");
        for field in expected_function.fields.keys() {
            if !actual.fields.contains_key(field) {
                diagnostics.push(layout_diagnostic(
                    "CRC8004",
                    "logical context field is missing a physical placement",
                    &function.coroutine.cfg.span,
                ));
            }
        }
        for (slot_index, slot) in actual.slots.iter().enumerate() {
            if slot.id != PhysicalSlotId(slot_index as u32)
                || (slot.kind == PhysicalSlotKind::Direct && slot.members.len() != 1)
                || (slot.kind == PhysicalSlotKind::Union && slot.members.len() < 2)
            {
                diagnostics.push(layout_diagnostic(
                    "CRC8008",
                    "physical context slot has an invalid shape or identity",
                    &function.coroutine.cfg.span,
                ));
            }
        }
        for (field, placement) in &actual.fields {
            let Some(slot) = actual.slots.get(placement.slot.0 as usize) else {
                diagnostics.push(layout_diagnostic(
                    "CRC8005",
                    "context placement references a missing physical slot",
                    &function.coroutine.cfg.span,
                ));
                continue;
            };
            let Some(member) = slot
                .members
                .iter()
                .find(|member| member.id == placement.member && member.logical == *field)
            else {
                diagnostics.push(layout_diagnostic(
                    "CRC8006",
                    "context placement references a missing physical member",
                    &function.coroutine.cfg.span,
                ));
                continue;
            };
            let valid_access = match (&slot.kind, &placement.access_path) {
                (PhysicalSlotKind::Direct, CAccessPath::Direct { field }) => {
                    field == &member.c_name
                }
                (
                    PhysicalSlotKind::Union,
                    CAccessPath::UnionMember {
                        slot: access_slot,
                        member: access_member,
                    },
                ) => access_slot == &slot.c_name && access_member == &member.c_name,
                _ => false,
            };
            if !valid_access {
                diagnostics.push(layout_diagnostic(
                    "CRC8009",
                    "context placement has an invalid C access path",
                    &function.coroutine.cfg.span,
                ));
            }
        }
        let slot_function = slot_liveness
            .functions
            .iter()
            .find(|candidate| candidate.function_index == function_index(unit, function));
        let function_plan = static_plan.and_then(|plan| {
            plan.functions
                .values()
                .find(|candidate| candidate.source_start == actual.source_start)
        });
        if let Some(slot_function) = slot_function {
            for slot in &actual.slots {
                if slot.kind == PhysicalSlotKind::Union
                    && slot.members.iter().any(|member| {
                        size_lifetime(member.logical, function_plan, function).is_none()
                    })
                {
                    diagnostics.push(layout_diagnostic(
                        "CRC8010",
                        "ineligible logical field is placed in reusable storage",
                        &function.coroutine.cfg.span,
                    ));
                }
                for (index, first) in slot.members.iter().enumerate() {
                    for second in &slot.members[index + 1..] {
                        let first_lifetime = size_lifetime(first.logical, function_plan, function);
                        let second_lifetime =
                            size_lifetime(second.logical, function_plan, function);
                        if first_lifetime
                            .zip(second_lifetime)
                            .is_some_and(|(first, second)| {
                                size_lifetimes_interfere(&first, &second, function, slot_function)
                            })
                        {
                            diagnostics.push(layout_diagnostic(
                                "CRC8007",
                                "interfering logical fields share one physical slot",
                                &function.coroutine.cfg.span,
                            ));
                        }
                    }
                }
            }
        }
    }
    diagnostics
}

fn function_index(unit: &LivenessUnit, function: &LivenessFunction) -> usize {
    unit.functions
        .iter()
        .position(|candidate| std::ptr::eq(candidate, function))
        .expect("layout function belongs to liveness input")
}

#[derive(Debug, Clone)]
struct ReuseCandidate {
    field: LogicalFieldId,
    storage: LogicalStorage,
    normalized_type: String,
}

fn apply_speed_layout(
    layout: &mut FunctionContextLayout,
    function_plan: Option<&CFunctionPlan>,
    slot_liveness: &SlotLivenessFunction,
) {
    let mut candidates = Vec::new();
    for slot in &layout.slots {
        let Some(member) = slot.members.first() else {
            continue;
        };
        let Some(storage) = reusable_storage(member.logical, function_plan) else {
            continue;
        };
        if let LogicalStorage::DirectChild(child) = storage
            && (slot_liveness.excluded_children.contains_key(&child)
                || !slot_liveness
                    .direct_children
                    .get(&child)
                    .is_some_and(|bundle| bundle.active_flag_independent))
        {
            continue;
        }
        candidates.push(ReuseCandidate {
            field: member.logical,
            storage,
            normalized_type: normalize_c_type(&member.c_type),
        });
    }

    let mut groups = Vec::<Vec<ReuseCandidate>>::new();
    for candidate in candidates {
        if let Some(group) = groups.iter_mut().find(|group| {
            group[0].normalized_type == candidate.normalized_type
                && group.iter().all(|placed| {
                    !storages_interfere(slot_liveness, placed.storage, candidate.storage)
                })
        }) {
            group.push(candidate);
        } else {
            groups.push(vec![candidate]);
        }
    }
    let groups: Vec<_> = groups.into_iter().filter(|group| group.len() > 1).collect();
    if groups.is_empty() {
        return;
    }

    let old_slots = std::mem::take(&mut layout.slots);
    let old_members: BTreeMap<_, _> = old_slots
        .iter()
        .flat_map(|slot| slot.members.iter())
        .map(|member| (member.logical, member.clone()))
        .collect();
    let group_by_field: BTreeMap<_, _> = groups
        .iter()
        .enumerate()
        .flat_map(|(group, members)| members.iter().map(move |member| (member.field, group)))
        .collect();
    let mut emitted_groups = BTreeSet::new();
    let mut union_ordinal = 0u32;
    for old_slot in old_slots {
        let member = old_slot
            .members
            .first()
            .expect("identity slot has one member");
        let Some(group_index) = group_by_field.get(&member.logical).copied() else {
            append_direct_slot(layout, member.clone());
            continue;
        };
        if !emitted_groups.insert(group_index) {
            continue;
        }
        let slot_id = PhysicalSlotId(layout.slots.len() as u32);
        let slot_name = format!("cr_slot_{union_ordinal}");
        union_ordinal += 1;
        let mut members = Vec::new();
        for (member_index, candidate) in groups[group_index].iter().enumerate() {
            let mut member = old_members
                .get(&candidate.field)
                .expect("reuse candidate has an identity member")
                .clone();
            member.id = UnionMemberId(member_index as u32);
            layout.fields.insert(
                candidate.field,
                FieldPlacement {
                    slot: slot_id,
                    member: member.id,
                    access_path: CAccessPath::UnionMember {
                        slot: slot_name.clone(),
                        member: member.c_name.clone(),
                    },
                },
            );
            if let Some(decision) = layout
                .decisions
                .iter_mut()
                .find(|decision| decision.field == candidate.field)
            {
                decision.reason = LayoutDecisionReason::ReusedSameType;
            }
            members.push(member);
        }
        layout.slots.push(PhysicalSlot {
            id: slot_id,
            kind: PhysicalSlotKind::Union,
            c_name: slot_name,
            members,
        });
    }
}

#[derive(Debug, Clone)]
struct SizeNode {
    original_slot: usize,
    members: Vec<PhysicalMember>,
    lifetimes: Vec<SizeLifetime>,
    weight: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SizeLifetime {
    Storage(LogicalStorage),
    Lifted(DeclarationId),
}

const AGGRESSIVE_NODE_BUDGET: u64 = 100_000;

#[derive(Debug, Clone)]
struct PlacementGraph {
    weights: Vec<u64>,
    access_weights: Vec<u64>,
    adjacency: Vec<BTreeSet<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PlacementCost {
    context_size: u64,
    slot_count: u64,
    access_complexity: u64,
}

#[derive(Debug, Clone, Copy)]
struct PlacementBounds {
    fixed_storage_size: u64,
    fixed_slot_count: u64,
    fixed_access_complexity: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AggressiveSearchResult {
    colors: Vec<usize>,
    cost: PlacementCost,
    explored_nodes: u64,
    exhausted: bool,
}

#[allow(clippy::too_many_arguments)]
fn apply_size_layouts(
    plan: &mut ContextLayoutPlan,
    unit: &LivenessUnit,
    static_plan: Option<&CStaticAwaitPlan>,
    slot_liveness: &SlotLivenessUnit,
    optimization: OptimizationLevel,
    target: &TargetConfig,
    packing_barrier: bool,
) {
    if packing_barrier {
        for layout in plan.functions.values_mut() {
            layout.size_decision =
                SizeLayoutDecision::RetainedUnknown(LayoutUnknownReason::PackingEnvironment);
            if optimization == OptimizationLevel::Aggressive {
                layout.aggressive_decision = AggressiveLayoutDecision::RetainedUnknown(
                    LayoutUnknownReason::PackingEnvironment,
                );
            }
        }
        return;
    }
    let model = match TargetLayoutModel::for_target(target) {
        LayoutKnowledge::Exact(model) => model,
        LayoutKnowledge::Unknown(reason) => {
            for layout in plan.functions.values_mut() {
                layout.size_decision = SizeLayoutDecision::RetainedUnknown(reason.clone());
                if optimization == OptimizationLevel::Aggressive {
                    layout.aggressive_decision =
                        AggressiveLayoutDecision::RetainedUnknown(reason.clone());
                }
            }
            return;
        }
    };
    let speed_plan = plan.clone();
    let generated = exact_generated_types(&speed_plan);
    for (function_index, function) in unit.functions.iter().enumerate() {
        if !function.coroutine.cfg.is_async {
            continue;
        }
        let source_start = function.coroutine.cfg.span.start_byte;
        let Some(speed_layout) = speed_plan.functions.get(&source_start) else {
            continue;
        };
        if let Some(layout) = plan.functions.get_mut(&source_start) {
            mark_lifted_exclusions(layout, function);
        }
        let Some(speed_size) = speed_layout
            .layout_knowledge
            .exact()
            .map(|layout| layout.size)
        else {
            if let Some(layout) = plan.functions.get_mut(&source_start) {
                let reason = LayoutUnknownReason::DependencyUnknown(speed_layout.task_type.clone());
                layout.size_decision = SizeLayoutDecision::RetainedUnknown(reason.clone());
                if optimization == OptimizationLevel::Aggressive {
                    layout.aggressive_decision = AggressiveLayoutDecision::RetainedUnknown(reason);
                }
            }
            continue;
        };
        let Some(slot_function) = slot_liveness
            .functions
            .iter()
            .find(|candidate| candidate.function_index == function_index)
        else {
            continue;
        };
        let function_plan = static_plan.and_then(|plan| {
            plan.functions
                .values()
                .find(|candidate| candidate.source_start == source_start)
        });
        let mut candidate = speed_layout.clone();
        mark_lifted_exclusions(&mut candidate, function);
        let mut nodes =
            collect_size_nodes(speed_layout, function, function_plan, &model, &generated);
        nodes.sort_by_key(|node| node.original_slot);
        if nodes.len() < 2 {
            candidate.size_decision = SizeLayoutDecision::NoCandidate;
            if optimization == OptimizationLevel::Aggressive {
                candidate.aggressive_decision = AggressiveLayoutDecision::RetainedSize {
                    size_size: speed_size,
                    explored_nodes: 0,
                };
            }
            if let Some(layout) = plan.functions.get_mut(&source_start) {
                *layout = candidate;
            }
            continue;
        }
        let graph = build_placement_graph(&nodes, function, slot_function);
        let colors = color_size_nodes(&graph);
        rebuild_size_layout(&mut candidate, &speed_layout.slots, &nodes, &colors);
        let candidate_knowledge = physical_context_layout(&candidate, &model, &generated);
        let candidate_size = match &candidate_knowledge {
            LayoutKnowledge::Exact(layout) => layout.size,
            LayoutKnowledge::Unknown(reason) => {
                if let Some(layout) = plan.functions.get_mut(&source_start) {
                    layout.size_decision = SizeLayoutDecision::RetainedUnknown(reason.clone());
                    if optimization == OptimizationLevel::Aggressive {
                        layout.aggressive_decision =
                            AggressiveLayoutDecision::RetainedUnknown(reason.clone());
                    }
                }
                continue;
            }
        };
        if candidate_size <= speed_size {
            candidate.layout_knowledge = candidate_knowledge;
            candidate.size_decision = SizeLayoutDecision::Accepted {
                speed_size,
                size_size: candidate_size,
            };
            if let Some(layout) = plan.functions.get_mut(&source_start) {
                *layout = candidate;
            }
        } else if let Some(layout) = plan.functions.get_mut(&source_start) {
            layout.size_decision = SizeLayoutDecision::RetainedLarger {
                speed_size,
                candidate_size,
            };
        }

        if optimization == OptimizationLevel::Aggressive {
            apply_aggressive_layout(
                plan,
                source_start,
                speed_layout,
                function,
                &nodes,
                &graph,
                &colors,
                &model,
                &generated,
            );
        }
    }
}

fn mark_lifted_exclusions(layout: &mut FunctionContextLayout, function: &LivenessFunction) {
    for field in &function.lifted_fields {
        let reason = if field.is_parameter {
            Some(LayoutDecisionReason::ExcludedParameter)
        } else if field.is_task {
            Some(LayoutDecisionReason::ExcludedTaskBinding)
        } else if field.cleanup_retained {
            Some(LayoutDecisionReason::ExcludedCleanupRetained)
        } else if field.address_taken {
            Some(LayoutDecisionReason::ExcludedAddressTaken)
        } else if field.volatile_or_atomic {
            Some(LayoutDecisionReason::ExcludedVolatileOrAtomic)
        } else {
            None
        };
        if let Some(reason) = reason
            && let Some(decision) = layout
                .decisions
                .iter_mut()
                .find(|decision| decision.field == LogicalFieldId::Lifted(field.declaration))
        {
            decision.reason = reason;
        }
    }
}

fn exact_generated_types(plan: &ContextLayoutPlan) -> BTreeMap<String, TypeLayout> {
    plan.functions
        .values()
        .filter_map(|layout| {
            layout.layout_knowledge.exact().map(|knowledge| {
                (
                    layout.task_type.clone(),
                    TypeLayout {
                        size: knowledge.size,
                        align: knowledge.align,
                    },
                )
            })
        })
        .collect()
}

fn collect_size_nodes(
    layout: &FunctionContextLayout,
    function: &LivenessFunction,
    function_plan: Option<&CFunctionPlan>,
    model: &TargetLayoutModel,
    generated: &BTreeMap<String, TypeLayout>,
) -> Vec<SizeNode> {
    let mut nodes = Vec::new();
    for (slot_index, slot) in layout.slots.iter().enumerate() {
        let lifetimes: Vec<_> = slot
            .members
            .iter()
            .filter_map(|member| size_lifetime(member.logical, function_plan, function))
            .collect();
        if lifetimes.len() != slot.members.len() {
            continue;
        }
        if slot.members.iter().any(|member| {
            model
                .type_layout(&member.c_type, generated)
                .exact()
                .is_none()
        }) {
            continue;
        }
        let weight = slot
            .members
            .iter()
            .filter_map(
                |member| match model.type_layout(&member.c_type, generated) {
                    LayoutKnowledge::Exact(layout) => Some(layout.size),
                    LayoutKnowledge::Unknown(_) => None,
                },
            )
            .max()
            .unwrap_or_default();
        nodes.push(SizeNode {
            original_slot: slot_index,
            members: slot.members.clone(),
            lifetimes,
            weight,
        });
    }
    nodes
}

fn size_lifetime(
    logical: LogicalFieldId,
    function_plan: Option<&CFunctionPlan>,
    function: &LivenessFunction,
) -> Option<SizeLifetime> {
    match logical {
        LogicalFieldId::Lifted(declaration) => function
            .lifted_fields
            .iter()
            .find(|field| field.declaration == declaration)
            .filter(|field| {
                !field.is_parameter
                    && !field.is_task
                    && !field.address_taken
                    && !field.cleanup_retained
                    && !field.volatile_or_atomic
            })
            .map(|_| SizeLifetime::Lifted(declaration)),
        LogicalFieldId::AwaitResult(slot) => {
            Some(SizeLifetime::Storage(LogicalStorage::AwaitResult(slot)))
        }
        LogicalFieldId::DirectChildPayload(child) => {
            Some(SizeLifetime::Storage(LogicalStorage::DirectChild(child)))
        }
        LogicalFieldId::Awaitable(slot) => function_plan
            .into_iter()
            .flat_map(|plan| plan.children.values())
            .find(|child| {
                matches!(child.origin, ChildOrigin::Direct(_))
                    && matches!(child.target, CChildTarget::Dynamic(_))
                    && child.effective_storage == CChildStorage::Opaque(slot)
            })
            .map(|child| SizeLifetime::Storage(LogicalStorage::DirectChild(child.instance))),
        _ => None,
    }
}

fn build_placement_graph(
    nodes: &[SizeNode],
    function: &LivenessFunction,
    slot_function: &SlotLivenessFunction,
) -> PlacementGraph {
    let mut adjacency = vec![BTreeSet::new(); nodes.len()];
    for first in 0..nodes.len() {
        for second in first + 1..nodes.len() {
            if nodes[first].lifetimes.iter().any(|left| {
                nodes[second]
                    .lifetimes
                    .iter()
                    .any(|right| size_lifetimes_interfere(left, right, function, slot_function))
            }) {
                adjacency[first].insert(second);
                adjacency[second].insert(first);
            }
        }
    }
    PlacementGraph {
        weights: nodes.iter().map(|node| node.weight).collect(),
        access_weights: nodes.iter().map(|node| node.members.len() as u64).collect(),
        adjacency,
    }
}

fn color_size_nodes(graph: &PlacementGraph) -> Vec<usize> {
    let nodes = graph.weights.len();
    let mut colors = vec![usize::MAX; nodes];
    for _ in 0..nodes {
        let mut selected = None;
        for index in 0..nodes {
            if colors[index] != usize::MAX {
                continue;
            }
            let saturation = graph.adjacency[index]
                .iter()
                .filter_map(|neighbor| {
                    (colors[*neighbor] != usize::MAX).then_some(colors[*neighbor])
                })
                .collect::<BTreeSet<_>>()
                .len();
            let key = (
                saturation,
                graph.weights[index],
                graph.adjacency[index].len(),
                usize::MAX - index,
            );
            if selected.is_none_or(|(best_key, _)| key > best_key) {
                selected = Some((key, index));
            }
        }
        let Some((_, index)) = selected else {
            break;
        };
        let mut color = 0;
        loop {
            if graph.adjacency[index]
                .iter()
                .all(|neighbor| colors[*neighbor] != color)
            {
                colors[index] = color;
                break;
            }
            color += 1;
        }
    }
    colors
}

#[cfg(test)]
fn placement_graph_for_test(
    weights: &[(usize, u64)],
    access_weights: &BTreeMap<usize, u64>,
    edges: &[(usize, usize)],
) -> PlacementGraph {
    let mut ordered = weights.to_vec();
    ordered.sort_by_key(|(key, _)| *key);
    let key_to_index: BTreeMap<_, _> = ordered
        .iter()
        .enumerate()
        .map(|(index, (key, _))| (*key, index))
        .collect();
    let mut adjacency = vec![BTreeSet::new(); ordered.len()];
    for (left, right) in edges {
        let left = key_to_index[left];
        let right = key_to_index[right];
        adjacency[left].insert(right);
        adjacency[right].insert(left);
    }
    PlacementGraph {
        weights: ordered.iter().map(|(_, weight)| *weight).collect(),
        access_weights: ordered
            .iter()
            .map(|(key, _)| access_weights.get(key).copied().unwrap_or(1))
            .collect(),
        adjacency,
    }
}

#[allow(clippy::too_many_arguments)]
fn apply_aggressive_layout(
    plan: &mut ContextLayoutPlan,
    source_start: usize,
    speed_layout: &FunctionContextLayout,
    function: &LivenessFunction,
    nodes: &[SizeNode],
    graph: &PlacementGraph,
    size_colors: &[usize],
    model: &TargetLayoutModel,
    generated: &BTreeMap<String, TypeLayout>,
) {
    let Some(size_layout) = plan.functions.get(&source_start).cloned() else {
        return;
    };
    let Some(size_size) = size_layout
        .layout_knowledge
        .exact()
        .map(|knowledge| knowledge.size)
    else {
        if let Some(layout) = plan.functions.get_mut(&source_start) {
            layout.aggressive_decision = AggressiveLayoutDecision::RetainedUnknown(
                LayoutUnknownReason::DependencyUnknown(size_layout.task_type),
            );
        }
        return;
    };
    let initial_colors = if matches!(
        size_layout.size_decision,
        SizeLayoutDecision::Accepted { .. }
    ) {
        size_colors.to_vec()
    } else {
        (0..nodes.len()).collect()
    };
    let incumbent_cost = placement_cost(&size_layout, size_size);
    let bounds = match placement_bounds(speed_layout, nodes, model, generated) {
        LayoutKnowledge::Exact(bounds) => bounds,
        LayoutKnowledge::Unknown(reason) => {
            if let Some(layout) = plan.functions.get_mut(&source_start) {
                layout.aggressive_decision = AggressiveLayoutDecision::RetainedUnknown(reason);
            }
            return;
        }
    };
    let result = search_aggressive_placement(
        graph,
        &initial_colors,
        incumbent_cost.clone(),
        bounds,
        AGGRESSIVE_NODE_BUDGET,
        |colors| {
            let mut candidate = speed_layout.clone();
            mark_lifted_exclusions(&mut candidate, function);
            rebuild_size_layout(&mut candidate, &speed_layout.slots, nodes, colors);
            physical_context_layout(&candidate, model, generated)
                .exact()
                .map(|knowledge| placement_cost(&candidate, knowledge.size))
        },
    );
    if result.exhausted {
        if let Some(layout) = plan.functions.get_mut(&source_start) {
            layout.aggressive_decision = AggressiveLayoutDecision::BudgetExhausted {
                size_size,
                explored_nodes: result.explored_nodes,
            };
        }
        return;
    }
    if result.cost >= incumbent_cost {
        if let Some(layout) = plan.functions.get_mut(&source_start) {
            layout.aggressive_decision = AggressiveLayoutDecision::RetainedSize {
                size_size,
                explored_nodes: result.explored_nodes,
            };
        }
        return;
    }

    let mut candidate = speed_layout.clone();
    mark_lifted_exclusions(&mut candidate, function);
    rebuild_size_layout(&mut candidate, &speed_layout.slots, nodes, &result.colors);
    let candidate_knowledge = physical_context_layout(&candidate, model, generated);
    let aggressive_size = candidate_knowledge
        .exact()
        .map(|knowledge| knowledge.size)
        .expect("completed aggressive placement retains exact layout knowledge");
    assert!(aggressive_size <= size_size);
    candidate.layout_knowledge = candidate_knowledge;
    candidate.size_decision = size_layout.size_decision;
    candidate.aggressive_decision = AggressiveLayoutDecision::Accepted {
        size_size,
        aggressive_size,
        explored_nodes: result.explored_nodes,
    };
    if let Some(layout) = plan.functions.get_mut(&source_start) {
        *layout = candidate;
    }
}

fn placement_cost(layout: &FunctionContextLayout, context_size: u64) -> PlacementCost {
    PlacementCost {
        context_size,
        slot_count: layout.slots.len() as u64,
        access_complexity: layout
            .slots
            .iter()
            .filter(|slot| slot.kind == PhysicalSlotKind::Union)
            .map(|slot| slot.members.len() as u64)
            .sum(),
    }
}

fn placement_bounds(
    layout: &FunctionContextLayout,
    nodes: &[SizeNode],
    model: &TargetLayoutModel,
    generated: &BTreeMap<String, TypeLayout>,
) -> LayoutKnowledge<PlacementBounds> {
    let node_slots: BTreeSet<_> = nodes.iter().map(|node| node.original_slot).collect();
    let mut fixed_storage_size = 0u64;
    let mut fixed_slot_count = 0u64;
    let mut fixed_access_complexity = 0u64;
    for (slot_index, slot) in layout.slots.iter().enumerate() {
        if node_slots.contains(&slot_index) {
            continue;
        }
        let mut slot_size = 0u64;
        for member in &slot.members {
            match model.type_layout(&member.c_type, generated) {
                LayoutKnowledge::Exact(knowledge) => {
                    slot_size = slot_size.max(knowledge.size);
                }
                LayoutKnowledge::Unknown(reason) => {
                    return LayoutKnowledge::Unknown(reason);
                }
            }
        }
        fixed_storage_size = match fixed_storage_size.checked_add(slot_size) {
            Some(size) => size,
            None => {
                return LayoutKnowledge::Unknown(LayoutUnknownReason::ArithmeticOverflow);
            }
        };
        fixed_slot_count += 1;
        if slot.kind == PhysicalSlotKind::Union {
            fixed_access_complexity += slot.members.len() as u64;
        }
    }
    LayoutKnowledge::Exact(PlacementBounds {
        fixed_storage_size,
        fixed_slot_count,
        fixed_access_complexity,
    })
}

fn search_aggressive_placement<F>(
    graph: &PlacementGraph,
    initial_colors: &[usize],
    incumbent_cost: PlacementCost,
    bounds: PlacementBounds,
    node_budget: u64,
    evaluate: F,
) -> AggressiveSearchResult
where
    F: FnMut(&[usize]) -> Option<PlacementCost>,
{
    let mut search = AggressiveSearcher {
        graph,
        bounds,
        node_budget,
        explored_nodes: 0,
        exhausted: false,
        best_colors: canonical_colors(initial_colors),
        best_cost: incumbent_cost,
        evaluate,
    };
    let mut colors = vec![usize::MAX; graph.weights.len()];
    search.visit(&mut colors);
    AggressiveSearchResult {
        colors: search.best_colors,
        cost: search.best_cost,
        explored_nodes: search.explored_nodes,
        exhausted: search.exhausted,
    }
}

struct AggressiveSearcher<'a, F> {
    graph: &'a PlacementGraph,
    bounds: PlacementBounds,
    node_budget: u64,
    explored_nodes: u64,
    exhausted: bool,
    best_colors: Vec<usize>,
    best_cost: PlacementCost,
    evaluate: F,
}

impl<F> AggressiveSearcher<'_, F>
where
    F: FnMut(&[usize]) -> Option<PlacementCost>,
{
    fn visit(&mut self, colors: &mut [usize]) {
        if self.exhausted {
            return;
        }
        if self.explored_nodes >= self.node_budget {
            self.exhausted = true;
            return;
        }
        self.explored_nodes += 1;
        if self.lower_bound(colors) > self.best_cost {
            return;
        }
        let Some(node) = self.select_node(colors) else {
            if let Some(cost) = (self.evaluate)(colors) {
                let stable = canonical_colors(colors);
                if cost < self.best_cost || (cost == self.best_cost && stable < self.best_colors) {
                    self.best_cost = cost;
                    self.best_colors = stable;
                }
            }
            return;
        };
        for color in self.candidate_colors(node, colors) {
            colors[node] = color;
            self.visit(colors);
            colors[node] = usize::MAX;
            if self.exhausted {
                return;
            }
        }
    }

    fn select_node(&self, colors: &[usize]) -> Option<usize> {
        let mut selected = None;
        for index in 0..colors.len() {
            if colors[index] != usize::MAX {
                continue;
            }
            let saturation = self.graph.adjacency[index]
                .iter()
                .filter_map(|neighbor| {
                    (colors[*neighbor] != usize::MAX).then_some(colors[*neighbor])
                })
                .collect::<BTreeSet<_>>()
                .len();
            let key = (
                saturation,
                self.graph.weights[index],
                self.graph.adjacency[index].len(),
                usize::MAX - index,
            );
            if selected.is_none_or(|(best, _)| key > best) {
                selected = Some((key, index));
            }
        }
        selected.map(|(_, index)| index)
    }

    fn candidate_colors(&self, node: usize, colors: &[usize]) -> Vec<usize> {
        let used = colors
            .iter()
            .copied()
            .filter(|color| *color != usize::MAX)
            .max()
            .map_or(0, |color| color + 1);
        let mut maxima = vec![0u64; used];
        for (index, color) in colors.iter().copied().enumerate() {
            if color != usize::MAX {
                maxima[color] = maxima[color].max(self.graph.weights[index]);
            }
        }
        let mut candidates = Vec::new();
        for color in 0..=used {
            if color < used
                && self.graph.adjacency[node]
                    .iter()
                    .any(|neighbor| colors[*neighbor] == color)
            {
                continue;
            }
            let previous = maxima.get(color).copied().unwrap_or_default();
            let resulting = previous.max(self.graph.weights[node]);
            candidates.push((resulting - previous, color == used, resulting, color));
        }
        candidates.sort_unstable();
        candidates
            .into_iter()
            .map(|(_, _, _, color)| color)
            .collect()
    }

    fn lower_bound(&self, colors: &[usize]) -> PlacementCost {
        let used = colors
            .iter()
            .copied()
            .filter(|color| *color != usize::MAX)
            .max()
            .map_or(0, |color| color + 1);
        let mut maxima = vec![0u64; used];
        let mut access = vec![0u64; used];
        for (index, color) in colors.iter().copied().enumerate() {
            if color != usize::MAX {
                maxima[color] = maxima[color].max(self.graph.weights[index]);
                access[color] += self.graph.access_weights[index];
            }
        }
        PlacementCost {
            context_size: self.bounds.fixed_storage_size + maxima.into_iter().sum::<u64>(),
            slot_count: self.bounds.fixed_slot_count + used as u64,
            access_complexity: self.bounds.fixed_access_complexity
                + access
                    .into_iter()
                    .filter(|members| *members > 1)
                    .sum::<u64>(),
        }
    }
}

fn canonical_colors(colors: &[usize]) -> Vec<usize> {
    let mut remap = BTreeMap::new();
    let mut next = 0usize;
    colors
        .iter()
        .map(|color| {
            *remap.entry(*color).or_insert_with(|| {
                let value = next;
                next += 1;
                value
            })
        })
        .collect()
}

fn size_lifetimes_interfere(
    first: &SizeLifetime,
    second: &SizeLifetime,
    function: &LivenessFunction,
    slot_function: &SlotLivenessFunction,
) -> bool {
    match (first, second) {
        (SizeLifetime::Storage(first), SizeLifetime::Storage(second)) => {
            storages_interfere(slot_function, *first, *second)
        }
        (SizeLifetime::Lifted(first), SizeLifetime::Lifted(second)) => function
            .live_points
            .values()
            .any(|live| live.contains(first) && live.contains(second)),
        (SizeLifetime::Lifted(declaration), SizeLifetime::Storage(storage))
        | (SizeLifetime::Storage(storage), SizeLifetime::Lifted(declaration)) => match storage {
            LogicalStorage::AwaitResult(slot) => slot_function.live.iter().any(|(point, live)| {
                live.contains(slot)
                    && function
                        .live_points
                        .get(point)
                        .is_some_and(|declarations| declarations.contains(declaration))
            }),
            LogicalStorage::DirectChild(child) => {
                function
                    .coroutine
                    .await_plan
                    .edges
                    .iter()
                    .find(|edge| edge.instance == *child)
                    .and_then(|edge| {
                        function.live_points.get(
                            &crate::slot_liveness::ProgramPoint::BeforeTerminator(edge.block),
                        )
                    })
                    .is_some_and(|declarations| declarations.contains(declaration))
            }
        },
    }
}

fn rebuild_size_layout(
    layout: &mut FunctionContextLayout,
    original_slots: &[PhysicalSlot],
    nodes: &[SizeNode],
    colors: &[usize],
) {
    let node_by_slot: BTreeMap<_, _> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.original_slot, index))
        .collect();
    let mut emitted = BTreeSet::new();
    let mut rebuilt = Vec::new();
    let mut union_index = 0u32;
    for (slot_index, original) in original_slots.iter().enumerate() {
        if let Some(node_index) = node_by_slot.get(&slot_index).copied() {
            let color = colors[node_index];
            if !emitted.insert(color) {
                continue;
            }
            let members: Vec<_> = nodes
                .iter()
                .enumerate()
                .filter(|(index, _)| colors[*index] == color)
                .flat_map(|(_, node)| node.members.clone())
                .collect();
            if members.len() > 1 {
                let slot_name = format!("cr_slot_{union_index}");
                union_index += 1;
                let slot_id = PhysicalSlotId(rebuilt.len() as u32);
                let members: Vec<_> = members
                    .into_iter()
                    .enumerate()
                    .map(|(member, mut value)| {
                        value.id = UnionMemberId(member as u32);
                        layout.fields.insert(
                            value.logical,
                            FieldPlacement {
                                slot: slot_id,
                                member: value.id,
                                access_path: CAccessPath::UnionMember {
                                    slot: slot_name.clone(),
                                    member: value.c_name.clone(),
                                },
                            },
                        );
                        value
                    })
                    .collect();
                rebuilt.push(PhysicalSlot {
                    id: slot_id,
                    kind: PhysicalSlotKind::Union,
                    c_name: slot_name,
                    members,
                });
            } else {
                append_direct_slot_to(&mut rebuilt, &mut layout.fields, members[0].clone());
            }
        } else if original.kind == PhysicalSlotKind::Union {
            let slot_name = format!("cr_slot_{union_index}");
            union_index += 1;
            append_union_slot_to(
                &mut rebuilt,
                &mut layout.fields,
                original.members.clone(),
                slot_name,
            );
        } else {
            append_direct_slot_to(
                &mut rebuilt,
                &mut layout.fields,
                original.members[0].clone(),
            );
        }
    }
    layout.slots = rebuilt;
    let reused: BTreeMap<_, _> = layout
        .slots
        .iter()
        .filter(|slot| slot.kind == PhysicalSlotKind::Union)
        .flat_map(|slot| {
            let types: BTreeSet<_> = slot
                .members
                .iter()
                .map(|member| normalize_c_type(&member.c_type))
                .collect();
            let reason = if types.len() > 1 {
                LayoutDecisionReason::ReusedCrossType
            } else {
                LayoutDecisionReason::ReusedSameType
            };
            slot.members
                .iter()
                .map(move |member| (member.logical, reason))
        })
        .collect();
    for decision in &mut layout.decisions {
        if let Some(reason) = reused.get(&decision.field) {
            decision.reason = *reason;
        }
    }
}

fn append_direct_slot_to(
    slots: &mut Vec<PhysicalSlot>,
    fields: &mut BTreeMap<LogicalFieldId, FieldPlacement>,
    mut member: PhysicalMember,
) {
    let slot = PhysicalSlotId(slots.len() as u32);
    member.id = UnionMemberId(0);
    fields.insert(
        member.logical,
        FieldPlacement {
            slot,
            member: member.id,
            access_path: CAccessPath::Direct {
                field: member.c_name.clone(),
            },
        },
    );
    slots.push(PhysicalSlot {
        id: slot,
        kind: PhysicalSlotKind::Direct,
        c_name: member.c_name.clone(),
        members: vec![member],
    });
}

fn append_union_slot_to(
    slots: &mut Vec<PhysicalSlot>,
    fields: &mut BTreeMap<LogicalFieldId, FieldPlacement>,
    members: Vec<PhysicalMember>,
    slot_name: String,
) {
    let slot = PhysicalSlotId(slots.len() as u32);
    let members: Vec<_> = members
        .into_iter()
        .enumerate()
        .map(|(member, mut value)| {
            value.id = UnionMemberId(member as u32);
            fields.insert(
                value.logical,
                FieldPlacement {
                    slot,
                    member: value.id,
                    access_path: CAccessPath::UnionMember {
                        slot: slot_name.clone(),
                        member: value.c_name.clone(),
                    },
                },
            );
            value
        })
        .collect();
    slots.push(PhysicalSlot {
        id: slot,
        kind: PhysicalSlotKind::Union,
        c_name: slot_name,
        members,
    });
}

fn append_direct_slot(layout: &mut FunctionContextLayout, mut member: PhysicalMember) {
    let slot = PhysicalSlotId(layout.slots.len() as u32);
    member.id = UnionMemberId(0);
    layout.fields.insert(
        member.logical,
        FieldPlacement {
            slot,
            member: member.id,
            access_path: CAccessPath::Direct {
                field: member.c_name.clone(),
            },
        },
    );
    layout.slots.push(PhysicalSlot {
        id: slot,
        kind: PhysicalSlotKind::Direct,
        c_name: member.c_name.clone(),
        members: vec![member],
    });
}

fn reusable_storage(
    field: LogicalFieldId,
    function_plan: Option<&CFunctionPlan>,
) -> Option<LogicalStorage> {
    match field {
        LogicalFieldId::AwaitResult(slot) => Some(LogicalStorage::AwaitResult(slot)),
        LogicalFieldId::DirectChildPayload(child) => Some(LogicalStorage::DirectChild(child)),
        LogicalFieldId::Awaitable(slot) => function_plan?
            .children
            .values()
            .find(|child| {
                matches!(child.origin, ChildOrigin::Direct(_))
                    && matches!(child.target, CChildTarget::Dynamic(_))
                    && child.effective_storage == CChildStorage::Opaque(slot)
            })
            .map(|child| LogicalStorage::DirectChild(child.instance)),
        _ => None,
    }
}

fn storages_interfere(
    liveness: &SlotLivenessFunction,
    first: LogicalStorage,
    second: LogicalStorage,
) -> bool {
    if first == second {
        return true;
    }
    if let (LogicalStorage::AwaitResult(first), LogicalStorage::AwaitResult(second)) =
        (first, second)
        && liveness
            .live
            .values()
            .any(|live| live.contains(&first) && live.contains(&second))
    {
        return true;
    }
    liveness.interferes(first, second)
}

fn normalize_c_type(c_type: &str) -> String {
    c_type.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn build_function_layout(
    function: &LivenessFunction,
    function_plan: Option<&CFunctionPlan>,
) -> FunctionContextLayout {
    let symbols = TypeEnvironment::new(function);
    let await_result_types = await_result_types(function, &symbols);
    let typed_await_slots = typed_await_slots(function, function_plan);
    let mut builder = IdentityLayoutBuilder::new(function);
    builder.add_required(LogicalFieldId::State, "uint32_t", "state");
    builder.add_required(LogicalFieldId::Status, "cr_poll_status", "status");
    builder.add_required(LogicalFieldId::Error, "cr_error", "error");
    builder.add_required(LogicalFieldId::Cleanups, "cr_cleanup_stack", "cleanups");

    for field in &function.lifted_fields {
        let binding = typed_binding_child(function_plan, field.declaration);
        let (field_type, declaration) = binding
            .and_then(|child| match (&child.target, child.effective_storage) {
                (CChildTarget::Static(callee), CChildStorage::Embedded(_)) => Some((
                    callee.task_type.clone(),
                    format!("{} {}", callee.task_type, field.field_name),
                )),
                (CChildTarget::Static(callee), CChildStorage::Boxed(_)) => Some((
                    format!("{} *", callee.task_type),
                    format!("{} *{}", callee.task_type, field.field_name),
                )),
                _ => None,
            })
            .unwrap_or_else(|| {
                (
                    field.ty.text.trim().to_owned(),
                    render_typed_name(field.ty.text.trim(), &field.field_name),
                )
            });
        builder.add_with_declaration(
            LogicalFieldId::Lifted(field.declaration),
            &field_type,
            &field.field_name,
            &declaration,
            field.is_parameter || field.address_taken || field.is_task,
        );
        if field.is_task {
            builder.add_required(
                LogicalFieldId::BindingActive(field.declaration),
                "bool",
                &format!("{}_active", field.field_name),
            );
            builder.add_required(
                LogicalFieldId::BindingGeneration(field.declaration),
                "uint64_t",
                &format!("{}_generation", field.field_name),
            );
        }
    }

    if let Some(function_plan) = function_plan {
        for child in function_plan.children.values() {
            let (CChildTarget::Static(callee), ChildOrigin::Direct(_)) =
                (&child.target, child.origin)
            else {
                continue;
            };
            let (c_type, c_name, declaration) = match child.effective_storage {
                CChildStorage::Embedded(slot) => {
                    let name = format!("cr_child_{}", slot.0);
                    (
                        callee.task_type.clone(),
                        name.clone(),
                        format!("{} {name}", callee.task_type),
                    )
                }
                CChildStorage::Boxed(slot) => {
                    let name = format!("cr_boxed_{}", slot.0);
                    (
                        format!("{} *", callee.task_type),
                        name.clone(),
                        format!("{} *{name}", callee.task_type),
                    )
                }
                CChildStorage::Opaque(_) => continue,
            };
            builder.add_with_declaration(
                LogicalFieldId::DirectChildPayload(child.instance),
                &c_type,
                &c_name,
                &declaration,
                false,
            );
            builder.add_required(
                LogicalFieldId::DirectChildActive(child.instance),
                "bool",
                &format!("{c_name}_active"),
            );
        }
    }

    for slot in &function.coroutine.await_slots {
        if !typed_await_slots.contains(&slot.id) {
            builder.add(
                LogicalFieldId::Awaitable(slot.id),
                "cr_awaitable",
                &format!("cr_await_{}", slot.id.0),
                false,
            );
            builder.add_required(
                LogicalFieldId::AwaitableActive(slot.id),
                "bool",
                &format!("cr_await_{}_active", slot.id.0),
            );
        }
        if let Some(ty) = await_result_types.get(&slot.id) {
            builder.add(
                LogicalFieldId::AwaitResult(slot.id),
                ty,
                &format!("cr_await_{}_result", slot.id.0),
                false,
            );
        }
    }

    if function.coroutine.cfg.return_type.text.trim() != "void" {
        let return_type = function.coroutine.cfg.return_type.text.trim();
        builder.add_required(LogicalFieldId::Result, return_type, "result");
        builder.add_required(LogicalFieldId::Yielded, return_type, "yielded");
    }
    builder.finish(await_result_types)
}

struct IdentityLayoutBuilder<'function> {
    function: &'function LivenessFunction,
    fields: BTreeMap<LogicalFieldId, FieldPlacement>,
    slots: Vec<PhysicalSlot>,
    decisions: Vec<LayoutDecision>,
}

impl<'function> IdentityLayoutBuilder<'function> {
    fn new(function: &'function LivenessFunction) -> Self {
        Self {
            function,
            fields: BTreeMap::new(),
            slots: Vec::new(),
            decisions: Vec::new(),
        }
    }

    fn add_required(&mut self, logical: LogicalFieldId, c_type: &str, c_name: &str) {
        self.add(logical, c_type, c_name, true);
    }

    fn add(&mut self, logical: LogicalFieldId, c_type: &str, c_name: &str, required: bool) {
        self.add_with_declaration(
            logical,
            c_type,
            c_name,
            &render_typed_name(c_type, c_name),
            required,
        );
    }

    fn add_with_declaration(
        &mut self,
        logical: LogicalFieldId,
        c_type: &str,
        c_name: &str,
        c_declaration: &str,
        required: bool,
    ) {
        let slot = PhysicalSlotId(self.slots.len() as u32);
        let member = UnionMemberId(0);
        let access_path = CAccessPath::Direct {
            field: c_name.to_owned(),
        };
        assert!(
            self.fields
                .insert(
                    logical,
                    FieldPlacement {
                        slot,
                        member,
                        access_path,
                    },
                )
                .is_none(),
            "identity layout contains duplicate logical field {logical:?}"
        );
        self.slots.push(PhysicalSlot {
            id: slot,
            kind: PhysicalSlotKind::Direct,
            c_name: c_name.to_owned(),
            members: vec![PhysicalMember {
                id: member,
                logical,
                c_type: c_type.trim().to_owned(),
                c_name: c_name.to_owned(),
                c_declaration: c_declaration.to_owned(),
            }],
        });
        self.decisions.push(LayoutDecision {
            field: logical,
            reason: if required {
                LayoutDecisionReason::RequiredIndependent
            } else {
                LayoutDecisionReason::IdentityBaseline
            },
        });
    }

    fn finish(self, await_result_types: BTreeMap<AwaitSlotId, String>) -> FunctionContextLayout {
        FunctionContextLayout {
            source_start: self.function.coroutine.cfg.span.start_byte,
            function_name: self.function.coroutine.cfg.name.clone(),
            task_type: self.function.coroutine.task_type.clone(),
            fields: self.fields,
            slots: self.slots,
            decisions: self.decisions,
            await_result_types,
            layout_knowledge: LayoutKnowledge::Unknown(LayoutUnknownReason::UnsupportedTarget),
            size_decision: SizeLayoutDecision::NotRequested,
            aggressive_decision: AggressiveLayoutDecision::NotRequested,
        }
    }
}

fn compute_layout_knowledge(
    plan: &mut ContextLayoutPlan,
    target: &TargetConfig,
    packing_barrier: bool,
) {
    if packing_barrier {
        for layout in plan.functions.values_mut() {
            layout.layout_knowledge =
                LayoutKnowledge::Unknown(LayoutUnknownReason::PackingEnvironment);
        }
        return;
    }
    let model = match TargetLayoutModel::for_target(target) {
        LayoutKnowledge::Exact(model) => model,
        LayoutKnowledge::Unknown(reason) => {
            for layout in plan.functions.values_mut() {
                layout.layout_knowledge = LayoutKnowledge::Unknown(reason.clone());
            }
            return;
        }
    };
    let mut generated = BTreeMap::<String, TypeLayout>::new();
    for _ in 0..=plan.functions.len() {
        let mut changed = false;
        for layout in plan.functions.values_mut() {
            let knowledge = physical_context_layout(layout, &model, &generated);
            if let LayoutKnowledge::Exact(aggregate) = &knowledge {
                let value = TypeLayout {
                    size: aggregate.size,
                    align: aggregate.align,
                };
                if generated.insert(layout.task_type.clone(), value) != Some(value) {
                    changed = true;
                }
            }
            layout.layout_knowledge = knowledge;
        }
        if !changed {
            break;
        }
    }
}

fn physical_context_layout(
    layout: &FunctionContextLayout,
    model: &TargetLayoutModel,
    generated: &BTreeMap<String, TypeLayout>,
) -> LayoutKnowledge<AggregateLayout> {
    let mut fields = Vec::new();
    for slot in &layout.slots {
        let slot_layout = match slot.kind {
            PhysicalSlotKind::Direct => {
                let member = slot
                    .members
                    .first()
                    .expect("direct physical slot has one member");
                model.type_layout(&member.c_type, generated)
            }
            PhysicalSlotKind::Union => {
                let mut members = Vec::new();
                for member in &slot.members {
                    let member_layout = match model.type_layout(&member.c_type, generated) {
                        LayoutKnowledge::Exact(layout) => layout,
                        LayoutKnowledge::Unknown(reason) => {
                            return LayoutKnowledge::Unknown(reason);
                        }
                    };
                    members.push(member_layout);
                }
                model.union_layout(members)
            }
        };
        let LayoutKnowledge::Exact(slot_layout) = slot_layout else {
            let LayoutKnowledge::Unknown(reason) = slot_layout else {
                unreachable!("layout match is exhaustive");
            };
            return LayoutKnowledge::Unknown(reason);
        };
        fields.push(slot_layout);
    }
    model.struct_layout(fields)
}

fn render_typed_name(ty: &str, name: &str) -> String {
    if let Some(index) = ty.find("(*)") {
        let mut rendered = ty.to_owned();
        rendered.replace_range(index..index + 3, &format!("(*{name})"));
        return rendered;
    }
    if let Some(index) = ty.find('[') {
        return format!("{} {name}{}", ty[..index].trim_end(), &ty[index..]);
    }
    format!("{ty} {name}")
}

fn typed_await_slots(
    function: &LivenessFunction,
    function_plan: Option<&CFunctionPlan>,
) -> BTreeSet<AwaitSlotId> {
    function_plan
        .into_iter()
        .flat_map(|plan| plan.edge_children.iter())
        .filter_map(|(edge, instance)| {
            let child = function_plan?.children.get(instance)?;
            is_typed_static_child(child)
                .then(|| {
                    function
                        .coroutine
                        .await_plan
                        .edges
                        .iter()
                        .find(|planned| planned.id == *edge)
                        .map(|planned| planned.slot)
                })
                .flatten()
        })
        .collect()
}

fn is_typed_static_child(child: &CChildPlan) -> bool {
    matches!(child.target, CChildTarget::Static(_))
        && matches!(
            child.effective_storage,
            CChildStorage::Embedded(_) | CChildStorage::Boxed(_)
        )
}

fn typed_binding_child(
    function_plan: Option<&CFunctionPlan>,
    declaration: DeclarationId,
) -> Option<&CChildPlan> {
    function_plan?.children.values().find(|child| {
        matches!(child.origin, ChildOrigin::Binding(binding) if binding == declaration)
            && is_typed_static_child(child)
    })
}

struct DeclarationType {
    id: DeclarationId,
    name: String,
    ty: String,
    scope: crate::semantic::ScopeId,
    start_byte: usize,
}

struct TypeEnvironment {
    declarations: Vec<DeclarationType>,
}

impl TypeEnvironment {
    fn new(function: &LivenessFunction) -> Self {
        let mut declarations: Vec<_> = function
            .coroutine
            .cfg
            .parameters
            .iter()
            .map(|parameter| DeclarationType {
                id: parameter.id,
                name: parameter.name.clone(),
                ty: parameter.ty.text.clone(),
                scope: parameter.scope,
                start_byte: parameter.span.start_byte,
            })
            .collect();
        for block in &function.coroutine.cfg.blocks {
            for instruction in &block.instructions {
                if let CfgInstruction::Declaration(declaration) = instruction {
                    declarations.push(DeclarationType {
                        id: declaration.id,
                        name: declaration.name.clone(),
                        ty: declaration.ty.text.clone(),
                        scope: declaration.scope,
                        start_byte: declaration.span.start_byte,
                    });
                }
            }
        }
        declarations.sort_by_key(|declaration| declaration.id);
        declarations.dedup_by_key(|declaration| declaration.id);
        Self { declarations }
    }

    fn resolve(
        &self,
        name: &str,
        byte: usize,
        scopes: &[crate::semantic::ScopeId],
    ) -> Option<&DeclarationType> {
        self.declarations
            .iter()
            .filter(|declaration| {
                declaration.name == name
                    && declaration.start_byte <= byte
                    && scopes.contains(&declaration.scope)
            })
            .max_by_key(|declaration| {
                (
                    scopes
                        .iter()
                        .position(|scope| *scope == declaration.scope)
                        .unwrap_or_default(),
                    declaration.start_byte,
                )
            })
    }
}

fn await_result_types(
    function: &LivenessFunction,
    symbols: &TypeEnvironment,
) -> BTreeMap<AwaitSlotId, String> {
    let mut types = BTreeMap::new();
    for block in &function.coroutine.cfg.blocks {
        for instruction in &block.instructions {
            if let CfgInstruction::AssignAwaitResult {
                destination, slot, ..
            } = instruction
                && let Some(declaration) = symbols
                    .declarations
                    .iter()
                    .find(|declaration| declaration.id == *destination)
            {
                types.insert(*slot, declaration.ty.trim().to_owned());
            }
            if let CfgInstruction::AssignExpressionSlot { slot, ty, .. } = instruction {
                types.insert(*slot, ty.text.trim().to_owned());
            }
        }
        if let CfgTerminator::Return {
            value: Some(CfgValue::AwaitResult(slot)),
            ..
        } = &block.terminator
        {
            types.insert(
                *slot,
                function.coroutine.cfg.return_type.text.trim().to_owned(),
            );
        }
        if let CfgTerminator::Suspend { operand, slot, .. } = &block.terminator
            && !types.contains_key(slot)
            && let Some(ty) = infer_expression_type(operand, block, symbols)
        {
            types.insert(*slot, ty);
        }
    }
    types
}

fn infer_expression_type(
    expression: &HirExpr,
    block: &BasicBlock,
    symbols: &TypeEnvironment,
) -> Option<String> {
    if let HirExprKind::AsyncCall { result_type, .. } = &expression.kind {
        return Some(result_type.text.trim().to_owned());
    }
    if let HirExprKind::TaskRef { declaration, .. } = &expression.kind {
        return symbols
            .declarations
            .iter()
            .find(|symbol| symbol.id == *declaration)
            .map(|symbol| symbol.ty.trim().to_owned());
    }
    let HirExprKind::Source(source) = &expression.kind else {
        return None;
    };
    let source = source.trim();
    if is_identifier(source) {
        return symbols
            .resolve(source, expression.span.start_byte, &block.scope_stack)
            .map(|declaration| declaration.ty.trim().to_owned());
    }
    if source.starts_with('"') {
        return Some("const char *".to_owned());
    }
    if source.starts_with('\'') {
        return Some("int".to_owned());
    }
    if source
        .chars()
        .all(|character| character.is_ascii_digit() || "xXabcdefABCDEFuUlL".contains(character))
    {
        return Some("long long".to_owned());
    }
    None
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn layout_diagnostic(code: &'static str, message: &str, span: &SourceSpan) -> Diagnostic {
    Diagnostic {
        code,
        severity: DiagnosticSeverity::Error,
        message: message.to_owned(),
        primary_span: span.clone(),
        related: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::await_plan::{AwaitStorageFunction, plan_await_storage};
    use crate::c_declaration_env::build_c_declaration_environment;
    use crate::c_emitter::{CEmitterConfig, emit_translation_unit};
    use crate::c_static_plan::build_c_static_await_plan;
    use crate::control_flow::build_cfg;
    use crate::coroutine::lower_coroutines;
    use crate::liveness::analyze_liveness;
    use crate::runtime_abi::runtime_header;
    use crate::scope_exit::lower_scope_exits;
    use crate::semantic::build_hir_with_symbol_index;
    use crate::slot_liveness::analyze_slot_liveness;
    use crate::symbol_index::{TranslationUnitId, build_local_async_symbol_index};
    use crate::syntax::SyntaxParser;

    use super::*;

    fn planned(source: &str) -> (LivenessUnit, CStaticAwaitPlan, SlotLivenessUnit) {
        let path = PathBuf::from("layout.cr");
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(path.clone(), source)
            .expect("layout source parses");
        let symbols = build_local_async_symbol_index(&syntax, "cr_");
        assert!(symbols.diagnostics.is_empty(), "{:?}", symbols.diagnostics);
        let hir = build_hir_with_symbol_index(&syntax, &symbols.index, Path::new("layout.cr"));
        let cfg = lower_scope_exits(&build_cfg(&hir));
        let mut coroutines = lower_coroutines(&cfg, "cr_");
        let mut functions: Vec<_> = coroutines
            .functions
            .iter_mut()
            .filter(|function| function.cfg.is_async)
            .map(|function| AwaitStorageFunction {
                caller: symbols
                    .index
                    .resolve(Path::new("layout.cr"), &function.cfg.name)
                    .expect("async symbol resolves")
                    .symbol
                    .id,
                translation_unit: TranslationUnitId("layout.cr".to_owned()),
                source_start: function.cfg.span.start_byte,
                plan: &mut function.await_plan,
            })
            .collect();
        let storage = plan_await_storage(&mut functions, &symbols.index);
        assert!(storage.diagnostics.is_empty(), "{:?}", storage.diagnostics);
        drop(functions);
        let slot_liveness = analyze_slot_liveness(&coroutines);
        let liveness = analyze_liveness(&coroutines);
        let declarations = build_c_declaration_environment(&syntax);
        let static_plan = build_c_static_await_plan(
            &liveness,
            Path::new("layout.cr"),
            &symbols.index,
            &declarations,
        );
        assert!(
            static_plan.diagnostics.is_empty(),
            "{:?}",
            static_plan.diagnostics
        );
        (liveness, static_plan, slot_liveness)
    }

    fn abstract_placement_cost(graph: &PlacementGraph, colors: &[usize]) -> PlacementCost {
        let used = colors.iter().copied().max().map_or(0, |color| color + 1);
        let mut maxima = vec![0u64; used];
        let mut members = vec![0u64; used];
        for (index, color) in colors.iter().copied().enumerate() {
            maxima[color] = maxima[color].max(graph.weights[index]);
            members[color] += graph.access_weights[index];
        }
        PlacementCost {
            context_size: maxima.into_iter().sum(),
            slot_count: used as u64,
            access_complexity: members.into_iter().filter(|count| *count > 1).sum(),
        }
    }

    fn exhaustive_placement(graph: &PlacementGraph) -> (Vec<usize>, PlacementCost) {
        fn visit(
            graph: &PlacementGraph,
            index: usize,
            colors: &mut [usize],
            used: usize,
            best: &mut Option<(Vec<usize>, PlacementCost)>,
        ) {
            if index == colors.len() {
                let stable = canonical_colors(colors);
                let cost = abstract_placement_cost(graph, &stable);
                if best.as_ref().is_none_or(|(best_colors, best_cost)| {
                    cost < *best_cost || (cost == *best_cost && stable < *best_colors)
                }) {
                    *best = Some((stable, cost));
                }
                return;
            }
            for color in 0..=used {
                if color < used
                    && graph.adjacency[index]
                        .iter()
                        .any(|neighbor| *neighbor < index && colors[*neighbor] == color)
                {
                    continue;
                }
                colors[index] = color;
                visit(graph, index + 1, colors, used.max(color + 1), best);
                colors[index] = usize::MAX;
            }
        }

        let mut colors = vec![usize::MAX; graph.weights.len()];
        let mut best = None;
        visit(graph, 0, &mut colors, 0, &mut best);
        best.expect("finite interference graph has a placement")
    }

    fn search_test_graph(graph: &PlacementGraph, budget: u64) -> AggressiveSearchResult {
        let greedy = color_size_nodes(graph);
        search_aggressive_placement(
            graph,
            &greedy,
            abstract_placement_cost(graph, &greedy),
            PlacementBounds {
                fixed_storage_size: 0,
                fixed_slot_count: 0,
                fixed_access_complexity: 0,
            },
            budget,
            |colors| Some(abstract_placement_cost(graph, colors)),
        )
    }

    #[test]
    fn aggressive_branch_and_bound_matches_exhaustive_weighted_optimum() {
        let graph = placement_graph_for_test(
            &[(0, 1), (1, 1), (2, 4), (3, 4)],
            &BTreeMap::new(),
            &[(0, 1), (0, 3), (1, 2)],
        );
        let greedy = color_size_nodes(&graph);
        assert_eq!(abstract_placement_cost(&graph, &greedy).context_size, 8);

        let result = search_test_graph(&graph, 10_000);
        let exhaustive = exhaustive_placement(&graph);
        assert!(!result.exhausted);
        assert_eq!((result.colors, result.cost), exhaustive);
        assert_eq!(exhaustive.1.context_size, 6);
    }

    #[test]
    fn aggressive_uses_access_complexity_after_size_and_slot_count() {
        let graph = placement_graph_for_test(
            &[(0, 1), (1, 1), (2, 1), (3, 1), (4, 1)],
            &BTreeMap::new(),
            &[(0, 1), (0, 2)],
        );
        let greedy = color_size_nodes(&graph);
        assert_eq!(
            abstract_placement_cost(&graph, &greedy),
            PlacementCost {
                context_size: 2,
                slot_count: 2,
                access_complexity: 5,
            }
        );

        let result = search_test_graph(&graph, 10_000);
        assert!(!result.exhausted);
        assert_eq!(result.cost.context_size, 2);
        assert_eq!(result.cost.slot_count, 2);
        assert_eq!(result.cost.access_complexity, 4);
    }

    #[test]
    fn aggressive_budget_exhaustion_retains_the_exact_incumbent() {
        let graph = placement_graph_for_test(
            &[(0, 1), (1, 1), (2, 4), (3, 4)],
            &BTreeMap::new(),
            &[(0, 1), (0, 3), (1, 2)],
        );
        let greedy = color_size_nodes(&graph);
        let incumbent = abstract_placement_cost(&graph, &greedy);
        let result = search_test_graph(&graph, 1);
        assert!(result.exhausted);
        assert_eq!(result.explored_nodes, 1);
        assert_eq!(result.colors, canonical_colors(&greedy));
        assert_eq!(result.cost, incumbent);
    }

    #[test]
    fn aggressive_search_is_stable_under_reordered_graph_inputs() {
        let access = BTreeMap::from([(0, 1), (1, 1), (2, 1), (3, 1)]);
        let first = placement_graph_for_test(
            &[(0, 1), (1, 1), (2, 4), (3, 4)],
            &access,
            &[(0, 1), (0, 3), (1, 2)],
        );
        let reordered = placement_graph_for_test(
            &[(3, 4), (1, 1), (0, 1), (2, 4)],
            &access,
            &[(2, 1), (3, 0), (1, 0)],
        );
        assert_eq!(first.weights, reordered.weights);
        assert_eq!(first.adjacency, reordered.adjacency);

        let expected = search_test_graph(&first, 10_000);
        for _ in 0..4 {
            let repeated = search_test_graph(&reordered, 10_000);
            assert_eq!(repeated, expected);
        }

        let parallel: Vec<_> = (0..4)
            .map(|_| {
                let graph = reordered.clone();
                std::thread::spawn(move || search_test_graph(&graph, 10_000))
            })
            .collect();
        for worker in parallel {
            assert_eq!(
                worker.join().expect("parallel placement completes"),
                expected
            );
        }
    }

    #[test]
    fn identity_layout_places_every_private_field_independently() {
        let (liveness, static_plan, slots) = planned(
            r#"
__async int child(int value) { return value; }
static cr_awaitable external_value(void);
__async int layout(int parameter) {
    int local = parameter;
    __async int binding = child(local);
    int first = __await child(local);
    int second = __await external_value();
    return first + second + __await binding;
}
"#,
        );
        let plan = build_identity_context_layout(&liveness, Some(&static_plan), &slots);
        assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
        let layout = plan
            .functions
            .values()
            .find(|layout| layout.function_name == "layout")
            .expect("layout function plan");
        assert!(layout.fields.contains_key(&LogicalFieldId::State));
        assert!(layout.fields.contains_key(&LogicalFieldId::Status));
        assert!(layout.fields.contains_key(&LogicalFieldId::Error));
        assert!(layout.fields.contains_key(&LogicalFieldId::Cleanups));
        assert!(
            layout
                .fields
                .keys()
                .any(|field| matches!(field, LogicalFieldId::Lifted(_)))
        );
        assert!(
            layout
                .fields
                .keys()
                .any(|field| matches!(field, LogicalFieldId::BindingActive(_)))
        );
        assert!(
            layout
                .fields
                .keys()
                .any(|field| matches!(field, LogicalFieldId::BindingGeneration(_)))
        );
        assert!(
            layout
                .fields
                .keys()
                .any(|field| matches!(field, LogicalFieldId::DirectChildPayload(_)))
        );
        assert!(
            layout
                .fields
                .keys()
                .any(|field| matches!(field, LogicalFieldId::DirectChildActive(_)))
        );
        assert!(
            layout
                .fields
                .keys()
                .any(|field| matches!(field, LogicalFieldId::Awaitable(_)))
        );
        assert!(
            layout
                .fields
                .keys()
                .any(|field| matches!(field, LogicalFieldId::AwaitableActive(_)))
        );
        assert!(
            layout
                .fields
                .keys()
                .any(|field| matches!(field, LogicalFieldId::AwaitResult(_)))
        );
        assert!(layout.fields.contains_key(&LogicalFieldId::Result));
        assert!(layout.fields.contains_key(&LogicalFieldId::Yielded));
        assert_eq!(layout.fields.len(), layout.slots.len());
        assert!(
            layout
                .slots
                .iter()
                .all(|slot| { slot.kind == PhysicalSlotKind::Direct && slot.members.len() == 1 })
        );
        assert_eq!(
            verify_context_layout(&liveness, Some(&static_plan), &slots, &plan),
            Vec::new()
        );
    }

    #[test]
    fn missing_placement_is_an_internal_layout_diagnostic() {
        let (liveness, static_plan, slots) = planned(
            r#"
__async int child(void) { return 1; }
__async int parent(void) { return __await child(); }
"#,
        );
        let mut plan = build_identity_context_layout(&liveness, Some(&static_plan), &slots);
        let parent = plan
            .functions
            .values_mut()
            .find(|layout| layout.function_name == "parent")
            .expect("parent layout");
        parent.fields.remove(&LogicalFieldId::State);
        let diagnostics = verify_context_layout(&liveness, Some(&static_plan), &slots, &plan);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CRC8004")
        );
    }

    #[test]
    fn speed_reuses_sequential_same_type_children_results_and_awaitables() {
        let (liveness, static_plan, slots) = planned(
            r#"
__async int child(int value) { return value; }
static cr_awaitable external_value(int value);
__async int layout(void) {
    int first = __await child(1);
    int second = __await child(2);
    int third = __await external_value(3);
    int fourth = __await external_value(4);
    return first + second + third + fourth;
}
"#,
        );
        let plan = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Speed,
            &TargetConfig::Host,
            false,
        );
        assert!(plan.diagnostics.is_empty(), "{:?}", plan.diagnostics);
        let layout = plan
            .functions
            .values()
            .find(|layout| layout.function_name == "layout")
            .expect("layout function plan");
        let unions: Vec<_> = layout
            .slots
            .iter()
            .filter(|slot| slot.kind == PhysicalSlotKind::Union)
            .collect();
        assert!(unions.iter().any(|slot| {
            slot.members.len() == 2
                && slot
                    .members
                    .iter()
                    .all(|member| matches!(member.logical, LogicalFieldId::DirectChildPayload(_)))
        }));
        assert!(unions.iter().any(|slot| {
            slot.members.len() == 4
                && slot
                    .members
                    .iter()
                    .all(|member| matches!(member.logical, LogicalFieldId::AwaitResult(_)))
        }));
        assert!(unions.iter().any(|slot| {
            slot.members.len() == 2
                && slot
                    .members
                    .iter()
                    .all(|member| matches!(member.logical, LogicalFieldId::Awaitable(_)))
        }));
        assert!(layout.fields.keys().any(|field| {
            matches!(field, LogicalFieldId::DirectChildActive(_))
                && layout
                    .placement(*field)
                    .is_some_and(|placement| placement.access_path.direct_field().is_some())
        }));
        assert_eq!(
            verify_context_layout(&liveness, Some(&static_plan), &slots, &plan),
            Vec::new()
        );
    }

    #[test]
    fn speed_keeps_simultaneously_live_results_in_different_slots() {
        let (liveness, static_plan, slots) = planned(
            r#"
__async int child(int value) { return value; }
__async int layout(void) {
    return (__await child(1)) + (__await child(2));
}
"#,
        );
        let plan = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Speed,
            &TargetConfig::Host,
            false,
        );
        let layout = plan
            .functions
            .values()
            .find(|layout| layout.function_name == "layout")
            .expect("layout function plan");
        let result_slots: Vec<_> = layout
            .fields
            .iter()
            .filter_map(|(field, placement)| {
                matches!(field, LogicalFieldId::AwaitResult(_)).then_some(placement.slot)
            })
            .collect();
        assert_eq!(result_slots.len(), 2);
        assert_ne!(result_slots[0], result_slots[1]);
        assert_eq!(
            verify_context_layout(&liveness, Some(&static_plan), &slots, &plan),
            Vec::new()
        );

        let size = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Size,
            &TargetConfig::Host,
            false,
        );
        let layout = size
            .functions
            .values()
            .find(|layout| layout.function_name == "layout")
            .expect("size layout function plan");
        let result_slots: Vec<_> = layout
            .fields
            .iter()
            .filter_map(|(field, placement)| {
                matches!(field, LogicalFieldId::AwaitResult(_)).then_some(placement.slot)
            })
            .collect();
        assert_eq!(result_slots.len(), 2);
        assert_ne!(result_slots[0], result_slots[1]);
        assert_eq!(
            verify_context_layout(&liveness, Some(&static_plan), &slots, &size),
            Vec::new()
        );
    }

    #[test]
    fn host_context_layout_reports_exact_size_and_alignment_for_known_fields() {
        let (liveness, static_plan, slots) = planned(
            r#"
__async int child(int value) { return value; }
__async int layout(void) {
    int first = __await child(1);
    return first;
}
"#,
        );
        let plan = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Speed,
            &TargetConfig::Host,
            false,
        );
        let layout = plan
            .functions
            .values()
            .find(|layout| layout.function_name == "layout")
            .expect("layout function plan");
        let knowledge = layout
            .layout_knowledge
            .exact()
            .expect("known host context layout");
        assert!(knowledge.size > 0);
        assert!(knowledge.align > 0);
        assert_eq!(knowledge.offsets.len(), layout.slots.len());
    }

    #[test]
    fn unknown_target_and_packing_keep_layout_knowledge_conservative() {
        let (liveness, static_plan, slots) = planned(
            r#"
__async int layout(void) { return 1; }
"#,
        );
        let custom = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Speed,
            &TargetConfig::Custom("vendor-target".to_owned()),
            false,
        );
        let custom_layout = custom.functions.values().next().expect("custom layout");
        assert!(matches!(
            custom_layout.layout_knowledge,
            LayoutKnowledge::Unknown(LayoutUnknownReason::UnsupportedTarget)
        ));

        let packed = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Speed,
            &TargetConfig::Host,
            true,
        );
        let packed_layout = packed.functions.values().next().expect("packed layout");
        assert!(matches!(
            packed_layout.layout_knowledge,
            LayoutKnowledge::Unknown(LayoutUnknownReason::PackingEnvironment)
        ));
    }

    #[test]
    fn generated_task_size_matches_exact_host_context_model() {
        let source = r#"
#include "cr_runtime.h"
#include <stdio.h>
static void consume(int value) { (void)value; }
__async int layout(void) {
    char first[64] = {1};
    __yield 1;
    consume(first[0]);
    long long second[8] = {2};
    __yield 2;
    return (int)second[0];
}
int main(void) {
    printf("%zu\n", sizeof(cr_layout_task));
    return 0;
}
"#;
        let (liveness, static_plan, slots) = planned(source);
        let plan = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Size,
            &TargetConfig::Host,
            false,
        );
        let expected = plan
            .functions
            .values()
            .find(|layout| layout.function_name == "layout")
            .and_then(|layout| layout.layout_knowledge.exact())
            .map(|layout| layout.size)
            .expect("exact generated layout size");
        let emission = emit_translation_unit(
            source,
            &liveness,
            Some(&static_plan),
            &slots,
            &plan,
            &CEmitterConfig {
                target: TargetConfig::Host,
                optimization: OptimizationLevel::Size,
                ..CEmitterConfig::default()
            },
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );

        let compiler = ["clang", "gcc"]
            .into_iter()
            .find(|compiler| {
                Command::new(compiler)
                    .arg("--version")
                    .output()
                    .is_ok_and(|output| output.status.success())
            })
            .expect("Clang or GCC is required for generated-layout tests");
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("cr_runtime.h"), runtime_header()).expect("runtime header");
        fs::write(directory.path().join("layout.c"), emission.source)
            .expect("generated layout source");
        let executable = if cfg!(windows) {
            "layout.exe"
        } else {
            "layout"
        };
        let compilation = Command::new(compiler)
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "layout.c", "-o"])
            .arg(executable)
            .current_dir(directory.path())
            .output()
            .expect("native compiler runs");
        assert!(
            compilation.status.success(),
            "{}",
            String::from_utf8_lossy(&compilation.stderr)
        );
        let execution = Command::new(directory.path().join(executable))
            .current_dir(directory.path())
            .output()
            .expect("generated layout executable runs");
        assert!(execution.status.success());
        let actual: u64 = String::from_utf8(execution.stdout)
            .expect("layout output is UTF-8")
            .trim()
            .parse()
            .expect("numeric generated layout size");
        assert_eq!(actual, expected);
    }

    #[test]
    fn size_reuses_nonoverlapping_lifted_array_storage() {
        let source = r#"
static void consume(int value) { (void)value; }
__async int layout(void) {
    char first[64] = {1};
    __yield 1;
    consume(first[0]);
    long long second[8] = {2};
    __yield 2;
    return (int)second[0];
}
"#;
        let (liveness, static_plan, slots) = planned(source);
        let plan = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Size,
            &TargetConfig::Host,
            false,
        );
        let layout = plan.functions.values().next().expect("size layout");
        assert!(
            matches!(layout.size_decision, SizeLayoutDecision::Accepted { .. }),
            "{layout:#?}"
        );
        assert!(
            layout.slots.iter().any(|slot| {
                slot.kind == PhysicalSlotKind::Union
                    && slot
                        .members
                        .iter()
                        .any(|member| member.c_type.contains("char"))
                    && slot
                        .members
                        .iter()
                        .any(|member| member.c_type.contains("long long"))
            }),
            "{layout:#?}"
        );

        let aggressive_host = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Aggressive,
            &TargetConfig::Host,
            false,
        );
        let aggressive_host_repeated = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Aggressive,
            &TargetConfig::Host,
            false,
        );
        assert_eq!(
            aggressive_host.functions,
            aggressive_host_repeated.functions
        );
        let aggressive_host_layout = aggressive_host
            .functions
            .values()
            .next()
            .expect("host Aggressive layout");
        assert!(matches!(
            aggressive_host_layout.aggressive_decision,
            AggressiveLayoutDecision::Accepted {
                explored_nodes: 1..,
                ..
            } | AggressiveLayoutDecision::RetainedSize {
                explored_nodes: 1..,
                ..
            } | AggressiveLayoutDecision::BudgetExhausted {
                explored_nodes: 1..,
                ..
            }
        ));

        let custom = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Size,
            &TargetConfig::Custom("unknown-layout".to_owned()),
            false,
        );
        let custom_layout = custom
            .functions
            .values()
            .next()
            .expect("custom size layout");
        assert!(matches!(
            custom_layout.size_decision,
            SizeLayoutDecision::RetainedUnknown(LayoutUnknownReason::UnsupportedTarget)
        ));
        assert!(
            !custom_layout
                .slots
                .iter()
                .any(|slot| slot.kind == PhysicalSlotKind::Union)
        );
        let aggressive_unknown = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Aggressive,
            &TargetConfig::Custom("unknown-layout".to_owned()),
            false,
        );
        assert!(matches!(
            aggressive_unknown
                .functions
                .values()
                .next()
                .expect("custom aggressive layout")
                .aggressive_decision,
            AggressiveLayoutDecision::RetainedUnknown(LayoutUnknownReason::UnsupportedTarget)
        ));

        for target in [
            TargetConfig::WindowsMsvc,
            TargetConfig::WindowsGnu,
            TargetConfig::LinuxGnu,
            TargetConfig::LinuxMusl,
            TargetConfig::Macos,
            TargetConfig::Wasm32Wasi,
        ] {
            let speed = build_context_layout(
                &liveness,
                Some(&static_plan),
                &slots,
                OptimizationLevel::Speed,
                &target,
                false,
            );
            let size = build_context_layout(
                &liveness,
                Some(&static_plan),
                &slots,
                OptimizationLevel::Size,
                &target,
                false,
            );
            let aggressive = build_context_layout(
                &liveness,
                Some(&static_plan),
                &slots,
                OptimizationLevel::Aggressive,
                &target,
                false,
            );
            let speed_size = speed
                .functions
                .values()
                .next()
                .and_then(|layout| layout.layout_knowledge.exact())
                .map(|layout| layout.size)
                .expect("exact Speed layout");
            let size_size = size
                .functions
                .values()
                .next()
                .and_then(|layout| layout.layout_knowledge.exact())
                .map(|layout| layout.size)
                .expect("exact Size layout");
            let aggressive_layout = aggressive
                .functions
                .values()
                .next()
                .expect("Aggressive layout");
            let aggressive_size = aggressive_layout
                .layout_knowledge
                .exact()
                .map(|layout| layout.size)
                .expect("exact Aggressive layout");
            assert!(size_size <= speed_size, "{target:?}");
            assert!(aggressive_size <= size_size, "{target:?}");
            assert!(matches!(
                aggressive_layout.aggressive_decision,
                AggressiveLayoutDecision::Accepted { .. }
                    | AggressiveLayoutDecision::RetainedSize { .. }
                    | AggressiveLayoutDecision::BudgetExhausted { .. }
            ));
        }
    }

    #[test]
    fn size_reuses_cross_type_results_and_embedded_child_slots() {
        let source = r#"
__async int child_int(void) { return 1; }
__async long long child_long(void) { return 2; }
__async int layout(void) {
    int first = __await child_int();
    long long second = __await child_long();
    return first + (int)second;
}
"#;
        let (liveness, static_plan, slots) = planned(source);
        let plan = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Size,
            &TargetConfig::Host,
            false,
        );
        let layout = plan
            .functions
            .values()
            .find(|layout| layout.function_name == "layout")
            .expect("cross-type layout");
        assert!(matches!(
            layout.size_decision,
            SizeLayoutDecision::Accepted { .. }
        ));
        assert!(layout.slots.iter().any(|slot| {
            let result_types: BTreeSet<_> = slot
                .members
                .iter()
                .filter(|member| matches!(member.logical, LogicalFieldId::AwaitResult(_)))
                .map(|member| normalize_c_type(&member.c_type))
                .collect();
            result_types.contains("int") && result_types.contains("long long")
        }));
        assert!(layout.slots.iter().any(|slot| {
            let child_types: BTreeSet<_> = slot
                .members
                .iter()
                .filter(|member| matches!(member.logical, LogicalFieldId::DirectChildPayload(_)))
                .map(|member| normalize_c_type(&member.c_type))
                .collect();
            child_types.len() > 1
        }));
        assert_eq!(
            verify_context_layout(&liveness, Some(&static_plan), &slots, &plan),
            Vec::new()
        );
    }

    #[test]
    fn size_excludes_parameter_alias_cleanup_task_and_qualified_fields() {
        let source = r#"
static void retain(int *value) { (void)value; }
__async int child(int value) { return value; }
__async int layout(int parameter) {
    int addressed = 1;
    int cleanup = 2;
    volatile int watched = 3;
    _Atomic int atomic = 4;
    __async int binding = child(5);
    __defer retain(&cleanup);
    int *pointer = &addressed;
    __yield 0;
    retain(pointer);
    return parameter + addressed + cleanup + watched + atomic + __await binding;
}
"#;
        let (liveness, static_plan, slots) = planned(source);
        let plan = build_context_layout(
            &liveness,
            Some(&static_plan),
            &slots,
            OptimizationLevel::Size,
            &TargetConfig::Host,
            false,
        );
        let function = liveness
            .functions
            .iter()
            .find(|function| function.coroutine.cfg.name == "layout")
            .expect("layout liveness");
        let layout = plan
            .functions
            .values()
            .find(|layout| layout.function_name == "layout")
            .expect("layout plan");
        let reason = |name: &str| {
            let declaration = function
                .lifted_fields
                .iter()
                .find(|field| field.source_name == name)
                .map(|field| field.declaration)
                .expect("lifted exclusion field");
            layout
                .decisions
                .iter()
                .find(|decision| decision.field == LogicalFieldId::Lifted(declaration))
                .map(|decision| decision.reason)
                .expect("layout decision")
        };
        assert_eq!(reason("parameter"), LayoutDecisionReason::ExcludedParameter);
        assert_eq!(
            reason("addressed"),
            LayoutDecisionReason::ExcludedAddressTaken
        );
        assert_eq!(
            reason("cleanup"),
            LayoutDecisionReason::ExcludedCleanupRetained
        );
        assert_eq!(reason("binding"), LayoutDecisionReason::ExcludedTaskBinding);
        assert_eq!(
            reason("watched"),
            LayoutDecisionReason::ExcludedVolatileOrAtomic
        );
        assert_eq!(
            reason("atomic"),
            LayoutDecisionReason::ExcludedVolatileOrAtomic
        );
    }
}
