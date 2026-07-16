//! Target, ownership, and storage metadata for coroutine await sites.

use std::collections::{BTreeMap, BTreeSet};

pub use crate::control_flow::AwaitEdgeId;
use crate::control_flow::{BlockId, CfgFunction, CfgInstruction, CfgTerminator};
use crate::semantic::{
    AwaitSlotId, DeclarationId, HirDeclaration, HirExpr, HirExprKind, SourceFragment,
};
use crate::symbol_index::{
    AsyncLinkageKey, AsyncSymbolIndex, AsyncSymbolSiteKind, FunctionId, TranslationUnitId,
};
use crate::syntax::{Diagnostic, DiagnosticSeverity, SourceSpan};

macro_rules! planning_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);
    };
}

planning_id!(ChildInstanceId);
planning_id!(ValueId);
planning_id!(ChildSlotId);
planning_id!(TypedSlotId);

/// The source construct that owns one compile-time child storage site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildOrigin {
    Direct(AwaitEdgeId),
    Binding(DeclarationId),
}

/// Whether a child has a compiler-known function target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwaitTarget {
    Static(FunctionId),
    Dynamic(ValueId),
}

/// Storage policy selected by a later planning pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwaitStorage {
    Unplanned,
    Embedded(ChildSlotId),
    Boxed(TypedSlotId),
    Opaque(AwaitSlotId),
}

/// Pre-codegen knowledge about the child's result representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResultLayout {
    KnownType(SourceFragment),
    RuntimeDefined,
}

/// The compiler site responsible for finalizing the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwaitOwnership {
    Edge(AwaitEdgeId),
    Binding(DeclarationId),
}

/// The runtime execution site that starts each new child generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildGeneration {
    AwaitEntry(AwaitEdgeId),
    DeclarationExecution(DeclarationId),
}

/// One compile-time child instance shared by one or more await edges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildInstance {
    pub id: ChildInstanceId,
    pub origin: ChildOrigin,
    pub target: AwaitTarget,
    pub storage: AwaitStorage,
    pub result: ResultLayout,
    pub ownership: AwaitOwnership,
    pub generation: ChildGeneration,
    pub requires_activation: bool,
    pub span: SourceSpan,
}

/// One suspension edge and the child instance it polls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitEdge {
    pub id: AwaitEdgeId,
    pub instance: ChildInstanceId,
    pub block: BlockId,
    pub slot: AwaitSlotId,
    pub continuation: BlockId,
    pub span: SourceSpan,
}

/// A dynamic target value retained without falling back to text identity.
#[derive(Debug, Clone)]
pub struct DynamicAwaitValue {
    pub id: ValueId,
    pub operand: HirExpr,
    pub span: SourceSpan,
}

/// Await metadata for one CFG function before storage planning.
#[derive(Debug, Clone, Default)]
pub struct FunctionAwaitPlan {
    pub edges: Vec<AwaitEdge>,
    pub children: Vec<ChildInstance>,
    pub dynamic_values: Vec<DynamicAwaitValue>,
}

/// Builds identity-bearing await metadata without selecting a storage policy.
#[must_use]
pub fn build_function_await_plan(function: &CfgFunction) -> FunctionAwaitPlan {
    AwaitPlanBuilder::new(function).build()
}

/// One planned function supplied to project-level static-call analysis.
pub struct AwaitGraphFunction<'plan> {
    pub caller: FunctionId,
    pub translation_unit: TranslationUnitId,
    pub source_start: usize,
    pub plan: &'plan FunctionAwaitPlan,
}

/// One static child-instantiation edge in the project call graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticCallEdge {
    pub caller: FunctionId,
    pub callee: FunctionId,
    pub instance: ChildInstanceId,
    pub translation_unit: TranslationUnitId,
    pub caller_source_start: usize,
    pub child_span: SourceSpan,
}

/// A deterministic graph of compiler-known child instantiations.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StaticCallGraph {
    pub nodes: Vec<FunctionId>,
    pub edges: Vec<StaticCallEdge>,
}

/// One strongly connected component in stable node order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallGraphScc {
    pub functions: Vec<FunctionId>,
    pub cyclic: bool,
}

