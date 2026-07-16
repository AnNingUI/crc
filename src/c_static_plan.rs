//! Target-specific typed and dynamic child plans for C emission.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::await_plan::{
    AwaitStorage, AwaitTarget, ChildInstanceId, ChildOrigin, ChildSlotId, ResultLayout,
    TypedSlotId, ValueId,
};
use crate::c_declaration_env::{CDeclarationEnvironment, DeclarationMoveBlock};
use crate::control_flow::AwaitEdgeId;
use crate::liveness::LivenessUnit;
use crate::semantic::AwaitSlotId;
use crate::symbol_index::{
    AsyncLinkageKey, AsyncParameter, AsyncSymbolIndex, FunctionId, LayoutVisibility,
    TranslationUnitId,
};
use crate::syntax::{Diagnostic, DiagnosticSeverity, SourceSpan};

/// One compiler-known asynchronous callee and its typed C surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StaticCallee {
    pub function: FunctionId,
    pub linkage: AsyncLinkageKey,
    pub stem: String,
    pub task_type: String,
    pub result_type: String,
    pub parameters: Vec<AsyncParameter>,
    pub is_variadic: bool,
    pub definition_unit: Option<TranslationUnitId>,
    pub layout_visibility: LayoutVisibility,
}

/// The target selected before C expression rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CChildTarget {
    Static(StaticCallee),
    Dynamic(ValueId),
}

/// The final physical representation used by the C backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CChildStorage {
    Embedded(ChildSlotId),
    Boxed(TypedSlotId),
    Opaque(AwaitSlotId),
}

/// Target-specific evidence for an embedded-to-boxed decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CLayoutReason {
    CompleteTypeNotVisible,
    PrototypeNotVisible,
    PreprocessorRegion,
    TargetPolicy,
}

/// One child instance after target-specific C planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CChildPlan {
    pub instance: ChildInstanceId,
    pub target: CChildTarget,
    pub requested_storage: AwaitStorage,
    pub effective_storage: CChildStorage,
    pub downgrade_reason: Option<CLayoutReason>,
    pub origin: ChildOrigin,
    pub result: ResultLayout,
    pub span: SourceSpan,
}

/// One function's child records and suspension-edge ownership map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CFunctionPlan {
    pub caller: FunctionId,
    pub source_start: usize,
    pub children: BTreeMap<ChildInstanceId, CChildPlan>,
    pub edge_children: BTreeMap<AwaitEdgeId, ChildInstanceId>,
    pub required_prototypes: Vec<FunctionId>,
}

/// Complete task layouts emitted together at one source anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutIsland {
    pub anchor: usize,
    pub layouts: Vec<FunctionId>,
}