/// Builds a project static-call graph and omits every dynamic await target.
#[must_use]
pub fn build_static_call_graph(
    functions: &[AwaitGraphFunction<'_>],
    symbols: &AsyncSymbolIndex,
) -> StaticCallGraph {
    struct OrderedEdge {
        edge: StaticCallEdge,
        callee_key: AsyncLinkageKey,
    }

    let mut nodes = BTreeSet::new();
    let mut ordered_edges = Vec::new();
    for function in functions {
        nodes.insert(function.caller);
        for child in &function.plan.children {
            let AwaitTarget::Static(callee) = child.target else {
                continue;
            };
            let callee_key = symbols
                .symbol(callee)
                .expect("static await target must exist in the async symbol index")
                .key
                .clone();
            nodes.insert(callee);
            ordered_edges.push(OrderedEdge {
                edge: StaticCallEdge {
                    caller: function.caller,
                    callee,
                    instance: child.id,
                    translation_unit: function.translation_unit.clone(),
                    caller_source_start: function.source_start,
                    child_span: child.span.clone(),
                },
                callee_key,
            });
        }
    }
    ordered_edges.sort_by(|left, right| {
        (
            &left.edge.translation_unit,
            left.edge.caller_source_start,
            left.edge.child_span.start_byte,
            &left.callee_key,
            left.edge.instance,
        )
            .cmp(&(
                &right.edge.translation_unit,
                right.edge.caller_source_start,
                right.edge.child_span.start_byte,
                &right.callee_key,
                right.edge.instance,
            ))
    });
    ordered_edges.dedup_by(|left, right| {
        left.edge.caller == right.edge.caller
            && left.edge.callee == right.edge.callee
            && left.edge.instance == right.edge.instance
            && left.edge.translation_unit == right.edge.translation_unit
            && left.edge.caller_source_start == right.edge.caller_source_start
            && left.edge.child_span == right.edge.child_span
    });

    StaticCallGraph {
        nodes: nodes.into_iter().collect(),
        edges: ordered_edges.into_iter().map(|item| item.edge).collect(),
    }
}

/// Computes deterministic Tarjan strongly connected components.
#[must_use]
pub fn strongly_connected_components(graph: &StaticCallGraph) -> Vec<CallGraphScc> {
    let mut nodes: BTreeSet<_> = graph.nodes.iter().copied().collect();
    let mut adjacency: BTreeMap<FunctionId, BTreeSet<FunctionId>> = BTreeMap::new();
    for edge in &graph.edges {
        nodes.insert(edge.caller);
        nodes.insert(edge.callee);
        adjacency
            .entry(edge.caller)
            .or_default()
            .insert(edge.callee);
    }
    for node in &nodes {
        adjacency.entry(*node).or_default();
    }

    let mut tarjan = Tarjan::new(&adjacency);
    for node in nodes {
        if !tarjan.indices.contains_key(&node) {
            tarjan.visit(node);
        }
    }
    let self_edges: BTreeSet<_> = graph
        .edges
        .iter()
        .filter(|edge| edge.caller == edge.callee)
        .map(|edge| edge.caller)
        .collect();
    let mut components: Vec<_> = tarjan
        .components
        .into_iter()
        .map(|mut functions| {
            functions.sort_unstable();
            let cyclic = functions.len() > 1 || self_edges.contains(&functions[0]);
            CallGraphScc { functions, cyclic }
        })
        .collect();
    components.sort_by_key(|component| component.functions[0]);
    components
}

/// One mutable function plan supplied to project storage selection.
pub struct AwaitStorageFunction<'plan> {
    pub caller: FunctionId,
    pub translation_unit: TranslationUnitId,
    pub source_start: usize,
    pub plan: &'plan mut FunctionAwaitPlan,
}

/// Storage-planning output and validation diagnostics.
#[derive(Debug, Clone)]
pub struct AwaitStoragePlanResult {
    pub graph: StaticCallGraph,
    pub components: Vec<CallGraphScc>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Selects deterministic storage for every child and validates the result.
pub fn plan_await_storage(
    functions: &mut [AwaitStorageFunction<'_>],
    symbols: &AsyncSymbolIndex,
) -> AwaitStoragePlanResult {
    let graph = {
        let inputs: Vec<_> = functions
            .iter()
            .map(|function| AwaitGraphFunction {
                caller: function.caller,
                translation_unit: function.translation_unit.clone(),
                source_start: function.source_start,
                plan: &*function.plan,
            })
            .collect();
        build_static_call_graph(&inputs, symbols)
    };
    let components = strongly_connected_components(&graph);
    let mut decisions = BTreeMap::new();
    let mut embedded = BTreeMap::<FunctionId, BTreeSet<FunctionId>>::new();
    let mut next_child_slot = BTreeMap::<FunctionId, u32>::new();
    let mut next_typed_slot = BTreeMap::<FunctionId, u32>::new();

    for edge in &graph.edges {
        let symbol = symbols
            .symbol(edge.callee)
            .expect("static await target must exist in the async symbol index");
        let layout_visible = symbol.sites.iter().any(|site| {
            site.kind == AsyncSymbolSiteKind::Definition
                && site.translation_unit == edge.translation_unit
        });
        let storage = if !layout_visible || would_create_cycle(&embedded, edge.caller, edge.callee)
        {
            let next = next_typed_slot.entry(edge.caller).or_default();
            let slot = TypedSlotId(*next);
            *next += 1;
            AwaitStorage::Boxed(slot)
        } else {
            embedded.entry(edge.caller).or_default().insert(edge.callee);
            let next = next_child_slot.entry(edge.caller).or_default();
            let slot = ChildSlotId(*next);
            *next += 1;
            AwaitStorage::Embedded(slot)
        };
        decisions.insert((edge.caller, edge.instance), storage);
    }

    let mut function_order: Vec<_> = (0..functions.len()).collect();
    function_order.sort_by(|left, right| {
        let left = &functions[*left];
        let right = &functions[*right];
        (&left.translation_unit, left.source_start, left.caller).cmp(&(
            &right.translation_unit,
            right.source_start,
            right.caller,
        ))
    });
    for function_index in function_order {
        let function = &mut functions[function_index];
        let mut next_opaque_slot = function
            .plan
            .edges
            .iter()
            .map(|edge| edge.slot.0)
            .max()
            .map_or(0, |slot| slot + 1);
        let edge_slots: BTreeMap<_, _> = function
            .plan
            .edges
            .iter()
            .map(|edge| (edge.instance, edge.slot))
            .collect();
        let mut child_order: Vec<_> = function
            .plan
            .children
            .iter()
            .enumerate()
            .map(|(index, child)| (child.span.start_byte, child.id, index))
            .collect();
        child_order.sort_unstable();
        for (_, child_id, index) in child_order {
            let child = &mut function.plan.children[index];
            child.storage = match child.target {
                AwaitTarget::Static(_) => decisions
                    .get(&(function.caller, child_id))
                    .copied()
                    .unwrap_or(AwaitStorage::Unplanned),
                AwaitTarget::Dynamic(_) => {
                    let slot = edge_slots.get(&child_id).copied().unwrap_or_else(|| {
                        let slot = AwaitSlotId(next_opaque_slot);
                        next_opaque_slot += 1;
                        slot
                    });
                    AwaitStorage::Opaque(slot)
                }
            };
        }
    }

    let validation_inputs: Vec<_> = functions
        .iter()
        .map(|function| AwaitGraphFunction {
            caller: function.caller,
            translation_unit: function.translation_unit.clone(),
            source_start: function.source_start,
            plan: &*function.plan,
        })
        .collect();
    let diagnostics = validate_storage_plan(&validation_inputs);
    AwaitStoragePlanResult {
        graph,
        components,
        diagnostics,
    }
}

/// Rejects incomplete, target-incompatible, or cyclic storage plans.
#[must_use]
pub fn validate_storage_plan(functions: &[AwaitGraphFunction<'_>]) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let mut embedded = BTreeMap::<FunctionId, BTreeSet<FunctionId>>::new();
    let mut children: Vec<_> = functions
        .iter()
        .flat_map(|function| {
            function
                .plan
                .children
                .iter()
                .map(move |child| (function.caller, child))
        })
        .collect();
    children.sort_by_key(|(caller, child)| (*caller, child.span.start_byte, child.id));
    for (caller, child) in children {
        if child.storage == AwaitStorage::Unplanned {
            diagnostics.push(plan_diagnostic(
                "CRP5001",
                "await child has no selected storage",
                &child.span,
            ));
            continue;
        }
        match (child.target, child.storage) {
            (AwaitTarget::Dynamic(_), AwaitStorage::Opaque(_))
            | (AwaitTarget::Static(_), AwaitStorage::Embedded(_) | AwaitStorage::Boxed(_)) => {}
            _ => diagnostics.push(plan_diagnostic(
                "CRP5002",
                "await target and storage strategy are incompatible",
                &child.span,
            )),
        }
        if let (AwaitTarget::Static(callee), AwaitStorage::Embedded(_)) =
            (child.target, child.storage)
        {
            if would_create_cycle(&embedded, caller, callee) {
                diagnostics.push(plan_diagnostic(
                    "CRP5003",
                    "embedded child storage creates a recursive layout cycle",
                    &child.span,
                ));
            } else {
                embedded.entry(caller).or_default().insert(callee);
            }
        }
    }
    diagnostics
}

fn would_create_cycle(
    adjacency: &BTreeMap<FunctionId, BTreeSet<FunctionId>>,
    caller: FunctionId,
    callee: FunctionId,
) -> bool {
    if caller == callee {
        return true;
    }
    let mut pending = vec![callee];
    let mut visited = BTreeSet::new();
    while let Some(node) = pending.pop() {
        if node == caller {
            return true;
        }
        if visited.insert(node)
            && let Some(neighbors) = adjacency.get(&node)
        {
            pending.extend(neighbors.iter().rev().copied());
        }
    }
    false
}

fn plan_diagnostic(code: &'static str, message: &str, span: &SourceSpan) -> Diagnostic {
    Diagnostic {
        code,
        severity: DiagnosticSeverity::Error,
        message: message.to_owned(),
        primary_span: span.clone(),
        related: Vec::new(),
    }
}

struct Tarjan<'graph> {
    adjacency: &'graph BTreeMap<FunctionId, BTreeSet<FunctionId>>,
    next_index: usize,
    indices: BTreeMap<FunctionId, usize>,
    lowlinks: BTreeMap<FunctionId, usize>,
    stack: Vec<FunctionId>,
    on_stack: BTreeSet<FunctionId>,
    components: Vec<Vec<FunctionId>>,
}

impl<'graph> Tarjan<'graph> {
    fn new(adjacency: &'graph BTreeMap<FunctionId, BTreeSet<FunctionId>>) -> Self {
        Self {
            adjacency,
            next_index: 0,
            indices: BTreeMap::new(),
            lowlinks: BTreeMap::new(),
            stack: Vec::new(),
            on_stack: BTreeSet::new(),
            components: Vec::new(),
        }
    }

    fn visit(&mut self, node: FunctionId) {
        let index = self.next_index;
        self.next_index += 1;
        self.indices.insert(node, index);
        self.lowlinks.insert(node, index);
        self.stack.push(node);
        self.on_stack.insert(node);

        let neighbors: Vec<_> = self.adjacency[&node].iter().copied().collect();
        for neighbor in neighbors {
            if !self.indices.contains_key(&neighbor) {
                self.visit(neighbor);
                let neighbor_lowlink = self.lowlinks[&neighbor];
                let node_lowlink = self.lowlinks[&node].min(neighbor_lowlink);
                self.lowlinks.insert(node, node_lowlink);
            } else if self.on_stack.contains(&neighbor) {
                let neighbor_index = self.indices[&neighbor];
                let node_lowlink = self.lowlinks[&node].min(neighbor_index);
                self.lowlinks.insert(node, node_lowlink);
            }
        }

        if self.lowlinks[&node] == self.indices[&node] {
            let mut component = Vec::new();
            loop {
                let member = self.stack.pop().expect("Tarjan stack contains root");
                self.on_stack.remove(&member);
                component.push(member);
                if member == node {
                    break;
                }
            }
            self.components.push(component);
        }
    }
}

struct AwaitPlanBuilder<'function> {
    function: &'function CfgFunction,
    plan: FunctionAwaitPlan,
    binding_children: BTreeMap<DeclarationId, ChildInstanceId>,
    next_child: u32,
    next_value: u32,
}

impl<'function> AwaitPlanBuilder<'function> {
    fn new(function: &'function CfgFunction) -> Self {
        Self {
            function,
            plan: FunctionAwaitPlan::default(),
            binding_children: BTreeMap::new(),
            next_child: 0,
            next_value: 0,
        }
    }