/// A validated translation-unit C plan.
#[derive(Debug, Clone, Default)]
pub struct CStaticAwaitPlan {
    pub functions: BTreeMap<FunctionId, CFunctionPlan>,
    pub layout_islands: Vec<LayoutIsland>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Resolves Stage 1 identities and storage into target-specific C records.
#[must_use]
pub fn build_c_static_await_plan(
    unit: &LivenessUnit,
    project_path: &Path,
    symbols: &AsyncSymbolIndex,
    declaration_environment: &CDeclarationEnvironment,
) -> CStaticAwaitPlan {
    let mut plan = CStaticAwaitPlan::default();
    let function_indices: BTreeMap<_, _> = unit
        .functions
        .iter()
        .enumerate()
        .filter(|(_, function)| function.coroutine.cfg.is_async)
        .filter_map(|(index, function)| {
            symbols
                .resolve(project_path, &function.coroutine.cfg.name)
                .map(|resolved| (resolved.symbol.id, index))
        })
        .collect();
    for function in unit
        .functions
        .iter()
        .filter(|function| function.coroutine.cfg.is_async)
    {
        let Some(caller) = symbols
            .resolve(project_path, &function.coroutine.cfg.name)
            .map(|resolved| resolved.symbol.id)
        else {
            plan.diagnostics.push(c_plan_diagnostic(
                "CRC6001",
                "async function is missing from the C symbol plan",
                &function.coroutine.cfg.span,
            ));
            continue;
        };
        let mut children = BTreeMap::new();
        for child in &function.coroutine.await_plan.children {
            let target = match child.target {
                AwaitTarget::Static(callee) => {
                    let Some(resolved) = symbols.resolve_id(project_path, callee) else {
                        plan.diagnostics.push(c_plan_diagnostic(
                            "CRC6002",
                            "static await target is missing from the C symbol plan",
                            &child.span,
                        ));
                        continue;
                    };
                    let symbol = resolved.symbol;
                    CChildTarget::Static(StaticCallee {
                        function: callee,
                        linkage: symbol.key.clone(),
                        stem: symbol.public_stem.clone(),
                        task_type: format!("{}_task", symbol.public_stem),
                        result_type: symbol.result_type.clone(),
                        parameters: symbol.parameters.clone(),
                        is_variadic: symbol.is_variadic,
                        definition_unit: symbol
                            .sites
                            .iter()
                            .find(|site| {
                                site.kind == crate::symbol_index::AsyncSymbolSiteKind::Definition
                            })
                            .map(|site| site.translation_unit.clone()),
                        layout_visibility: resolved.layout_visibility,
                    })
                }
                AwaitTarget::Dynamic(value) => CChildTarget::Dynamic(value),
            };
            let Some(effective_storage) = effective_storage(child.storage) else {
                plan.diagnostics.push(c_plan_diagnostic(
                    "CRC6003",
                    "await child has no C storage strategy",
                    &child.span,
                ));
                continue;
            };
            if let Some(diagnostic) =
                validate_combination(&target, effective_storage, child.origin, &child.span)
            {
                plan.diagnostics.push(diagnostic);
                continue;
            }
            children.insert(
                child.id,
                CChildPlan {
                    instance: child.id,
                    target,
                    requested_storage: child.storage,
                    effective_storage,
                    downgrade_reason: None,
                    origin: child.origin,
                    result: child.result.clone(),
                    span: child.span.clone(),
                },
            );
        }
        let edge_children = function
            .coroutine
            .await_plan
            .edges
            .iter()
            .map(|edge| (edge.id, edge.instance))
            .collect();
        plan.functions.insert(
            caller,
            CFunctionPlan {
                caller,
                source_start: function.coroutine.cfg.span.start_byte,
                children,
                edge_children,
                required_prototypes: Vec::new(),
            },
        );
    }
    finalize_c_layout_plan(
        &mut plan,
        unit,
        project_path,
        symbols,
        declaration_environment,
        &function_indices,
    );
    plan
}

fn finalize_c_layout_plan(
    plan: &mut CStaticAwaitPlan,
    unit: &LivenessUnit,
    project_path: &Path,
    symbols: &AsyncSymbolIndex,
    declaration_environment: &CDeclarationEnvironment,
    function_indices: &BTreeMap<FunctionId, usize>,
) {
    for function_index in function_indices.values() {
        let function = &unit.functions[*function_index];
        let types = task_layout_types(function);
        if declaration_environment.contains_local_context_type(
            types.iter().copied(),
            function.coroutine.cfg.span.start_byte,
            function.coroutine.cfg.span.end_byte,
        ) {
            plan.diagnostics.push(c_plan_diagnostic(
                "CRC6008",
                "function-local type can't be stored in a coroutine task context",
                &function.coroutine.cfg.span,
            ));
        }
    }
    plan_required_prototypes(plan, project_path, symbols, declaration_environment);
    let mut anchors: BTreeMap<_, _> = plan
        .functions
        .iter()
        .map(|(id, function)| (*id, function.source_start))
        .collect();
    let mut next_typed_slots: BTreeMap<_, _> = plan
        .functions
        .iter()
        .map(|(id, function)| {
            let next = function
                .children
                .values()
                .filter_map(|child| match child.effective_storage {
                    CChildStorage::Boxed(slot) => Some(slot.0),
                    _ => None,
                })
                .max()
                .map_or(0, |slot| slot + 1);
            (*id, next)
        })
        .collect();

    loop {
        let mut changed = false;
        let edges = embedded_edges(plan);
        for (caller, instance, callee) in edges {
            let caller_anchor = anchors[&caller];
            if anchors[&callee] <= caller_anchor {
                continue;
            }
            let Some(callee_index) = function_indices.get(&callee) else {
                continue;
            };
            let callee_function = &unit.functions[*callee_index];
            let types = task_layout_types(callee_function);
            if let Some(block) = declaration_environment.classify_move(
                types.iter().copied(),
                callee_function.coroutine.cfg.span.start_byte,
                caller_anchor,
            ) {
                let next = next_typed_slots
                    .get_mut(&caller)
                    .expect("planned caller has typed slot state");
                let child = plan
                    .functions
                    .get_mut(&caller)
                    .and_then(|function| function.children.get_mut(&instance))
                    .expect("embedded edge has a child plan");
                child.effective_storage = CChildStorage::Boxed(TypedSlotId(*next));
                *next += 1;
                child.downgrade_reason = Some(match block {
                    DeclarationMoveBlock::TypeNotVisible => CLayoutReason::CompleteTypeNotVisible,
                    DeclarationMoveBlock::PreprocessorBoundary => CLayoutReason::PreprocessorRegion,
                });
                changed = true;
            } else {
                anchors.insert(callee, caller_anchor);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut islands = BTreeMap::<usize, Vec<FunctionId>>::new();
    for (function, anchor) in anchors {
        islands.entry(anchor).or_default().push(function);
    }
    plan.layout_islands = islands
        .into_iter()
        .map(|(anchor, functions)| LayoutIsland {
            anchor,
            layouts: topological_layout_order(&functions, plan),
        })
        .collect();
}

fn plan_required_prototypes(
    plan: &mut CStaticAwaitPlan,
    project_path: &Path,
    symbols: &AsyncSymbolIndex,
    declaration_environment: &CDeclarationEnvironment,
) {
    let callers: Vec<_> = plan.functions.keys().copied().collect();
    for caller in callers {
        let (source_start, static_children) = {
            let function = &plan.functions[&caller];
            let children = function
                .children
                .values()
                .filter_map(|child| match &child.target {
                    CChildTarget::Static(callee) => Some((callee.clone(), child.span.clone())),
                    CChildTarget::Dynamic(_) => None,
                })
                .collect::<Vec<_>>();
            (function.source_start, children)
        };
        let mut required = BTreeSet::new();
        for (callee, span) in static_children {
            if symbols.has_site_before(project_path, callee.function, source_start) {
                continue;
            }
            let mut signature_types = Vec::with_capacity(callee.parameters.len() + 1);
            signature_types.push(callee.result_type.as_str());
            signature_types.extend(
                callee
                    .parameters
                    .iter()
                    .map(|parameter| parameter.adjusted_type.as_str()),
            );
            if declaration_environment
                .classify_visibility_at(signature_types, source_start)
                .is_some()
            {
                plan.diagnostics.push(c_plan_diagnostic(
                    "CRC6007",
                    "static await requires a visible compatible typed declaration",
                    &span,
                ));
            } else {
                required.insert(callee.function);
            }
        }
        plan.functions
            .get_mut(&caller)
            .expect("planned caller exists")
            .required_prototypes = required.into_iter().collect();
    }
}

fn embedded_edges(plan: &CStaticAwaitPlan) -> Vec<(FunctionId, ChildInstanceId, FunctionId)> {
    let mut edges = Vec::new();
    for (caller, function) in &plan.functions {
        for child in function.children.values() {
            if matches!(child.effective_storage, CChildStorage::Embedded(_))
                && let CChildTarget::Static(callee) = &child.target
            {
                edges.push((*caller, child.instance, callee.function));
            }
        }
    }
    edges.sort_by_key(|(caller, instance, callee)| {
        (
            plan.functions[caller].source_start,
            *caller,
            *instance,
            *callee,
        )
    });
    edges
}

fn topological_layout_order(functions: &[FunctionId], plan: &CStaticAwaitPlan) -> Vec<FunctionId> {
    fn visit(
        function: FunctionId,
        members: &BTreeSet<FunctionId>,
        plan: &CStaticAwaitPlan,
        visiting: &mut BTreeSet<FunctionId>,
        visited: &mut BTreeSet<FunctionId>,
        output: &mut Vec<FunctionId>,
    ) {
        if visited.contains(&function) || !visiting.insert(function) {
            return;
        }
        let mut dependencies: Vec<_> = plan.functions[&function]
            .children
            .values()
            .filter_map(|child| {
                matches!(child.effective_storage, CChildStorage::Embedded(_))
                    .then(|| match &child.target {
                        CChildTarget::Static(callee) if members.contains(&callee.function) => {
                            Some(callee.function)
                        }
                        _ => None,
                    })
                    .flatten()
            })
            .collect();
        dependencies.sort_unstable();
        dependencies.dedup();
        for dependency in dependencies {
            visit(dependency, members, plan, visiting, visited, output);
        }
        visiting.remove(&function);
        visited.insert(function);
        output.push(function);
    }

    let members: BTreeSet<_> = functions.iter().copied().collect();
    let mut roots: Vec<_> = functions.to_vec();
    roots.sort_unstable();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut output = Vec::new();
    for function in roots {
        visit(
            function,
            &members,
            plan,
            &mut visiting,
            &mut visited,
            &mut output,
        );
    }
    output
}

fn task_layout_types(function: &crate::liveness::LivenessFunction) -> Vec<&str> {
    let mut types: Vec<_> = function
        .lifted_fields
        .iter()
        .map(|field| field.ty.text.as_str())
        .collect();
    let return_type = function.coroutine.cfg.return_type.text.trim();
    if return_type != "void" {
        types.push(return_type);
    }
    for child in &function.coroutine.await_plan.children {
        if let ResultLayout::KnownType(result) = &child.result {
            types.push(result.text.as_str());
        }
    }
    types
}

fn effective_storage(storage: AwaitStorage) -> Option<CChildStorage> {
    match storage {
        AwaitStorage::Embedded(slot) => Some(CChildStorage::Embedded(slot)),
        AwaitStorage::Boxed(slot) => Some(CChildStorage::Boxed(slot)),
        AwaitStorage::Opaque(slot) => Some(CChildStorage::Opaque(slot)),
        AwaitStorage::Unplanned => None,
    }
}

fn validate_combination(
    target: &CChildTarget,
    storage: CChildStorage,
    origin: ChildOrigin,
    span: &SourceSpan,
) -> Option<Diagnostic> {
    match (target, storage, origin) {
        (CChildTarget::Static(_), CChildStorage::Embedded(_) | CChildStorage::Boxed(_), _)
        | (CChildTarget::Dynamic(_), CChildStorage::Opaque(_), ChildOrigin::Direct(_)) => None,
        (CChildTarget::Static(_), CChildStorage::Opaque(_), _) => Some(c_plan_diagnostic(
            "CRC6004",
            "static await target can't use opaque C storage",
            span,
        )),
        (CChildTarget::Dynamic(_), CChildStorage::Embedded(_) | CChildStorage::Boxed(_), _) => {
            Some(c_plan_diagnostic(
                "CRC6005",
                "dynamic await target can't use typed C storage",
                span,
            ))
        }
        (CChildTarget::Dynamic(_), CChildStorage::Opaque(_), ChildOrigin::Binding(_)) => Some(
            c_plan_diagnostic("CRC6006", "dynamic task bindings aren't supported", span),
        ),
    }
}

fn c_plan_diagnostic(code: &'static str, message: &str, span: &SourceSpan) -> Diagnostic {
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
    use std::path::{Path, PathBuf};

    use crate::await_plan::{AwaitStorageFunction, plan_await_storage};
    use crate::c_declaration_env::build_c_declaration_environment;
    use crate::control_flow::build_cfg;
    use crate::coroutine::lower_coroutines;
    use crate::liveness::analyze_liveness;
    use crate::scope_exit::lower_scope_exits;
    use crate::semantic::{DeclarationId, build_hir_with_symbol_index};
    use crate::symbol_index::{AsyncSymbolInput, build_async_symbol_index};
    use crate::syntax::SyntaxParser;

    use super::*;

    #[test]
    fn resolves_static_and_dynamic_children_without_changing_storage() {
        let source = r#"
cr_awaitable external_value(int value);

__async int child(int value) { return value; }

__async int parent(int value) {
    __async int bound = child(value);
    int first = __await child(value);
    int second = __await bound;
    return __await external_value(first + second);
}

__async int earlier_parent(int value) {
    return __await late_child(value);
}

typedef int LateValue;
__async int late_child(int value) {
    LateValue held = value;
    __yield held;
    return held;
}

__async int invalid_signature_parent(int value) {
    return __await signature_child(value);
}

typedef int SignatureValue;
__async int signature_child(SignatureValue value) { return value; }

__async int ordered_parent(int value) {
    return __await ordered_child(value);
}

__async int ordered_child(int value) { return value; }

__async int local_context(int value) {
    typedef int LocalContextValue;
    LocalContextValue held = value;
    __yield held;
    return held;
}
"#;
        let project_path = Path::new("src/plan.cr");
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from(project_path), source)
            .expect("source parses");
        let symbol_build = build_async_symbol_index(
            &[AsyncSymbolInput {
                project_path,
                unit: &syntax,
            }],
            "cr_",
        );
        assert!(
            symbol_build.diagnostics.is_empty(),
            "{:?}",
            symbol_build.diagnostics
        );
        let hir = build_hir_with_symbol_index(&syntax, &symbol_build.index, project_path);
        let cfg = lower_scope_exits(&build_cfg(&hir));
        let mut coroutines = lower_coroutines(&cfg, "cr_");
        let translation_unit = TranslationUnitId("src/plan.cr".to_owned());
        let mut planning_functions = Vec::new();
        for function in coroutines
            .functions
            .iter_mut()
            .filter(|function| function.cfg.is_async)
        {
            let caller = symbol_build
                .index
                .resolve(project_path, &function.cfg.name)
                .expect("caller resolves")
                .symbol
                .id;
            planning_functions.push(AwaitStorageFunction {
                caller,
                translation_unit: translation_unit.clone(),
                source_start: function.cfg.span.start_byte,
                plan: &mut function.await_plan,
            });
        }
        let storage = plan_await_storage(&mut planning_functions, &symbol_build.index);
        assert!(storage.diagnostics.is_empty(), "{:?}", storage.diagnostics);
        drop(planning_functions);
        let liveness = analyze_liveness(&coroutines);
        let declaration_environment = build_c_declaration_environment(&syntax);
        let plan = build_c_static_await_plan(
            &liveness,
            project_path,
            &symbol_build.index,
            &declaration_environment,
        );

        let diagnostic_codes: BTreeSet<_> = plan.diagnostics.iter().map(|item| item.code).collect();
        assert_eq!(
            diagnostic_codes,
            BTreeSet::from(["CRC6007", "CRC6008"]),
            "{:?}",
            plan.diagnostics
        );
        let parent_id = symbol_build
            .index
            .resolve(project_path, "parent")
            .expect("parent resolves")
            .symbol
            .id;
        let parent = &plan.functions[&parent_id];
        assert_eq!(parent.children.len(), 3);
        assert_eq!(parent.edge_children.len(), 3);
        let static_children: Vec<_> = parent
            .children
            .values()
            .filter(|child| matches!(child.target, CChildTarget::Static(_)))
            .collect();
        assert_eq!(static_children.len(), 2);
        assert!(
            static_children
                .iter()
                .all(|child| matches!(child.effective_storage, CChildStorage::Embedded(_)))
        );
        let dynamic = parent
            .children
            .values()
            .find(|child| matches!(child.target, CChildTarget::Dynamic(_)))
            .expect("dynamic child exists");
        assert!(matches!(
            dynamic.effective_storage,
            CChildStorage::Opaque(_)
        ));

        let static_opaque = validate_combination(
            &static_children[0].target,
            CChildStorage::Opaque(AwaitSlotId(99)),
            static_children[0].origin,
            &static_children[0].span,
        )
        .expect("static opaque storage is rejected");
        assert_eq!(static_opaque.code, "CRC6004");
        let dynamic_typed = validate_combination(
            &dynamic.target,
            CChildStorage::Embedded(ChildSlotId(99)),
            dynamic.origin,
            &dynamic.span,
        )
        .expect("dynamic typed storage is rejected");
        assert_eq!(dynamic_typed.code, "CRC6005");
        let dynamic_binding = validate_combination(
            &dynamic.target,
            dynamic.effective_storage,
            ChildOrigin::Binding(DeclarationId(99)),
            &dynamic.span,
        )
        .expect("dynamic binding is rejected");
        assert_eq!(dynamic_binding.code, "CRC6006");

        let earlier_parent_id = symbol_build
            .index
            .resolve(project_path, "earlier_parent")
            .expect("earlier parent resolves")
            .symbol
            .id;
        let earlier_child = plan.functions[&earlier_parent_id]
            .children
            .values()
            .next()
            .expect("earlier parent child");
        assert!(matches!(
            earlier_child.requested_storage,
            AwaitStorage::Embedded(_)
        ));
        assert!(matches!(
            earlier_child.effective_storage,
            CChildStorage::Boxed(_)
        ));
        assert_eq!(
            earlier_child.downgrade_reason,
            Some(CLayoutReason::CompleteTypeNotVisible)
        );
        assert!(plan.layout_islands.iter().any(|island| {
            island.anchor == plan.functions[&earlier_parent_id].source_start
                && island.layouts.contains(&earlier_parent_id)
        }));

        let ordered_parent_id = symbol_build
            .index
            .resolve(project_path, "ordered_parent")
            .expect("ordered parent resolves")
            .symbol
            .id;
        let ordered_child_id = symbol_build
            .index
            .resolve(project_path, "ordered_child")
            .expect("ordered child resolves")
            .symbol
            .id;
        let ordered_island = plan
            .layout_islands
            .iter()
            .find(|island| {
                island.layouts.contains(&ordered_parent_id)
                    && island.layouts.contains(&ordered_child_id)
            })
            .expect("ordered layouts share an island");
        let child_position = ordered_island
            .layouts
            .iter()
            .position(|function| *function == ordered_child_id)
            .expect("ordered child is present");
        let parent_position = ordered_island
            .layouts
            .iter()
            .position(|function| *function == ordered_parent_id)
            .expect("ordered parent is present");
        assert!(child_position < parent_position);
        assert_eq!(
            plan.functions[&ordered_parent_id].required_prototypes,
            vec![ordered_child_id]
        );
    }
}