    fn build(mut self) -> FunctionAwaitPlan {
        self.collect_binding_children();
        self.collect_await_edges();
        self.plan
    }

    fn collect_binding_children(&mut self) {
        let mut declarations = BTreeMap::new();
        let mut assigned_initializers = BTreeMap::new();
        for block in &self.function.blocks {
            for instruction in &block.instructions {
                match instruction {
                    CfgInstruction::Declaration(declaration) if declaration.is_task => {
                        declarations.insert(declaration.id, declaration.clone());
                    }
                    CfgInstruction::AssignExpression {
                        destination,
                        expression,
                        ..
                    } => {
                        assigned_initializers.insert(*destination, expression.clone());
                    }
                    _ => {}
                }
            }
        }

        for (declaration_id, declaration) in declarations {
            let initializer = declaration
                .initializer
                .clone()
                .or_else(|| assigned_initializers.remove(&declaration_id))
                .unwrap_or_else(|| binding_placeholder(&declaration));
            let target = self.target_for(&initializer);
            let child = self.fresh_child();
            self.binding_children.insert(declaration_id, child);
            self.plan.children.push(ChildInstance {
                id: child,
                origin: ChildOrigin::Binding(declaration_id),
                target,
                storage: AwaitStorage::Unplanned,
                result: ResultLayout::KnownType(declaration.ty),
                ownership: AwaitOwnership::Binding(declaration_id),
                generation: ChildGeneration::DeclarationExecution(declaration_id),
                requires_activation: true,
                span: declaration.span,
            });
        }
    }

    fn collect_await_edges(&mut self) {
        let mut suspensions: Vec<_> = self
            .function
            .blocks
            .iter()
            .filter_map(|block| match &block.terminator {
                CfgTerminator::Suspend {
                    edge,
                    operand,
                    slot,
                    continuation,
                    span,
                } => Some((
                    *edge,
                    block.id,
                    operand.clone(),
                    *slot,
                    continuation.target,
                    span.clone(),
                )),
                _ => None,
            })
            .collect();
        suspensions.sort_by_key(|(edge, block, _, _, _, span)| {
            (*edge, *block, span.start_byte, span.end_byte)
        });

        for (edge, block, operand, slot, continuation, span) in suspensions {
            let instance = if let HirExprKind::TaskRef { declaration, .. } = &operand.kind {
                *self
                    .binding_children
                    .get(declaration)
                    .expect("task reference must name a declaration-owned child")
            } else {
                let target = self.target_for(&operand);
                let result = match &operand.kind {
                    HirExprKind::AsyncCall { result_type, .. } => {
                        ResultLayout::KnownType(result_type.clone())
                    }
                    _ => ResultLayout::RuntimeDefined,
                };
                let child = self.fresh_child();
                self.plan.children.push(ChildInstance {
                    id: child,
                    origin: ChildOrigin::Direct(edge),
                    target,
                    storage: AwaitStorage::Unplanned,
                    result,
                    ownership: AwaitOwnership::Edge(edge),
                    generation: ChildGeneration::AwaitEntry(edge),
                    requires_activation: true,
                    span: span.clone(),
                });
                child
            };
            self.plan.edges.push(AwaitEdge {
                id: edge,
                instance,
                block,
                slot,
                continuation,
                span,
            });
        }
    }

    fn target_for(&mut self, expression: &HirExpr) -> AwaitTarget {
        if let HirExprKind::AsyncCall {
            target: Some(target),
            ..
        } = &expression.kind
        {
            AwaitTarget::Static(*target)
        } else {
            let value = ValueId(self.next_value);
            self.next_value += 1;
            self.plan.dynamic_values.push(DynamicAwaitValue {
                id: value,
                operand: expression.clone(),
                span: expression.span.clone(),
            });
            AwaitTarget::Dynamic(value)
        }
    }

    fn fresh_child(&mut self) -> ChildInstanceId {
        let child = ChildInstanceId(self.next_child);
        self.next_child += 1;
        child
    }
}

fn binding_placeholder(declaration: &HirDeclaration) -> HirExpr {
    HirExpr {
        kind: HirExprKind::Source(declaration.name.clone()),
        span: declaration.span.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use crate::control_flow::build_cfg;
    use crate::semantic::{build_hir, build_hir_with_symbol_index};
    use crate::symbol_index::{
        AsyncSymbolInput, build_async_symbol_index, build_local_async_symbol_index,
    };
    use crate::syntax::{SourcePoint, SyntaxParser, SyntaxUnit};

    use super::*;

    fn plan_for(source: &str, function_name: &str) -> FunctionAwaitPlan {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("await-plan.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);
        assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
        let cfg = build_cfg(&hir);
        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
        let function = cfg
            .functions
            .iter()
            .find(|function| function.name == function_name)
            .expect("planned function exists");
        build_function_await_plan(function)
    }

    #[test]
    fn direct_await_owns_one_static_unplanned_child() {
        let plan = plan_for(
            r#"
__async int child(int value) { return value; }
__async int parent(int value) { return __await child(value); }
"#,
            "parent",
        );

        assert_eq!(plan.edges.len(), 1);
        assert_eq!(plan.children.len(), 1);
        let edge = &plan.edges[0];
        let child = &plan.children[0];
        assert_eq!(edge.instance, child.id);
        assert_eq!(child.origin, ChildOrigin::Direct(edge.id));
        assert!(matches!(child.target, AwaitTarget::Static(_)));
        assert_eq!(child.storage, AwaitStorage::Unplanned);
        assert!(matches!(child.result, ResultLayout::KnownType(_)));
        assert_eq!(child.ownership, AwaitOwnership::Edge(edge.id));
        assert_eq!(child.generation, ChildGeneration::AwaitEntry(edge.id));
        assert!(child.requires_activation);
    }

    #[test]
    fn repeated_task_awaits_share_one_declaration_owned_child() {
        let plan = plan_for(
            r#"
__async int child(int value) { return value; }
__async int parent(int value) {
    __async int task = child(value);
    int first = __await task;
    return first + __await task;
}
"#,
            "parent",
        );

        assert_eq!(plan.edges.len(), 2);
        assert_eq!(plan.children.len(), 1);
        assert_eq!(plan.edges[0].instance, plan.edges[1].instance);
        let child = &plan.children[0];
        let ChildOrigin::Binding(declaration) = child.origin else {
            panic!("binding-owned child expected");
        };
        assert_eq!(child.ownership, AwaitOwnership::Binding(declaration));
        assert_eq!(
            child.generation,
            ChildGeneration::DeclarationExecution(declaration)
        );
        assert!(child.requires_activation);
        assert!(matches!(child.target, AwaitTarget::Static(_)));
        assert_eq!(child.storage, AwaitStorage::Unplanned);
    }

    #[test]
    fn dynamic_await_retains_one_source_backed_value() {
        let plan = plan_for(
            "__async int parent(cr_awaitable input) { return __await input; }",
            "parent",
        );

        assert_eq!(plan.edges.len(), 1);
        assert_eq!(plan.children.len(), 1);
        assert_eq!(plan.dynamic_values.len(), 1);
        let value = &plan.dynamic_values[0];
        assert!(matches!(
            plan.children[0].target,
            AwaitTarget::Dynamic(id) if id == value.id
        ));
        assert!(matches!(
            &value.operand.kind,
            HirExprKind::Source(name) if name == "input"
        ));
        assert_eq!(value.span, value.operand.span);
        assert_eq!(plan.children[0].storage, AwaitStorage::Unplanned);
    }

    #[test]
    fn loop_await_has_one_compile_time_edge_instance() {
        let plan = plan_for(
            r#"
__async int child(int value) { return value; }
__async int parent(int count) {
    while (count > 0) {
        __await child(count);
        count = count - 1;
    }
    return count;
}
"#,
            "parent",
        );

        assert_eq!(plan.edges.len(), 1);
        assert_eq!(plan.children.len(), 1);
        assert_eq!(
            plan.children[0].origin,
            ChildOrigin::Direct(plan.edges[0].id)
        );
    }

    struct OwnedFunctionPlan {
        caller: FunctionId,
        translation_unit: TranslationUnitId,
        source_start: usize,
        plan: FunctionAwaitPlan,
    }

    fn project_plans(sources: &[(&str, &str)]) -> (AsyncSymbolIndex, Vec<OwnedFunctionPlan>) {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let units: Vec<(PathBuf, SyntaxUnit)> = sources
            .iter()
            .map(|(path, source)| {
                let path = PathBuf::from(path);
                let unit = parser
                    .parse(path.clone(), *source)
                    .expect("project source parses");
                (path, unit)
            })
            .collect();
        let inputs: Vec<_> = units
            .iter()
            .map(|(path, unit)| AsyncSymbolInput {
                project_path: path,
                unit,
            })
            .collect();
        let symbol_build = build_async_symbol_index(&inputs, "");
        assert!(
            symbol_build.diagnostics.is_empty(),
            "{:?}",
            symbol_build.diagnostics
        );
        let mut plans = Vec::new();
        for (path, unit) in &units {
            let hir = build_hir_with_symbol_index(unit, &symbol_build.index, path);
            assert!(hir.diagnostics.is_empty(), "{:?}", hir.diagnostics);
            let cfg = build_cfg(&hir);
            assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
            for function in &cfg.functions {
                let caller = symbol_build
                    .index
                    .resolve(path, &function.name)
                    .expect("project function resolves")
                    .symbol
                    .id;
                plans.push(OwnedFunctionPlan {
                    caller,
                    translation_unit: TranslationUnitId(path.to_string_lossy().replace('\\', "/")),
                    source_start: function.span.start_byte,
                    plan: build_function_await_plan(function),
                });
            }
        }
        (symbol_build.index, plans)
    }

    fn run_storage_planner(
        symbols: &AsyncSymbolIndex,
        plans: &mut [OwnedFunctionPlan],
    ) -> AwaitStoragePlanResult {
        let mut inputs: Vec<_> = plans
            .iter_mut()
            .map(|function| AwaitStorageFunction {
                caller: function.caller,
                translation_unit: function.translation_unit.clone(),
                source_start: function.source_start,
                plan: &mut function.plan,
            })
            .collect();
        plan_await_storage(&mut inputs, symbols)
    }

    fn child_storages(plans: &[OwnedFunctionPlan]) -> Vec<(FunctionId, Vec<AwaitStorage>)> {
        let mut snapshot: Vec<_> = plans
            .iter()
            .map(|function| {
                (
                    function.caller,
                    function
                        .plan
                        .children
                        .iter()
                        .map(|child| child.storage)
                        .collect(),
                )
            })
            .collect();
        snapshot.sort_by_key(|(caller, _)| *caller);
        snapshot
    }

    #[test]
    fn storage_same_unit_acyclic_child_is_embedded() {
        let (symbols, mut plans) = project_plans(&[(
            "same.cr",
            r#"
__async int leaf(int value) { return value; }
__async int parent(int value) { return __await leaf(value); }
"#,
        )]);
        let result = run_storage_planner(&symbols, &mut plans);

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let parent = plans
            .iter()
            .find(|function| !function.plan.children.is_empty())
            .expect("parent plan exists");
        assert!(matches!(
            parent.plan.children[0].storage,
            AwaitStorage::Embedded(ChildSlotId(0))
        ));
    }

    #[test]
    fn storage_cross_unit_static_child_is_boxed() {
        let (symbols, mut plans) = project_plans(&[
            ("include/leaf.hr", "__async int leaf(int value);"),
            (
                "src/leaf.cr",
                "__async int leaf(int value) { return value; }",
            ),
            (
                "src/parent.cr",
                "__async int parent(int value) { return __await leaf(value); }",
            ),
        ]);
        let result = run_storage_planner(&symbols, &mut plans);

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let parent = plans
            .iter()
            .find(|function| !function.plan.children.is_empty())
            .expect("parent plan exists");
        assert!(matches!(
            parent.plan.children[0].storage,
            AwaitStorage::Boxed(TypedSlotId(0))
        ));
    }

    #[test]
    fn storage_self_recursive_child_is_boxed() {
        let (symbols, mut plans) = project_plans(&[(
            "recursive.cr",
            "__async int recursive(int value) { return value ? __await recursive(value - 1) : 0; }",
        )]);
        let result = run_storage_planner(&symbols, &mut plans);

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert_eq!(result.components.len(), 1);
        assert!(result.components[0].cyclic);
        assert!(matches!(
            plans[0].plan.children[0].storage,
            AwaitStorage::Boxed(TypedSlotId(0))
        ));
    }

    #[test]
    fn storage_mutual_recursion_boxes_only_cycle_closing_edge() {
        let (symbols, mut plans) = project_plans(&[(
            "mutual.cr",
            r#"
__async int second(int value);
__async int first(int value) { return __await second(value); }
__async int second(int value) { return __await first(value); }
"#,
        )]);
        let result = run_storage_planner(&symbols, &mut plans);

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        let storages: Vec<_> = plans
            .iter()
            .flat_map(|function| function.plan.children.iter().map(|child| child.storage))
            .collect();
        assert_eq!(
            storages
                .iter()
                .filter(|storage| matches!(storage, AwaitStorage::Embedded(_)))
                .count(),
            1
        );
        assert_eq!(
            storages
                .iter()
                .filter(|storage| matches!(storage, AwaitStorage::Boxed(_)))
                .count(),
            1
        );
    }

    #[test]
    fn storage_dynamic_child_is_opaque() {
        let (symbols, mut plans) = project_plans(&[(
            "dynamic.cr",
            "__async int parent(cr_awaitable input) { return __await input; }",
        )]);
        let result = run_storage_planner(&symbols, &mut plans);

        assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
        assert!(matches!(
            plans[0].plan.children[0].storage,
            AwaitStorage::Opaque(AwaitSlotId(0))
        ));
    }

    #[test]
    fn storage_plan_is_independent_of_source_and_function_input_order() {
        let forward_sources = [
            (
                "src/leaf.cr",
                "__async int leaf(int value) { return value; }",
            ),
            (
                "src/parent.cr",
                "__async int parent(int value) { return __await leaf(value); }",
            ),
        ];
        let reverse_sources = [forward_sources[1], forward_sources[0]];
        let (forward_symbols, mut forward_plans) = project_plans(&forward_sources);
        let (reverse_symbols, mut reverse_plans) = project_plans(&reverse_sources);
        reverse_plans.reverse();

        let forward = run_storage_planner(&forward_symbols, &mut forward_plans);
        let reverse = run_storage_planner(&reverse_symbols, &mut reverse_plans);

        assert!(forward.diagnostics.is_empty(), "{:?}", forward.diagnostics);
        assert!(reverse.diagnostics.is_empty(), "{:?}", reverse.diagnostics);
        assert_eq!(forward.graph, reverse.graph);
        assert_eq!(
            child_storages(&forward_plans),
            child_storages(&reverse_plans)
        );
    }

    #[test]
    fn storage_validation_rejects_unplanned_child() {
        let (symbols, plans) = project_plans(&[(
            "unplanned.cr",
            r#"
__async int leaf(int value) { return value; }
__async int parent(int value) { return __await leaf(value); }
"#,
        )]);
        let inputs: Vec<_> = plans
            .iter()
            .map(|function| AwaitGraphFunction {
                caller: function.caller,
                translation_unit: function.translation_unit.clone(),
                source_start: function.source_start,
                plan: &function.plan,
            })
            .collect();

        assert!(symbols.symbols().len() >= 2);
        assert!(
            validate_storage_plan(&inputs)
                .iter()
                .any(|diagnostic| diagnostic.code == "CRP5001")
        );
    }

    fn pure_graph(nodes: &[u32], edges: &[(u32, u32)]) -> StaticCallGraph {
        StaticCallGraph {
            nodes: nodes.iter().copied().map(FunctionId).collect(),
            edges: edges
                .iter()
                .enumerate()
                .map(|(index, (caller, callee))| StaticCallEdge {
                    caller: FunctionId(*caller),
                    callee: FunctionId(*callee),
                    instance: ChildInstanceId(index as u32),
                    translation_unit: TranslationUnitId("pure.cr".to_owned()),
                    caller_source_start: index,
                    child_span: SourceSpan {
                        path: PathBuf::from("pure.cr"),
                        start_byte: index,
                        end_byte: index + 1,
                        start: SourcePoint {
                            row: index,
                            column: 0,
                        },
                        end: SourcePoint {
                            row: index,
                            column: 1,
                        },
                    },
                })
                .collect(),
        }
    }

    #[test]
    fn scc_acyclic_chain_has_three_singletons() {
        let components = strongly_connected_components(&pure_graph(&[0, 1, 2], &[(0, 1), (1, 2)]));

        assert_eq!(components.len(), 3);
        assert_eq!(components[0].functions, vec![FunctionId(0)]);
        assert_eq!(components[1].functions, vec![FunctionId(1)]);
        assert_eq!(components[2].functions, vec![FunctionId(2)]);
        assert!(components.iter().all(|component| !component.cyclic));
    }

    #[test]
    fn scc_self_loop_is_cyclic() {
        let components = strongly_connected_components(&pure_graph(&[0], &[(0, 0)]));

        assert_eq!(components.len(), 1);
        assert_eq!(components[0].functions, vec![FunctionId(0)]);
        assert!(components[0].cyclic);
    }

    #[test]
    fn scc_three_function_cycle_is_one_component() {
        let components =
            strongly_connected_components(&pure_graph(&[0, 1, 2], &[(0, 1), (1, 2), (2, 0)]));

        assert_eq!(components.len(), 1);
        assert_eq!(
            components[0].functions,
            vec![FunctionId(0), FunctionId(1), FunctionId(2)]
        );
        assert!(components[0].cyclic);
    }

    #[test]
    fn scc_connected_cycles_and_disconnected_nodes_stay_distinct() {
        let components = strongly_connected_components(&pure_graph(
            &[0, 1, 2, 3, 4, 5],
            &[(0, 1), (1, 0), (1, 2), (2, 3), (3, 2)],
        ));

        assert_eq!(components.len(), 4);
        assert_eq!(components[0].functions, vec![FunctionId(0), FunctionId(1)]);
        assert!(components[0].cyclic);
        assert_eq!(components[1].functions, vec![FunctionId(2), FunctionId(3)]);
        assert!(components[1].cyclic);
        assert_eq!(components[2].functions, vec![FunctionId(4)]);
        assert_eq!(components[3].functions, vec![FunctionId(5)]);
    }

    #[test]
    fn scc_result_is_independent_of_edge_insertion_order() {
        let forward = pure_graph(&[0, 1, 2, 3], &[(0, 1), (1, 2), (2, 0), (2, 3)]);
        let reverse = pure_graph(&[3, 2, 1, 0], &[(2, 3), (2, 0), (1, 2), (0, 1)]);

        assert_eq!(
            strongly_connected_components(&forward),
            strongly_connected_components(&reverse)
        );
    }

    #[test]
    fn source_call_graph_includes_static_bindings_and_omits_dynamic_targets() {
        let source = r#"
__async int leaf(int value) { return value; }
__async int binding_parent(void) {
    __async int task = leaf(1);
    return 0;
}
__async int direct_parent(void) { return __await leaf(2); }
__async int dynamic_parent(cr_awaitable input) { return __await input; }
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("await-plan.cr"), source)
            .expect("source parses");
        let symbols = build_local_async_symbol_index(&syntax, "");
        assert!(symbols.diagnostics.is_empty(), "{:?}", symbols.diagnostics);
        let hir = build_hir_with_symbol_index(&syntax, &symbols.index, Path::new("await-plan.cr"));
        let cfg = build_cfg(&hir);
        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
        let plans: Vec<_> = cfg
            .functions
            .iter()
            .map(|function| {
                let caller = symbols
                    .index
                    .resolve(Path::new("await-plan.cr"), &function.name)
                    .expect("caller resolves")
                    .symbol
                    .id;
                (
                    caller,
                    function.span.start_byte,
                    build_function_await_plan(function),
                )
            })
            .collect();
        let inputs: Vec<_> = plans
            .iter()
            .map(|(caller, source_start, plan)| AwaitGraphFunction {
                caller: *caller,
                translation_unit: TranslationUnitId("await-plan.cr".to_owned()),
                source_start: *source_start,
                plan,
            })
            .collect();
        let reversed_inputs: Vec<_> = plans
            .iter()
            .rev()
            .map(|(caller, source_start, plan)| AwaitGraphFunction {
                caller: *caller,
                translation_unit: TranslationUnitId("await-plan.cr".to_owned()),
                source_start: *source_start,
                plan,
            })
            .collect();
        let graph = build_static_call_graph(&inputs, &symbols.index);
        let reversed = build_static_call_graph(&reversed_inputs, &symbols.index);

        assert_eq!(graph, reversed);
        assert_eq!(graph.nodes.len(), 4);
        assert_eq!(graph.edges.len(), 2);
        let leaf = symbols
            .index
            .resolve(Path::new("await-plan.cr"), "leaf")
            .expect("leaf resolves")
            .symbol
            .id;
        assert!(graph.edges.iter().all(|edge| edge.callee == leaf));
    }
}
