//! Source-preserving C11 emitter for the new CR lowering pipeline.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write;

use crate::await_plan::ChildOrigin;
use crate::c_static_plan::{
    CChildPlan, CChildStorage, CChildTarget, CFunctionPlan, CStaticAwaitPlan, LayoutIsland,
    StaticCallee,
};
use crate::config::{OptimizationLevel, TargetConfig};
use crate::context_layout::{
    ContextLayoutPlan, FunctionContextLayout, LogicalFieldId, PhysicalSlotKind,
    verify_context_layout,
};
use crate::control_flow::{
    BasicBlock, BlockId, CfgInstruction, CfgTerminator, CfgValue, CleanupId, CleanupRegistration,
};
use crate::coroutine::CoroutineFunction;
use crate::liveness::{LiftedField, LivenessFunction, LivenessUnit};
use crate::semantic::{AwaitSlotId, DeclarationId, HirDeclaration, HirExpr, HirExprKind, ScopeId};
use crate::slot_liveness::SlotLivenessUnit;
use crate::symbol_index::FunctionId;
use crate::syntax::{Diagnostic, DiagnosticSeverity, SourceSpan};

/// C backend selection. Both variants consume the same lowered CFG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CBackend {
    Portable,
    ComputedGoto,
}

/// Resolved configuration for generated identifiers and dispatch.
#[derive(Debug, Clone)]
pub struct CEmitterConfig {
    pub prefix: String,
    pub context_name: String,
    pub backend: CBackend,
    pub target: TargetConfig,
    pub optimization: OptimizationLevel,
}

impl Default for CEmitterConfig {
    fn default() -> Self {
        Self {
            prefix: "cr_".to_owned(),
            context_name: "ctx".to_owned(),
            backend: CBackend::Portable,
            target: TargetConfig::Host,
            optimization: OptimizationLevel::None,
        }
    }
}

/// Generated source and diagnostics from one translation unit.
#[derive(Debug, Clone)]
pub struct CEmission {
    pub source: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// Replaces transformed functions while preserving unaffected source bytes.
#[must_use]
pub fn emit_translation_unit(
    original: &str,
    unit: &LivenessUnit,
    static_plan: Option<&CStaticAwaitPlan>,
    slot_liveness: &SlotLivenessUnit,
    layout_plan: &ContextLayoutPlan,
    config: &CEmitterConfig,
) -> CEmission {
    let mut layout_diagnostics = layout_plan.diagnostics.clone();
    layout_diagnostics.extend(verify_context_layout(
        unit,
        static_plan,
        slot_liveness,
        layout_plan,
    ));
    if layout_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == DiagnosticSeverity::Error)
    {
        let mut diagnostics = unit.diagnostics.clone();
        diagnostics.extend(layout_diagnostics);
        return CEmission {
            source: String::new(),
            diagnostics,
        };
    }
    let static_stems = static_plan
        .into_iter()
        .flat_map(|plan| plan.functions.values())
        .flat_map(|function| function.children.values())
        .filter_map(|child| match &child.target {
            CChildTarget::Static(callee) => Some((callee.function, callee.stem.clone())),
            CChildTarget::Dynamic(_) => None,
        })
        .collect();
    let internal_stems = static_plan
        .into_iter()
        .flat_map(|plan| plan.functions.values())
        .flat_map(|function| function.children.values())
        .filter_map(|child| match &child.target {
            CChildTarget::Static(callee)
                if matches!(
                    callee.linkage,
                    crate::symbol_index::AsyncLinkageKey::Internal { .. }
                ) =>
            {
                Some(callee.stem.clone())
            }
            _ => None,
        })
        .collect();
    let function_plans = static_plan
        .into_iter()
        .flat_map(|plan| plan.functions.values())
        .map(|function| (function.source_start, function.clone()))
        .collect();
    let heap_api_stems = static_plan
        .into_iter()
        .flat_map(|plan| plan.functions.values())
        .flat_map(|function| function.children.values())
        .filter_map(|child| match (&child.target, child.effective_storage) {
            (CChildTarget::Static(callee), CChildStorage::Boxed(_))
                if callee.layout_visibility == crate::symbol_index::LayoutVisibility::Visible =>
            {
                Some(callee.stem.clone())
            }
            _ => None,
        })
        .collect();
    let accessor_stems = static_plan
        .into_iter()
        .flat_map(|plan| plan.functions.values())
        .flat_map(|function| function.children.values())
        .filter_map(|child| match (&child.target, child.effective_storage) {
            (CChildTarget::Static(callee), CChildStorage::Boxed(_))
                if child.downgrade_reason.is_some() =>
            {
                Some(callee.stem.clone())
            }
            _ => None,
        })
        .collect();
    let mut emitter = Emitter {
        config,
        diagnostics: unit.diagnostics.clone(),
        static_stems,
        internal_stems,
        function_plans,
        heap_api_stems,
        accessor_stems,
        layouts: layout_plan.functions.clone(),
    };
    let mut layout_insertions = BTreeMap::<usize, String>::new();
    if let Some(static_plan) = static_plan {
        for island in &static_plan.layout_islands {
            let mut layout = emitter.emit_required_static_prototypes(static_plan, island);
            for function_id in &island.layouts {
                let Some(function_plan) = static_plan.functions.get(function_id) else {
                    continue;
                };
                let Some(function) = unit.functions.iter().find(|function| {
                    function.coroutine.cfg.is_async
                        && function.coroutine.cfg.span.start_byte == function_plan.source_start
                }) else {
                    continue;
                };
                layout.push_str(&emitter.emit_async_layout(function));
            }
            layout_insertions
                .entry(island.anchor)
                .or_default()
                .push_str(&layout);
        }
    } else {
        for function in unit
            .functions
            .iter()
            .filter(|function| function.coroutine.cfg.is_async)
        {
            layout_insertions
                .entry(function.coroutine.cfg.span.start_byte)
                .or_default()
                .push_str(&emitter.emit_async_layout(function));
        }
    }
    let mut replacements = Vec::new();
    for function in &unit.functions {
        let mut replacement = layout_insertions
            .remove(&function.coroutine.cfg.span.start_byte)
            .unwrap_or_default();
        let body = if function.coroutine.cfg.is_async {
            emitter.emit_async_function(function)
        } else {
            emitter.emit_sync_function(original, function)
        };
        replacement.push_str(&body);
        replacements.push((
            function.coroutine.cfg.span.start_byte,
            function.coroutine.cfg.span.end_byte,
            replacement,
        ));
    }
    replacements.sort_by_key(|replacement| replacement.0);

    let mut output = String::new();
    if !replacements.is_empty() && !original.contains("cr_runtime.h") {
        output.push_str("#include \"cr_runtime.h\"\n\n");
    }
    let mut cursor = 0;
    for (start, end, replacement) in replacements {
        if start < cursor || end > original.len() {
            continue;
        }
        output.push_str(&original[cursor..start]);
        output.push_str(&replacement);
        cursor = end;
    }
    output.push_str(&original[cursor..]);
    output = output.replace(".hr\"", ".h\"").replace(".hr>", ".h>");

    CEmission {
        source: output,
        diagnostics: emitter.diagnostics,
    }
}

struct Emitter<'config> {
    config: &'config CEmitterConfig,
    diagnostics: Vec<Diagnostic>,
    static_stems: BTreeMap<FunctionId, String>,
    internal_stems: BTreeSet<String>,
    function_plans: BTreeMap<usize, CFunctionPlan>,
    heap_api_stems: BTreeSet<String>,
    accessor_stems: BTreeSet<String>,
    layouts: BTreeMap<usize, FunctionContextLayout>,
}

impl Emitter<'_> {
    fn emit_required_static_prototypes(
        &self,
        plan: &CStaticAwaitPlan,
        island: &LayoutIsland,
    ) -> String {
        let mut required = BTreeSet::new();
        for function in &island.layouts {
            if let Some(function_plan) = plan.functions.get(function) {
                required.extend(function_plan.required_prototypes.iter().copied());
                required.extend(function_plan.children.values().filter_map(|child| {
                    if is_same_unit_typed_child(child)
                        && let CChildTarget::Static(callee) = &child.target
                        && plan
                            .functions
                            .get(&callee.function)
                            .is_none_or(|callee_plan| callee_plan.source_start >= island.anchor)
                    {
                        Some(callee.function)
                    } else {
                        None
                    }
                }));
            }
        }
        let mut callees = BTreeMap::<_, &StaticCallee>::new();
        for function_plan in plan.functions.values() {
            for child in function_plan.children.values() {
                if let CChildTarget::Static(callee) = &child.target
                    && required.contains(&callee.function)
                {
                    callees.entry(callee.function).or_insert(callee);
                }
            }
        }
        let mut output = String::new();
        for callee in callees.values() {
            let linkage = if matches!(
                callee.linkage,
                crate::symbol_index::AsyncLinkageKey::Internal { .. }
            ) {
                "static "
            } else {
                ""
            };
            let parameters = typed_parameter_declarations(callee);
            let with_task = if parameters.is_empty() {
                format!("{} *task", callee.task_type)
            } else {
                format!("{} *task, {parameters}", callee.task_type)
            };
            let create_parameters = if parameters.is_empty() {
                "cr_error *out_error".to_owned()
            } else {
                format!("{parameters}, cr_error *out_error")
            };
            let _ = writeln!(
                output,
                "typedef struct {} {};",
                callee.task_type, callee.task_type
            );
            if callee.layout_visibility == crate::symbol_index::LayoutVisibility::Visible {
                let _ = writeln!(output, "{linkage}void {}_init({with_task});", callee.stem);
                let _ = writeln!(
                    output,
                    "{linkage}void {}_drop({} *task);",
                    callee.stem, callee.task_type
                );
            }
            let is_internal = matches!(
                callee.linkage,
                crate::symbol_index::AsyncLinkageKey::Internal { .. }
            );
            let needs_dynamic_adapters = !is_internal;
            let needs_heap_api =
                needs_dynamic_adapters || self.heap_api_stems.contains(&callee.stem);
            let needs_typed_accessors = !is_internal || self.accessor_stems.contains(&callee.stem);
            if needs_heap_api {
                let _ = writeln!(
                    output,
                    "{linkage}{} *{}_create({create_parameters});",
                    callee.task_type, callee.stem
                );
            }
            let _ = writeln!(
                output,
                "{linkage}cr_poll_status {}_poll({} *task, const cr_poll_context *poll_context);",
                callee.stem, callee.task_type
            );
            if needs_heap_api {
                let _ = writeln!(
                    output,
                    "{linkage}void {}_destroy({} *task);",
                    callee.stem, callee.task_type
                );
            }
            if needs_typed_accessors && callee.result_type.trim() != "void" {
                let _ = writeln!(
                    output,
                    "{linkage}const {} *{}_result(const {} *task);",
                    callee.result_type.trim(),
                    callee.stem,
                    callee.task_type
                );
                let _ = writeln!(
                    output,
                    "{linkage}const {} *{}_yielded(const {} *task);",
                    callee.result_type.trim(),
                    callee.stem,
                    callee.task_type
                );
            }
            if needs_typed_accessors {
                let _ = writeln!(
                    output,
                    "{linkage}const cr_error *{}_error(const {} *task);",
                    callee.stem, callee.task_type
                );
            }
            if !is_internal {
                let _ = writeln!(
                    output,
                    "{linkage}cr_awaitable {}_as_awaitable({} *task);",
                    callee.stem, callee.task_type
                );
            }
            if needs_dynamic_adapters {
                let _ = writeln!(
                    output,
                    "{linkage}cr_awaitable {}_into_awaitable({} *task);\n",
                    callee.stem, callee.task_type
                );
            }
        }
        output
    }

    fn emit_async_layout(&mut self, function: &LivenessFunction) -> String {
        let coroutine = &function.coroutine;
        let layout = self
            .layouts
            .get(&coroutine.cfg.span.start_byte)
            .expect("validated async function layout");
        let mut output = String::new();
        let _ = writeln!(
            output,
            "typedef struct {} {};",
            coroutine.task_type, coroutine.task_type
        );
        let _ = writeln!(output, "struct {} {{", coroutine.task_type);
        for slot in &layout.slots {
            match slot.kind {
                PhysicalSlotKind::Direct => {
                    let member = slot
                        .members
                        .first()
                        .expect("direct physical slot has one member");
                    let _ = writeln!(output, "    {};", member.c_declaration);
                }
                PhysicalSlotKind::Union => {
                    let _ = writeln!(output, "    union {{");
                    for member in &slot.members {
                        let _ = writeln!(output, "        {};", member.c_declaration);
                    }
                    let _ = writeln!(output, "    }} {};", slot.c_name);
                }
            }
        }
        let _ = writeln!(output, "}};\n");
        output
    }

    fn emit_async_function(&mut self, function: &LivenessFunction) -> String {
        let symbols = FunctionSymbols::new(function);
        let helpers = self.cleanup_helpers(function, &symbols);
        let coroutine = &function.coroutine;
        let context = c_identifier(&self.config.context_name);
        let return_type = coroutine.cfg.return_type.text.trim();
        let returns_value = return_type != "void";
        let layout = self
            .layouts
            .get(&coroutine.cfg.span.start_byte)
            .cloned()
            .expect("validated async function layout");
        let await_types = layout.await_result_types.clone();
        let symbol_stem = coroutine
            .poll_name
            .strip_suffix("_poll")
            .unwrap_or(&coroutine.poll_name);
        let create_name = format!("{symbol_stem}_create");
        let destroy_name = format!("{symbol_stem}_destroy");
        let result_name = format!("{symbol_stem}_result");
        let yielded_name = format!("{symbol_stem}_yielded");
        let error_name = format!("{symbol_stem}_error");
        let await_poll_name = format!("{symbol_stem}_await_poll");
        let await_error_name = format!("{symbol_stem}_await_error");
        let await_drop_name = format!("{symbol_stem}_await_drop");
        let await_destroy_name = format!("{symbol_stem}_await_destroy");
        let await_null_error_name = format!("{symbol_stem}_await_null_error");
        let as_awaitable_name = format!("{symbol_stem}_as_awaitable");
        let into_awaitable_name = format!("{symbol_stem}_into_awaitable");
        let is_internal = self.internal_stems.contains(symbol_stem);
        let needs_dynamic_adapters = !is_internal;
        let needs_heap_api = needs_dynamic_adapters || self.heap_api_stems.contains(symbol_stem);
        let needs_typed_accessors = !is_internal || self.accessor_stems.contains(symbol_stem);

        let mut output = String::new();
        write_runtime_abi_guard(&mut output);
        self.write_cleanup_helpers(&mut output, &helpers);
        let reachable_task_bindings = reachable_task_bindings(function);
        let function_plan = self
            .function_plans
            .get(&function.coroutine.cfg.span.start_byte);
        for field in function
            .lifted_fields
            .iter()
            .filter(|field| field.is_task && reachable_task_bindings.contains(&field.declaration))
        {
            let payload = format!("{}_{}_cleanup_payload", symbol_stem, field.field_name);
            let helper = format!("{}_{}_cleanup", symbol_stem, field.field_name);
            let _ = writeln!(output, "typedef struct {payload} {{");
            let typed_child = typed_binding_child(function_plan, field.declaration);
            if let Some(child) = typed_child
                && let CChildTarget::Static(callee) = &child.target
            {
                match child.effective_storage {
                    CChildStorage::Embedded(_) => {
                        let _ = writeln!(output, "    {} *slot;", callee.task_type);
                    }
                    CChildStorage::Boxed(_) => {
                        let _ = writeln!(output, "    {} **slot;", callee.task_type);
                    }
                    CChildStorage::Opaque(_) => unreachable!("typed binding can't be opaque"),
                }
            } else {
                let _ = writeln!(output, "    cr_awaitable *slot;");
            }
            let _ = writeln!(output, "    bool *active;");
            let _ = writeln!(output, "    uint64_t *generation;");
            let _ = writeln!(output, "    uint64_t captured_generation;");
            let _ = writeln!(output, "}} {payload};");
            let _ = writeln!(output, "static void {helper}(void *raw) {{");
            let _ = writeln!(output, "    {payload} *payload = ({payload} *)raw;");
            let _ = writeln!(
                output,
                "    if (!*payload->active || *payload->generation != payload->captured_generation) return;"
            );
            if let Some(child) = typed_child
                && let CChildTarget::Static(callee) = &child.target
            {
                match child.effective_storage {
                    CChildStorage::Embedded(_) => {
                        let _ = writeln!(output, "    {}_drop(payload->slot);", callee.stem);
                    }
                    CChildStorage::Boxed(_) => {
                        let _ = writeln!(output, "    {}_destroy(*payload->slot);", callee.stem);
                        let _ = writeln!(output, "    *payload->slot = NULL;");
                    }
                    CChildStorage::Opaque(_) => unreachable!("typed binding can't be opaque"),
                }
            } else {
                let _ = writeln!(
                    output,
                    "    payload->slot->vtable->drop(payload->slot->state);"
                );
            }
            let _ = writeln!(output, "    *payload->active = false;");
            let _ = writeln!(output, "}}\n");
        }

        self.write_async_init(&mut output, function, &symbols, &context);
        self.write_async_poll(
            &mut output,
            function,
            &symbols,
            &helpers,
            &await_types,
            &context,
        );
        self.write_async_drop(&mut output, function, &context);

        if needs_heap_api {
            let parameters = create_parameter_list(coroutine);
            let arguments = parameter_names(coroutine);
            let _ = writeln!(
                output,
                "{} *{}({}) {{",
                coroutine.task_type, create_name, parameters
            );
            let _ = writeln!(
                output,
                "    {} *task = ({} *)malloc(sizeof(*task));",
                coroutine.task_type, coroutine.task_type
            );
            let _ = writeln!(output, "    if (task == NULL) {{");
            let _ = writeln!(
                output,
                "        if (out_error != NULL) *out_error = (cr_error){{1006, \"async task allocation failed\"}};"
            );
            let _ = writeln!(output, "        return NULL;");
            let _ = writeln!(output, "    }}");
            let _ = writeln!(
                output,
                "    if (out_error != NULL) *out_error = (cr_error){{0, NULL}};"
            );
            let _ = writeln!(
                output,
                "    {}(task{}{});",
                coroutine.init_name,
                if arguments.is_empty() { "" } else { ", " },
                arguments
            );
            let _ = writeln!(output, "    return task;");
            let _ = writeln!(output, "}}\n");
            let _ = writeln!(
                output,
                "void {}({} *task) {{",
                destroy_name, coroutine.task_type
            );
            let _ = writeln!(output, "    if (task == NULL) return;");
            let _ = writeln!(output, "    {}(task);", coroutine.drop_name);
            let _ = writeln!(output, "    free(task);");
            let _ = writeln!(output, "}}\n");
        }

        if needs_typed_accessors {
            if returns_value {
                let result_access = required_access(Some(&layout), LogicalFieldId::Result, "task");
                let yielded_access =
                    required_access(Some(&layout), LogicalFieldId::Yielded, "task");
                let _ = writeln!(
                    output,
                    "const {return_type} *{}(const {} *task) {{ return &{result_access}; }}",
                    result_name, coroutine.task_type
                );
                let _ = writeln!(
                    output,
                    "const {return_type} *{}(const {} *task) {{ return &{yielded_access}; }}",
                    yielded_name, coroutine.task_type
                );
            }
            let error_access = required_access(Some(&layout), LogicalFieldId::Error, "task");
            let _ = writeln!(
                output,
                "const cr_error *{}(const {} *task) {{ return &{error_access}; }}\n",
                error_name, coroutine.task_type
            );
        }

        if needs_dynamic_adapters {
            let _ = writeln!(
                output,
                "static cr_poll_status {await_poll_name}(void *state, const cr_poll_context *poll_context, void *out_value) {{"
            );
            let _ = writeln!(output, "    if (state == NULL) return CR_POLL_ERROR;");
            let _ = writeln!(
                output,
                "    {} *task = ({} *)state;",
                coroutine.task_type, coroutine.task_type
            );
            let _ = writeln!(
                output,
                "    cr_poll_status status = {}(task, poll_context);",
                coroutine.poll_name
            );
            if returns_value {
                let result_access = required_access(Some(&layout), LogicalFieldId::Result, "task");
                let yielded_access =
                    required_access(Some(&layout), LogicalFieldId::Yielded, "task");
                let _ = writeln!(
                    output,
                    "    if (out_value != NULL && status == CR_POLL_READY) *({return_type} *)out_value = {result_access};"
                );
                let _ = writeln!(
                    output,
                    "    if (out_value != NULL && status == CR_POLL_YIELDED) *({return_type} *)out_value = {yielded_access};"
                );
            }
            let _ = writeln!(output, "    return status;");
            let _ = writeln!(output, "}}\n");
            let _ = writeln!(
                output,
                "static const cr_error {await_null_error_name} = {{1006, \"async task allocation failed\"}};"
            );
            let adapter_error = required_access(
                Some(&layout),
                LogicalFieldId::Error,
                &format!("((const {} *)state)", coroutine.task_type),
            );
            let _ = writeln!(
                output,
                "static const cr_error *{await_error_name}(const void *state) {{ return state != NULL ? &{adapter_error} : &{await_null_error_name}; }}"
            );
            if !is_internal {
                let _ = writeln!(
                    output,
                    "static void {await_drop_name}(void *state) {{ {}(({} *)state); }}",
                    coroutine.drop_name, coroutine.task_type
                );
            }
            let _ = writeln!(
                output,
                "static void {await_destroy_name}(void *state) {{ {destroy_name}(({} *)state); }}",
                coroutine.task_type
            );
            let value_size = if returns_value {
                format!("sizeof({return_type})")
            } else {
                "0u".to_owned()
            };
            let value_align = if returns_value {
                format!("_Alignof({return_type})")
            } else {
                "0u".to_owned()
            };
            let borrowed_vtable = format!("{symbol_stem}_borrowed_awaitable_vtable");
            let owning_vtable = format!("{symbol_stem}_owning_awaitable_vtable");
            let vtables = if is_internal {
                vec![(owning_vtable.as_str(), await_destroy_name.as_str())]
            } else {
                vec![
                    (borrowed_vtable.as_str(), await_drop_name.as_str()),
                    (owning_vtable.as_str(), await_destroy_name.as_str()),
                ]
            };
            for (vtable, drop_callback) in vtables {
                let _ = writeln!(output, "static const cr_awaitable_vtable {vtable} = {{");
                let _ = writeln!(output, "    CR_AWAITABLE_VTABLE_ABI_VERSION,");
                let _ = writeln!(output, "    sizeof(cr_awaitable_vtable),");
                let _ = writeln!(output, "    CR_AWAITABLE_CAN_YIELD,");
                let _ = writeln!(output, "    0u,");
                let _ = writeln!(output, "    {await_poll_name},");
                let _ = writeln!(output, "    {await_error_name},");
                let _ = writeln!(output, "    {drop_callback},");
                let _ = writeln!(output, "    {value_size},");
                let _ = writeln!(output, "    {value_align}");
                let _ = writeln!(output, "}};\n");
            }
            if !is_internal {
                let _ = writeln!(
                    output,
                    "cr_awaitable {as_awaitable_name}({} *task) {{",
                    coroutine.task_type
                );
                let _ = writeln!(
                    output,
                    "    return (cr_awaitable){{task, &{borrowed_vtable}}};"
                );
                let _ = writeln!(output, "}}\n");
            }
            let _ = writeln!(
                output,
                "cr_awaitable {into_awaitable_name}({} *task) {{",
                coroutine.task_type
            );
            let _ = writeln!(
                output,
                "    return (cr_awaitable){{task, &{owning_vtable}}};"
            );
            let _ = writeln!(output, "}}\n");
        }
        output
    }

    fn emit_sync_function(&mut self, original: &str, function: &LivenessFunction) -> String {
        let symbols = FunctionSymbols::new(function);
        let helpers = self.cleanup_helpers(function, &symbols);
        let cfg = &function.coroutine.cfg;
        let signature = &original[cfg.span.start_byte..cfg.body_span.start_byte];
        let mut output = String::new();
        write_runtime_abi_guard(&mut output);
        self.write_cleanup_helpers(&mut output, &helpers);
        output.push_str(signature.trim_end());
        output.push_str(" {\n    cr_cleanup_stack cr_cleanups;\n");
        output.push_str("    cr_cleanup_stack_init(&cr_cleanups);\n");
        let _ = writeln!(output, "    goto cr_b{};", cfg.entry.0);
        let no_await_types = BTreeMap::new();
        let environment = FunctionEmission {
            function,
            symbols: &symbols,
            helpers: &helpers,
            await_types: &no_await_types,
            layout: None,
            async_context: None,
        };
        let reachable = reachable_blocks(cfg);
        for block in cfg
            .blocks
            .iter()
            .filter(|block| reachable.contains(&block.id))
        {
            self.write_block(&mut output, &environment, block);
        }
        output.push_str("}\n");
        output
    }

    fn write_async_init(
        &mut self,
        output: &mut String,
        function: &LivenessFunction,
        symbols: &FunctionSymbols,
        context: &str,
    ) {
        let coroutine = &function.coroutine;
        let layout = self.layouts.get(&coroutine.cfg.span.start_byte);
        let state = required_access(layout, LogicalFieldId::State, context);
        let status = required_access(layout, LogicalFieldId::Status, context);
        let cleanups = required_access(layout, LogicalFieldId::Cleanups, context);
        let _ = writeln!(
            output,
            "void {}({} *{}{}) {{",
            coroutine.init_name,
            coroutine.task_type,
            context,
            parameter_list_with_leading_comma(coroutine)
        );
        let _ = writeln!(output, "    memset({}, 0, sizeof(*{}));", context, context);
        let _ = writeln!(output, "    {state} = 0;");
        let _ = writeln!(output, "    {status} = CR_POLL_PENDING;");
        let _ = writeln!(output, "    cr_cleanup_stack_init(&{cleanups});");
        for parameter in &coroutine.cfg.parameters {
            if let Some(field) = symbols.field(parameter.id) {
                let target =
                    required_access(layout, LogicalFieldId::Lifted(field.declaration), context);
                let _ = writeln!(output, "    {target} = {};", parameter.name);
            }
        }
        let _ = writeln!(output, "}}\n");
    }

    fn write_async_poll(
        &mut self,
        output: &mut String,
        function: &LivenessFunction,
        symbols: &FunctionSymbols,
        helpers: &HashMap<CleanupId, CleanupHelper>,
        await_types: &BTreeMap<AwaitSlotId, String>,
        context: &str,
    ) {
        let coroutine = &function.coroutine;
        let layout = self
            .layouts
            .get(&coroutine.cfg.span.start_byte)
            .cloned()
            .expect("validated async function layout");
        let state_access = required_access(Some(&layout), LogicalFieldId::State, context);
        let status_access = required_access(Some(&layout), LogicalFieldId::Status, context);
        let error_access = required_access(Some(&layout), LogicalFieldId::Error, context);
        let cleanups_access = required_access(Some(&layout), LogicalFieldId::Cleanups, context);
        let _ = writeln!(
            output,
            "cr_poll_status {}({} *{}, const cr_poll_context *poll_context) {{",
            coroutine.poll_name, coroutine.task_type, context
        );
        let _ = writeln!(output, "    if ({} == NULL) return CR_POLL_ERROR;", context);
        let _ = writeln!(
            output,
            "    if ({status_access} == CR_POLL_READY || {status_access} == CR_POLL_ERROR || {status_access} == CR_POLL_CANCELED) return {status_access};"
        );
        let _ = writeln!(
            output,
            "    if (poll_context != NULL && (poll_context->abi_version < CR_POLL_CONTEXT_ABI_VERSION || poll_context->struct_size < CR_POLL_CONTEXT_V1_MIN_SIZE || ((poll_context->available_capabilities & CR_POLL_CAP_WAKER) != 0u && poll_context->waker == NULL))) {{"
        );
        let _ = writeln!(
            output,
            "        {error_access} = (cr_error){{CR_ERROR_INVALID_POLL_CONTEXT, \"invalid poll context\"}};"
        );
        let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
        let _ = writeln!(output, "        {status_access} = CR_POLL_ERROR;");
        let _ = writeln!(output, "        return {status_access};");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(
            output,
            "    if ({status_access} == CR_POLL_YIELDED) {status_access} = CR_POLL_PENDING;"
        );
        match self.config.backend {
            CBackend::Portable => {
                let _ = writeln!(output, "    switch ({state_access}) {{");
                for state in &coroutine.states {
                    let _ = writeln!(
                        output,
                        "    case {}: goto cr_b{};",
                        state.state, state.block.0
                    );
                }
                let _ = writeln!(output, "    default:");
                self.write_invalid_state(output, Some(&layout), context);
                let _ = writeln!(output, "    }}");
            }
            CBackend::ComputedGoto => {
                let labels = coroutine
                    .states
                    .iter()
                    .map(|state| format!("&&cr_b{}", state.block.0))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(
                    output,
                    "    static void *const cr_dispatch[] = {{{labels}}};"
                );
                let _ = writeln!(
                    output,
                    "    if ({state_access} >= (sizeof(cr_dispatch) / sizeof(cr_dispatch[0]))) {{"
                );
                self.write_invalid_state(output, Some(&layout), context);
                let _ = writeln!(output, "    }}");
                let _ = writeln!(output, "    goto *cr_dispatch[{state_access}];");
            }
        }
        let environment = FunctionEmission {
            function,
            symbols,
            helpers,
            await_types,
            layout: Some(&layout),
            async_context: Some(context),
        };
        let reachable = reachable_blocks(&coroutine.cfg);
        for block in coroutine
            .cfg
            .blocks
            .iter()
            .filter(|block| reachable.contains(&block.id))
        {
            self.write_block(output, &environment, block);
        }
        let _ = writeln!(output, "}}\n");
    }

    fn write_async_drop(
        &mut self,
        output: &mut String,
        function: &LivenessFunction,
        context: &str,
    ) {
        let coroutine = &function.coroutine;
        let _ = writeln!(
            output,
            "void {}({} *{}) {{",
            coroutine.drop_name, coroutine.task_type, context
        );
        let _ = writeln!(output, "    if ({} == NULL) return;", context);
        let binding_slots: HashSet<_> = coroutine
            .await_plan
            .edges
            .iter()
            .filter_map(|edge| {
                let child = coroutine
                    .await_plan
                    .children
                    .iter()
                    .find(|child| child.id == edge.instance)?;
                matches!(child.origin, ChildOrigin::Binding(_)).then_some(edge.slot)
            })
            .collect();
        let function_plan = self
            .function_plans
            .get(&function.coroutine.cfg.span.start_byte);
        let layout = self.layouts.get(&function.coroutine.cfg.span.start_byte);
        if let Some(function_plan) = function_plan {
            for child in function_plan.children.values() {
                let (CChildTarget::Static(callee), ChildOrigin::Direct(_)) =
                    (&child.target, child.origin)
                else {
                    continue;
                };
                match child.effective_storage {
                    CChildStorage::Embedded(_) => {
                        let payload = required_access(
                            layout,
                            LogicalFieldId::DirectChildPayload(child.instance),
                            context,
                        );
                        let active = required_access(
                            layout,
                            LogicalFieldId::DirectChildActive(child.instance),
                            context,
                        );
                        let _ = writeln!(output, "    if ({active}) {{");
                        let _ = writeln!(output, "        {}_drop(&{payload});", callee.stem);
                        let _ = writeln!(output, "        {active} = false;");
                        let _ = writeln!(output, "    }}");
                    }
                    CChildStorage::Boxed(_) => {
                        let payload = required_access(
                            layout,
                            LogicalFieldId::DirectChildPayload(child.instance),
                            context,
                        );
                        let active = required_access(
                            layout,
                            LogicalFieldId::DirectChildActive(child.instance),
                            context,
                        );
                        let _ = writeln!(output, "    if ({active}) {{");
                        let _ = writeln!(output, "        {}_destroy({payload});", callee.stem);
                        let _ = writeln!(output, "        {payload} = NULL;");
                        let _ = writeln!(output, "        {active} = false;");
                        let _ = writeln!(output, "    }}");
                    }
                    CChildStorage::Opaque(_) => {}
                }
            }
        }
        let typed_slots: HashSet<_> = function_plan
            .into_iter()
            .flat_map(|plan| plan.edge_children.iter())
            .filter_map(|(edge, instance)| {
                let child = function_plan?.children.get(instance)?;
                is_typed_static_child(child)
                    .then(|| {
                        coroutine
                            .await_plan
                            .edges
                            .iter()
                            .find(|planned| planned.id == *edge)
                            .map(|planned| planned.slot)
                    })
                    .flatten()
            })
            .collect();
        for slot in &coroutine.await_slots {
            if typed_slots.contains(&slot.id) {
                continue;
            }
            let awaitable = required_access(layout, LogicalFieldId::Awaitable(slot.id), context);
            let active = required_access(layout, LogicalFieldId::AwaitableActive(slot.id), context);
            let _ = writeln!(output, "    if ({active}) {{");
            if !binding_slots.contains(&slot.id) {
                let _ = writeln!(
                    output,
                    "        if ({awaitable}.vtable != NULL && {awaitable}.vtable->drop != NULL) {awaitable}.vtable->drop({awaitable}.state);"
                );
            }
            let _ = writeln!(output, "        {active} = false;");
            let _ = writeln!(output, "    }}");
        }
        let _ = writeln!(
            output,
            "    cr_cleanup_stack_destroy(&{});",
            required_access(layout, LogicalFieldId::Cleanups, context)
        );
        let status = required_access(layout, LogicalFieldId::Status, context);
        let _ = writeln!(
            output,
            "    if ({status} != CR_POLL_READY && {status} != CR_POLL_ERROR) {status} = CR_POLL_CANCELED;"
        );
        let _ = writeln!(output, "}}\n");
    }

    fn write_block(
        &mut self,
        output: &mut String,
        environment: &FunctionEmission<'_>,
        block: &BasicBlock,
    ) {
        let _ = writeln!(output, "cr_b{}: ;", block.id.0);
        for instruction in &block.instructions {
            self.write_instruction(output, environment, block, instruction);
        }
        self.write_terminator(output, environment, block);
    }

    #[allow(clippy::too_many_arguments)]
    fn write_typed_binding_declaration(
        &mut self,
        output: &mut String,
        function: &LivenessFunction,
        block: &BasicBlock,
        declaration: &HirDeclaration,
        field: &LiftedField,
        context: &str,
        child: &CChildPlan,
    ) -> bool {
        let CChildTarget::Static(callee) = &child.target else {
            return false;
        };
        if !is_typed_static_child(child) {
            return false;
        }
        let Some(HirExprKind::AsyncCall { arguments, .. }) = declaration
            .initializer
            .as_ref()
            .map(|initializer| &initializer.kind)
        else {
            self.diagnostics.push(Diagnostic {
                code: "CRC5007",
                severity: DiagnosticSeverity::Error,
                message: "embedded task binding has no static async initializer".to_owned(),
                primary_span: declaration.span.clone(),
                related: Vec::new(),
            });
            return true;
        };
        let parent_stem = function
            .coroutine
            .poll_name
            .strip_suffix("_poll")
            .unwrap_or(&function.coroutine.poll_name);
        let payload = format!("{}_{}_cleanup_payload", parent_stem, field.field_name);
        let helper = format!("{}_{}_cleanup", parent_stem, field.field_name);
        let layout = self.layouts.get(&function.coroutine.cfg.span.start_byte);
        let value_access =
            required_access(layout, LogicalFieldId::Lifted(field.declaration), context);
        let active_access = required_access(
            layout,
            LogicalFieldId::BindingActive(field.declaration),
            context,
        );
        let generation_access = required_access(
            layout,
            LogicalFieldId::BindingGeneration(field.declaration),
            context,
        );
        let cleanups_access = required_access(layout, LogicalFieldId::Cleanups, context);
        let error_access = required_access(layout, LogicalFieldId::Error, context);
        let status_access = required_access(layout, LogicalFieldId::Status, context);
        let _ = writeln!(output, "    if ({active_access}) {{");
        match child.effective_storage {
            CChildStorage::Embedded(_) => {
                let _ = writeln!(output, "        {}_drop(&{value_access});", callee.stem);
            }
            CChildStorage::Boxed(_) => {
                let _ = writeln!(output, "        {}_destroy({value_access});", callee.stem);
                let _ = writeln!(output, "        {value_access} = NULL;");
            }
            CChildStorage::Opaque(_) => unreachable!("typed binding can't be opaque"),
        }
        let _ = writeln!(output, "        {active_access} = false;");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    {generation_access}++;");

        let mut rendered_arguments = Vec::with_capacity(arguments.len());
        for (index, argument) in arguments.iter().enumerate() {
            let rendered = render_expression(
                argument,
                block,
                &function.lifted_fields,
                self.layouts.get(&function.coroutine.cfg.span.start_byte),
                Some(context),
                &self.config.prefix,
                &self.static_stems,
            );
            if let Some(parameter) = callee.parameters.get(index) {
                let temporary = format!("cr_binding_{}_arg_{index}", declaration.id.0);
                let _ = writeln!(
                    output,
                    "    {} = {rendered};",
                    render_typed_name(parameter.adjusted_type.trim(), &temporary)
                );
                rendered_arguments.push(temporary);
            } else {
                rendered_arguments.push(rendered);
            }
        }
        let arguments = rendered_arguments.join(", ");
        match child.effective_storage {
            CChildStorage::Embedded(_) => {
                let _ = writeln!(
                    output,
                    "    {}_init(&{value_access}{}{});",
                    callee.stem,
                    if arguments.is_empty() { "" } else { ", " },
                    arguments
                );
            }
            CChildStorage::Boxed(_) => {
                let create_error = format!("cr_binding_{}_create_error", declaration.id.0);
                let _ = writeln!(output, "    cr_error {create_error} = {{0, NULL}};");
                let _ = writeln!(
                    output,
                    "    {value_access} = {}_create({}{}&{create_error});",
                    callee.stem,
                    arguments,
                    if arguments.is_empty() { "" } else { ", " }
                );
                let _ = writeln!(output, "    if ({value_access} == NULL) {{");
                let _ = writeln!(
                    output,
                    "        {error_access} = {create_error}.code != 0 ? {create_error} : (cr_error){{1006, \"async task allocation failed\"}};"
                );
                let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
                let _ = writeln!(output, "        {status_access} = CR_POLL_ERROR;");
                let _ = writeln!(output, "        return {status_access};");
                let _ = writeln!(output, "    }}");
            }
            CChildStorage::Opaque(_) => unreachable!("typed binding can't be opaque"),
        }
        let _ = writeln!(output, "    {active_access} = true;");
        let cleanup_payload = format!("cr_binding_payload_{}", declaration.id.0);
        let _ = writeln!(
            output,
            "    {payload} {cleanup_payload} = {{&{value_access}, &{active_access}, &{generation_access}, {generation_access}}};"
        );
        let _ = writeln!(
            output,
            "    if (!cr_cleanup_push(&{cleanups_access}, {}u, {helper}, &{cleanup_payload}, sizeof({cleanup_payload}))) {{",
            declaration.scope.0
        );
        let _ = writeln!(output, "        {helper}(&{cleanup_payload});");
        self.write_async_error(output, layout, context, 1002, "cleanup allocation failed");
        let _ = writeln!(output, "    }}");
        true
    }

    fn write_instruction(
        &mut self,
        output: &mut String,
        environment: &FunctionEmission<'_>,
        block: &BasicBlock,
        instruction: &CfgInstruction,
    ) {
        let function = environment.function;
        let symbols = environment.symbols;
        let helpers = environment.helpers;
        let layout = environment.layout;
        let async_context = environment.async_context;
        match instruction {
            CfgInstruction::Source(fragment) => {
                let text = rewrite_source(
                    &fragment.text,
                    fragment.span.start_byte,
                    &block.scope_stack,
                    &function.lifted_fields,
                    environment.layout,
                    async_context,
                );
                let _ = writeln!(output, "    {text}");
            }
            CfgInstruction::Declaration(declaration) => {
                let field = symbols.field(declaration.id);
                let planned_binding = self
                    .function_plans
                    .get(&function.coroutine.cfg.span.start_byte)
                    .and_then(|plan| typed_binding_child(Some(plan), declaration.id))
                    .cloned();
                if let (Some(field), Some(context), Some(child)) =
                    (field, async_context, planned_binding.as_ref())
                    && field.is_task
                    && self.write_typed_binding_declaration(
                        output,
                        function,
                        block,
                        declaration,
                        field,
                        context,
                        child,
                    )
                {
                    return;
                }
                if let Some(child) = planned_binding.as_ref()
                    && self.reject_static_dynamic_fallback(child, &declaration.span, "task binding")
                {
                    return;
                }
                let initializer = declaration.initializer.as_ref().map(|expression| {
                    render_expression(
                        expression,
                        block,
                        &function.lifted_fields,
                        environment.layout,
                        async_context,
                        &self.config.prefix,
                        &self.static_stems,
                    )
                });
                if let Some(field) = field {
                    if let (Some(context), Some(initializer)) = (async_context, initializer) {
                        if field.is_task {
                            let symbol_stem = function
                                .coroutine
                                .poll_name
                                .strip_suffix("_poll")
                                .unwrap_or(&function.coroutine.poll_name);
                            let payload =
                                format!("{}_{}_cleanup_payload", symbol_stem, field.field_name);
                            let helper = format!("{}_{}_cleanup", symbol_stem, field.field_name);
                            let value_access = required_access(
                                layout,
                                LogicalFieldId::Lifted(field.declaration),
                                context,
                            );
                            let active_access = required_access(
                                layout,
                                LogicalFieldId::BindingActive(field.declaration),
                                context,
                            );
                            let generation_access = required_access(
                                layout,
                                LogicalFieldId::BindingGeneration(field.declaration),
                                context,
                            );
                            let _ = writeln!(
                                output,
                                "    if ({active_access}) {{ {value_access}.vtable->drop({value_access}.state); {active_access} = false; }}"
                            );
                            let _ = writeln!(output, "    {generation_access}++;");
                            let _ = writeln!(output, "    {value_access} = {initializer};");
                            self.write_awaitable_activation_validation(
                                output,
                                layout,
                                context,
                                &value_access,
                                Some(declaration.ty.text.trim()),
                                "    ",
                                true,
                            );
                            let _ = writeln!(output, "    {active_access} = true;");
                            let cleanup_payload =
                                format!("cr_binding_payload_{}", declaration.id.0);
                            let _ = writeln!(
                                output,
                                "    {payload} {cleanup_payload} = {{&{value_access}, &{active_access}, &{generation_access}, {generation_access}}};"
                            );
                            let _ = writeln!(
                                output,
                                "    if (!cr_cleanup_push(&{}, {}u, {helper}, &{cleanup_payload}, sizeof({cleanup_payload}))) {{",
                                required_access(layout, LogicalFieldId::Cleanups, context),
                                declaration.scope.0
                            );
                            let _ = writeln!(output, "        {helper}(&{cleanup_payload});");
                            self.write_async_error(
                                output,
                                layout,
                                context,
                                1002,
                                "cleanup allocation failed",
                            );
                            let _ = writeln!(output, "    }}");
                        } else if field.ty.text.contains('[') {
                            let target = required_access(
                                layout,
                                LogicalFieldId::Lifted(field.declaration),
                                context,
                            );
                            let _ = writeln!(
                                output,
                                "    memcpy({target}, ({}){initializer}, sizeof({target}));",
                                field.ty.text.trim(),
                            );
                        } else {
                            let target = required_access(
                                layout,
                                LogicalFieldId::Lifted(field.declaration),
                                context,
                            );
                            let _ = writeln!(output, "    {target} = {initializer};");
                        }
                    }
                } else if let Some(initializer) = initializer {
                    let ty = if declaration.is_task {
                        "cr_awaitable"
                    } else {
                        declaration.ty.text.trim()
                    };
                    let _ = writeln!(
                        output,
                        "    {} = {};",
                        render_typed_name(ty, &declaration.name),
                        initializer
                    );
                } else {
                    let ty = if declaration.is_task {
                        "cr_awaitable"
                    } else {
                        declaration.ty.text.trim()
                    };
                    let _ = writeln!(output, "    {};", render_typed_name(ty, &declaration.name));
                }
            }
            CfgInstruction::AssignAwaitResult {
                destination, slot, ..
            } => {
                let target = symbols
                    .field(*destination)
                    .and_then(|field| {
                        async_context.map(|context| {
                            required_access(
                                layout,
                                LogicalFieldId::Lifted(field.declaration),
                                context,
                            )
                        })
                    })
                    .unwrap_or_else(|| symbols.name(*destination).to_owned());
                let context = async_context.unwrap_or("ctx");
                let result = required_access(layout, LogicalFieldId::AwaitResult(*slot), context);
                let _ = writeln!(output, "    {target} = {result};");
            }
            CfgInstruction::AssignExpression {
                destination,
                expression,
                ..
            } => {
                let target = symbols
                    .field(*destination)
                    .and_then(|field| {
                        async_context.map(|context| {
                            required_access(
                                layout,
                                LogicalFieldId::Lifted(field.declaration),
                                context,
                            )
                        })
                    })
                    .unwrap_or_else(|| symbols.name(*destination).to_owned());
                let expression = render_expression(
                    expression,
                    block,
                    &function.lifted_fields,
                    environment.layout,
                    async_context,
                    &self.config.prefix,
                    &self.static_stems,
                );
                let _ = writeln!(output, "    {target} = {expression};");
            }
            CfgInstruction::AssignExpressionSlot {
                slot, expression, ..
            } => {
                let Some(context) = async_context else {
                    self.diagnostics.push(Diagnostic {
                        code: "CRC5006",
                        severity: DiagnosticSeverity::Error,
                        message: "persistent expression slot reached synchronous emission"
                            .to_owned(),
                        primary_span: expression.span.clone(),
                        related: Vec::new(),
                    });
                    return;
                };
                let expression = render_expression(
                    expression,
                    block,
                    &function.lifted_fields,
                    environment.layout,
                    async_context,
                    &self.config.prefix,
                    &self.static_stems,
                );
                let target = required_access(layout, LogicalFieldId::AwaitResult(*slot), context);
                let _ = writeln!(output, "    {target} = {expression};");
            }
            CfgInstruction::Evaluate(expression) => {
                let expression = render_expression(
                    expression,
                    block,
                    &function.lifted_fields,
                    environment.layout,
                    async_context,
                    &self.config.prefix,
                    &self.static_stems,
                );
                let _ = writeln!(output, "    {expression};");
            }
            CfgInstruction::RegisterDefer(defer) => {
                self.diagnostics.push(Diagnostic {
                    code: "CRC5002",
                    severity: DiagnosticSeverity::Error,
                    message: "unlowered defer reached C emission".to_owned(),
                    primary_span: defer.span.clone(),
                    related: Vec::new(),
                });
            }
            CfgInstruction::PushCleanup(registration) => {
                let Some(helper) = helpers.get(&registration.id) else {
                    return;
                };
                let payload = format!("cr_payload_{}", registration.id.0);
                let _ = writeln!(output, "    {} {} = {{", helper.payload_type, payload);
                if helper.argument_types.is_empty() {
                    let _ = writeln!(output, "        0");
                } else {
                    for (index, argument) in registration.arguments.iter().enumerate() {
                        let argument = render_expression(
                            argument,
                            block,
                            &function.lifted_fields,
                            environment.layout,
                            async_context,
                            &self.config.prefix,
                            &self.static_stems,
                        );
                        let comma = if index + 1 == registration.arguments.len() {
                            ""
                        } else {
                            ","
                        };
                        let _ = writeln!(output, "        {argument}{comma}");
                    }
                }
                let _ = writeln!(output, "    }};");
                let stack = async_context
                    .map(|context| {
                        format!(
                            "&{}",
                            required_access(layout, LogicalFieldId::Cleanups, context)
                        )
                    })
                    .unwrap_or_else(|| "&cr_cleanups".to_owned());
                let _ = writeln!(
                    output,
                    "    if (!cr_cleanup_push({stack}, {}u, {}, &{}, sizeof({}))) {{",
                    registration.scope.0, helper.run_function, payload, payload
                );
                if let Some(context) = async_context {
                    let error = required_access(layout, LogicalFieldId::Error, context);
                    let cleanups = required_access(layout, LogicalFieldId::Cleanups, context);
                    let status = required_access(layout, LogicalFieldId::Status, context);
                    let _ = writeln!(
                        output,
                        "        {error} = (cr_error){{1002, \"cleanup allocation failed\"}};"
                    );
                    let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups});");
                    let _ = writeln!(output, "        {status} = CR_POLL_ERROR;");
                    let _ = writeln!(output, "        return {status};");
                } else {
                    let _ = writeln!(output, "        cr_oom_abort();");
                }
                let _ = writeln!(output, "    }}");
            }
            CfgInstruction::RunCleanups { exited_scopes } => {
                let stack = async_context
                    .map(|context| {
                        format!(
                            "&{}",
                            required_access(layout, LogicalFieldId::Cleanups, context)
                        )
                    })
                    .unwrap_or_else(|| "&cr_cleanups".to_owned());
                for scope in exited_scopes {
                    let _ = writeln!(output, "    cr_cleanup_run_scope({stack}, {}u);", scope.0);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_embedded_direct_suspend(
        &mut self,
        output: &mut String,
        function: &LivenessFunction,
        block: &BasicBlock,
        operand: &HirExpr,
        slot: AwaitSlotId,
        continuation: BlockId,
        context: &str,
        child: &CChildPlan,
    ) -> bool {
        let (
            CChildTarget::Static(callee),
            CChildStorage::Embedded(child_slot),
            ChildOrigin::Direct(_),
            HirExprKind::AsyncCall { arguments, .. },
        ) = (
            &child.target,
            child.effective_storage,
            child.origin,
            &operand.kind,
        )
        else {
            return false;
        };
        let layout = self.layouts.get(&function.coroutine.cfg.span.start_byte);
        let child_access = required_access(
            layout,
            LogicalFieldId::DirectChildPayload(child.instance),
            context,
        );
        let active_access = required_access(
            layout,
            LogicalFieldId::DirectChildActive(child.instance),
            context,
        );
        let state_access = required_access(layout, LogicalFieldId::State, context);
        let error_access = required_access(layout, LogicalFieldId::Error, context);
        let cleanups_access = required_access(layout, LogicalFieldId::Cleanups, context);
        let status_access = required_access(layout, LogicalFieldId::Status, context);
        let yielded_access = required_access(layout, LogicalFieldId::Yielded, context);
        let result_access =
            layout.and_then(|layout| layout.access(LogicalFieldId::AwaitResult(slot), context));
        let state = function
            .coroutine
            .state_by_block
            .get(&block.id)
            .copied()
            .unwrap_or_default();

        let _ = writeln!(output, "    if (!{active_access}) {{");
        let mut rendered_arguments = Vec::with_capacity(arguments.len());
        for (index, argument) in arguments.iter().enumerate() {
            let rendered = render_expression(
                argument,
                block,
                &function.lifted_fields,
                self.layouts.get(&function.coroutine.cfg.span.start_byte),
                Some(context),
                &self.config.prefix,
                &self.static_stems,
            );
            if let Some(parameter) = callee.parameters.get(index) {
                let temporary = format!("cr_child_{}_arg_{index}", child_slot.0);
                let _ = writeln!(
                    output,
                    "        {} = {rendered};",
                    render_typed_name(parameter.adjusted_type.trim(), &temporary)
                );
                rendered_arguments.push(temporary);
            } else {
                rendered_arguments.push(rendered);
            }
        }
        let arguments = rendered_arguments.join(", ");
        let _ = writeln!(
            output,
            "        {}_init(&{child_access}{}{});",
            callee.stem,
            if arguments.is_empty() { "" } else { ", " },
            arguments
        );
        let _ = writeln!(output, "        {active_access} = true;");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    {state_access} = {state}u;");
        let status = format!("cr_await_{}_status", slot.0);
        let _ = writeln!(
            output,
            "    cr_poll_status {status} = {}_poll(&{child_access}, poll_context);",
            callee.stem
        );
        let _ = writeln!(
            output,
            "    if ({status} == CR_POLL_PENDING) return {status};"
        );
        let _ = writeln!(output, "    if ({status} == CR_POLL_READY) {{");
        if callee.result_type.trim() != "void" {
            let result_access = result_access
                .as_deref()
                .expect("non-void child has an await-result placement");
            let _ = writeln!(output, "        {result_access} = {child_access}.result;");
        }
        let _ = writeln!(output, "        {}_drop(&{child_access});", callee.stem);
        let _ = writeln!(output, "        {active_access} = false;");
        let _ = writeln!(output, "        goto cr_b{};", continuation.0);
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    if ({status} == CR_POLL_ERROR) {{");
        let _ = writeln!(output, "        {error_access} = {child_access}.error;");
        let _ = writeln!(output, "        {}_drop(&{child_access});", callee.stem);
        let _ = writeln!(output, "        {active_access} = false;");
        let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
        let _ = writeln!(output, "        {status_access} = CR_POLL_ERROR;");
        let _ = writeln!(output, "        return {status_access};");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    if ({status} == CR_POLL_CANCELED) {{");
        let _ = writeln!(output, "        {}_drop(&{child_access});", callee.stem);
        let _ = writeln!(output, "        {active_access} = false;");
        let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
        let _ = writeln!(output, "        {status_access} = CR_POLL_CANCELED;");
        let _ = writeln!(output, "        return {status_access};");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    if ({status} == CR_POLL_YIELDED) {{");
        let return_type = function.coroutine.cfg.return_type.text.trim();
        if callee.result_type.trim() == return_type && return_type != "void" {
            let _ = writeln!(output, "        {yielded_access} = {child_access}.yielded;");
            let _ = writeln!(output, "        {status_access} = CR_POLL_YIELDED;");
            let _ = writeln!(output, "        return {status_access};");
        } else {
            let _ = writeln!(output, "        {}_drop(&{child_access});", callee.stem);
            let _ = writeln!(output, "        {active_access} = false;");
            self.write_async_error(
                output,
                layout,
                context,
                1005,
                "yielded awaitable has an incompatible value type",
            );
        }
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    {}_drop(&{child_access});", callee.stem);
        let _ = writeln!(output, "    {active_access} = false;");
        self.write_async_error_at(
            output,
            layout,
            context,
            1106,
            "invalid awaitable poll status",
            "    ",
        );
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn write_boxed_direct_suspend(
        &mut self,
        output: &mut String,
        function: &LivenessFunction,
        block: &BasicBlock,
        operand: &HirExpr,
        slot: AwaitSlotId,
        continuation: BlockId,
        context: &str,
        child: &CChildPlan,
    ) -> bool {
        let (
            CChildTarget::Static(callee),
            CChildStorage::Boxed(child_slot),
            ChildOrigin::Direct(_),
            HirExprKind::AsyncCall { arguments, .. },
        ) = (
            &child.target,
            child.effective_storage,
            child.origin,
            &operand.kind,
        )
        else {
            return false;
        };
        let layout = self.layouts.get(&function.coroutine.cfg.span.start_byte);
        let child_access = required_access(
            layout,
            LogicalFieldId::DirectChildPayload(child.instance),
            context,
        );
        let active_access = required_access(
            layout,
            LogicalFieldId::DirectChildActive(child.instance),
            context,
        );
        let state_access = required_access(layout, LogicalFieldId::State, context);
        let error_access = required_access(layout, LogicalFieldId::Error, context);
        let cleanups_access = required_access(layout, LogicalFieldId::Cleanups, context);
        let status_access = required_access(layout, LogicalFieldId::Status, context);
        let yielded_access = required_access(layout, LogicalFieldId::Yielded, context);
        let result_access =
            layout.and_then(|layout| layout.access(LogicalFieldId::AwaitResult(slot), context));
        let uses_accessors = callee.layout_visibility
            == crate::symbol_index::LayoutVisibility::Opaque
            || child.downgrade_reason.is_some();
        let state = function
            .coroutine
            .state_by_block
            .get(&block.id)
            .copied()
            .unwrap_or_default();
        let _ = writeln!(output, "    if (!{active_access}) {{");
        let mut rendered_arguments = Vec::with_capacity(arguments.len());
        for (index, argument) in arguments.iter().enumerate() {
            let rendered = render_expression(
                argument,
                block,
                &function.lifted_fields,
                self.layouts.get(&function.coroutine.cfg.span.start_byte),
                Some(context),
                &self.config.prefix,
                &self.static_stems,
            );
            if let Some(parameter) = callee.parameters.get(index) {
                let temporary = format!("cr_boxed_{}_arg_{index}", child_slot.0);
                let _ = writeln!(
                    output,
                    "        {} = {rendered};",
                    render_typed_name(parameter.adjusted_type.trim(), &temporary)
                );
                rendered_arguments.push(temporary);
            } else {
                rendered_arguments.push(rendered);
            }
        }
        let arguments = rendered_arguments.join(", ");
        let create_error = format!("cr_boxed_{}_create_error", child_slot.0);
        let _ = writeln!(output, "        cr_error {create_error} = {{0, NULL}};");
        let _ = writeln!(
            output,
            "        {child_access} = {}_create({}{}&{create_error});",
            callee.stem,
            arguments,
            if arguments.is_empty() { "" } else { ", " }
        );
        let _ = writeln!(output, "        if ({child_access} == NULL) {{");
        let _ = writeln!(
            output,
            "            {error_access} = {create_error}.code != 0 ? {create_error} : (cr_error){{1006, \"async task allocation failed\"}};"
        );
        let _ = writeln!(
            output,
            "            cr_cleanup_run_all(&{cleanups_access});"
        );
        let _ = writeln!(output, "            {status_access} = CR_POLL_ERROR;");
        let _ = writeln!(output, "            return {status_access};");
        let _ = writeln!(output, "        }}");
        let _ = writeln!(output, "        {active_access} = true;");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    {state_access} = {state}u;");
        let status = format!("cr_await_{}_status", slot.0);
        let _ = writeln!(
            output,
            "    cr_poll_status {status} = {}_poll({child_access}, poll_context);",
            callee.stem
        );
        let _ = writeln!(
            output,
            "    if ({status} == CR_POLL_PENDING) return {status};"
        );
        let _ = writeln!(output, "    if ({status} == CR_POLL_READY) {{");
        if callee.result_type.trim() != "void" {
            let result_access = result_access
                .as_deref()
                .expect("non-void child has an await-result placement");
            if uses_accessors {
                let _ = writeln!(
                    output,
                    "        {result_access} = *{}_result({child_access});",
                    callee.stem
                );
            } else {
                let _ = writeln!(output, "        {result_access} = {child_access}->result;");
            }
        }
        let _ = writeln!(output, "        {}_destroy({child_access});", callee.stem);
        let _ = writeln!(output, "        {child_access} = NULL;");
        let _ = writeln!(output, "        {active_access} = false;");
        let _ = writeln!(output, "        goto cr_b{};", continuation.0);
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    if ({status} == CR_POLL_ERROR) {{");
        if uses_accessors {
            let error = format!("cr_boxed_{}_error", child_slot.0);
            let _ = writeln!(
                output,
                "        const cr_error *{error} = {}_error({child_access});",
                callee.stem
            );
            let _ = writeln!(
                output,
                "        {error_access} = {error} != NULL ? *{error} : (cr_error){{CR_ERROR_MISSING_CHILD_ERROR, \"boxed child error without details\"}};"
            );
        } else {
            let _ = writeln!(output, "        {error_access} = {child_access}->error;");
        }
        let _ = writeln!(output, "        {}_destroy({child_access});", callee.stem);
        let _ = writeln!(output, "        {child_access} = NULL;");
        let _ = writeln!(output, "        {active_access} = false;");
        let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
        let _ = writeln!(output, "        {status_access} = CR_POLL_ERROR;");
        let _ = writeln!(output, "        return {status_access};");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    if ({status} == CR_POLL_CANCELED) {{");
        let _ = writeln!(output, "        {}_destroy({child_access});", callee.stem);
        let _ = writeln!(output, "        {child_access} = NULL;");
        let _ = writeln!(output, "        {active_access} = false;");
        let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
        let _ = writeln!(output, "        {status_access} = CR_POLL_CANCELED;");
        let _ = writeln!(output, "        return {status_access};");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    if ({status} == CR_POLL_YIELDED) {{");
        let return_type = function.coroutine.cfg.return_type.text.trim();
        if callee.result_type.trim() == return_type && return_type != "void" {
            if uses_accessors {
                let _ = writeln!(
                    output,
                    "        {yielded_access} = *{}_yielded({child_access});",
                    callee.stem
                );
            } else {
                let _ = writeln!(
                    output,
                    "        {yielded_access} = {child_access}->yielded;"
                );
            }
            let _ = writeln!(output, "        {status_access} = CR_POLL_YIELDED;");
            let _ = writeln!(output, "        return {status_access};");
        } else {
            let _ = writeln!(output, "        {}_destroy({child_access});", callee.stem);
            let _ = writeln!(output, "        {child_access} = NULL;");
            let _ = writeln!(output, "        {active_access} = false;");
            self.write_async_error(
                output,
                layout,
                context,
                1005,
                "yielded awaitable has an incompatible value type",
            );
        }
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    {}_destroy({child_access});", callee.stem);
        let _ = writeln!(output, "    {child_access} = NULL;");
        let _ = writeln!(output, "    {active_access} = false;");
        self.write_async_error_at(
            output,
            layout,
            context,
            1106,
            "invalid awaitable poll status",
            "    ",
        );
        true
    }

    #[allow(clippy::too_many_arguments)]
    fn write_typed_binding_suspend(
        &mut self,
        output: &mut String,
        function: &LivenessFunction,
        symbols: &FunctionSymbols<'_>,
        block: &BasicBlock,
        slot: AwaitSlotId,
        continuation: BlockId,
        context: &str,
        child: &CChildPlan,
    ) -> bool {
        let (CChildTarget::Static(callee), ChildOrigin::Binding(declaration)) =
            (&child.target, child.origin)
        else {
            return false;
        };
        if !is_typed_static_child(child) {
            return false;
        }
        let Some(field) = symbols.field(declaration).filter(|field| field.is_task) else {
            return false;
        };
        let layout = self.layouts.get(&function.coroutine.cfg.span.start_byte);
        let binding_access =
            required_access(layout, LogicalFieldId::Lifted(field.declaration), context);
        let binding_active = required_access(
            layout,
            LogicalFieldId::BindingActive(field.declaration),
            context,
        );
        let state_access = required_access(layout, LogicalFieldId::State, context);
        let error_access = required_access(layout, LogicalFieldId::Error, context);
        let cleanups_access = required_access(layout, LogicalFieldId::Cleanups, context);
        let status_access = required_access(layout, LogicalFieldId::Status, context);
        let yielded_access = required_access(layout, LogicalFieldId::Yielded, context);
        let result_access =
            layout.and_then(|layout| layout.access(LogicalFieldId::AwaitResult(slot), context));
        let state = function
            .coroutine
            .state_by_block
            .get(&block.id)
            .copied()
            .unwrap_or_default();
        let _ = writeln!(output, "    if (!{binding_active}) {{");
        self.write_async_error_at(
            output,
            layout,
            context,
            1109,
            "inactive task binding",
            "        ",
        );
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    {state_access} = {state}u;");
        let status = format!("cr_await_{}_status", slot.0);
        let child_pointer = match child.effective_storage {
            CChildStorage::Embedded(_) => format!("&{binding_access}"),
            CChildStorage::Boxed(_) => binding_access.clone(),
            CChildStorage::Opaque(_) => unreachable!("typed binding can't be opaque"),
        };
        let member = match child.effective_storage {
            CChildStorage::Embedded(_) => ".",
            CChildStorage::Boxed(_) => "->",
            CChildStorage::Opaque(_) => unreachable!("typed binding can't be opaque"),
        };
        let uses_accessors = matches!(child.effective_storage, CChildStorage::Boxed(_))
            && (callee.layout_visibility == crate::symbol_index::LayoutVisibility::Opaque
                || child.downgrade_reason.is_some());
        let _ = writeln!(
            output,
            "    cr_poll_status {status} = {}_poll({child_pointer}, poll_context);",
            callee.stem
        );
        let _ = writeln!(
            output,
            "    if ({status} == CR_POLL_PENDING) return {status};"
        );
        let _ = writeln!(output, "    if ({status} == CR_POLL_READY) {{");
        if callee.result_type.trim() != "void" {
            let result_access = result_access
                .as_deref()
                .expect("typed binding result has an await-result placement");
            if uses_accessors {
                let _ = writeln!(
                    output,
                    "        {result_access} = *{}_result({binding_access});",
                    callee.stem
                );
            } else {
                let _ = writeln!(
                    output,
                    "        {result_access} = {binding_access}{member}result;"
                );
            }
        }
        let _ = writeln!(output, "        goto cr_b{};", continuation.0);
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    if ({status} == CR_POLL_ERROR) {{");
        if uses_accessors {
            let error = format!("cr_binding_{}_error", declaration.0);
            let _ = writeln!(
                output,
                "        const cr_error *{error} = {}_error({binding_access});",
                callee.stem
            );
            let _ = writeln!(
                output,
                "        {error_access} = {error} != NULL ? *{error} : (cr_error){{CR_ERROR_MISSING_CHILD_ERROR, \"boxed binding error without details\"}};"
            );
        } else {
            let _ = writeln!(
                output,
                "        {error_access} = {binding_access}{member}error;"
            );
        }
        let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
        let _ = writeln!(output, "        {status_access} = CR_POLL_ERROR;");
        let _ = writeln!(output, "        return {status_access};");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    if ({status} == CR_POLL_CANCELED) {{");
        let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
        let _ = writeln!(output, "        {status_access} = CR_POLL_CANCELED;");
        let _ = writeln!(output, "        return {status_access};");
        let _ = writeln!(output, "    }}");
        let _ = writeln!(output, "    if ({status} == CR_POLL_YIELDED) {{");
        let return_type = function.coroutine.cfg.return_type.text.trim();
        if callee.result_type.trim() == return_type && return_type != "void" {
            if uses_accessors {
                let _ = writeln!(
                    output,
                    "        {yielded_access} = *{}_yielded({binding_access});",
                    callee.stem
                );
            } else {
                let _ = writeln!(
                    output,
                    "        {yielded_access} = {binding_access}{member}yielded;"
                );
            }
            let _ = writeln!(output, "        {status_access} = CR_POLL_YIELDED;");
            let _ = writeln!(output, "        return {status_access};");
        } else {
            self.write_async_error(
                output,
                layout,
                context,
                1005,
                "yielded awaitable has an incompatible value type",
            );
        }
        let _ = writeln!(output, "    }}");
        self.write_async_error_at(
            output,
            layout,
            context,
            1106,
            "invalid awaitable poll status",
            "    ",
        );
        true
    }

    fn write_terminator(
        &mut self,
        output: &mut String,
        environment: &FunctionEmission<'_>,
        block: &BasicBlock,
    ) {
        let function = environment.function;
        let symbols = environment.symbols;
        let await_types = environment.await_types;
        let layout = environment.layout;
        let async_context = environment.async_context;
        let state_access =
            async_context.map(|context| required_access(layout, LogicalFieldId::State, context));
        let error_access =
            async_context.map(|context| required_access(layout, LogicalFieldId::Error, context));
        let cleanups_access =
            async_context.map(|context| required_access(layout, LogicalFieldId::Cleanups, context));
        let status_access =
            async_context.map(|context| required_access(layout, LogicalFieldId::Status, context));
        let result_field_access =
            async_context.map(|context| required_access(layout, LogicalFieldId::Result, context));
        let yielded_field_access =
            async_context.map(|context| required_access(layout, LogicalFieldId::Yielded, context));
        match &block.terminator {
            CfgTerminator::Goto(edge) => {
                let _ = writeln!(output, "    goto cr_b{};", edge.target.0);
            }
            CfgTerminator::Branch {
                condition,
                consequence,
                alternative,
            } => {
                let condition = render_expression(
                    condition,
                    block,
                    &function.lifted_fields,
                    environment.layout,
                    async_context,
                    &self.config.prefix,
                    &self.static_stems,
                );
                let condition = parenthesized_condition(&condition);
                let _ = writeln!(
                    output,
                    "    if {condition} goto cr_b{}; else goto cr_b{};",
                    consequence.target.0, alternative.target.0
                );
            }
            CfgTerminator::Switch {
                expression,
                cases,
                default,
            } => {
                let expression = render_expression(
                    expression,
                    block,
                    &function.lifted_fields,
                    environment.layout,
                    async_context,
                    &self.config.prefix,
                    &self.static_stems,
                );
                let _ = writeln!(output, "    switch ({expression}) {{");
                for case in cases {
                    let value = render_expression(
                        &case.value,
                        block,
                        &function.lifted_fields,
                        environment.layout,
                        async_context,
                        &self.config.prefix,
                        &self.static_stems,
                    );
                    let _ = writeln!(output, "    case {value}: goto cr_b{};", case.edge.target.0);
                }
                let _ = writeln!(output, "    default: goto cr_b{};", default.target.0);
                let _ = writeln!(output, "    }}");
            }
            CfgTerminator::Suspend {
                edge,
                operand,
                slot,
                continuation,
                span,
            } => {
                let Some(context) = async_context else {
                    self.invalid_async_terminator(span, "await");
                    return;
                };
                let planned_child = self
                    .function_plans
                    .get(&function.coroutine.cfg.span.start_byte)
                    .and_then(|plan| {
                        plan.edge_children
                            .get(edge)
                            .map(|instance| (plan, instance))
                    })
                    .and_then(|(plan, instance)| plan.children.get(instance))
                    .cloned();
                if let Some(child) = planned_child.as_ref()
                    && self.write_embedded_direct_suspend(
                        output,
                        function,
                        block,
                        operand,
                        *slot,
                        continuation.target,
                        context,
                        child,
                    )
                {
                    return;
                }
                if let Some(child) = planned_child.as_ref()
                    && self.write_boxed_direct_suspend(
                        output,
                        function,
                        block,
                        operand,
                        *slot,
                        continuation.target,
                        context,
                        child,
                    )
                {
                    return;
                }
                if let Some(child) = planned_child.as_ref()
                    && self.write_typed_binding_suspend(
                        output,
                        function,
                        symbols,
                        block,
                        *slot,
                        continuation.target,
                        context,
                        child,
                    )
                {
                    return;
                }
                if let Some(child) = planned_child.as_ref()
                    && self.reject_static_dynamic_fallback(child, span, "await child")
                {
                    return;
                }
                let binding_field = match &operand.kind {
                    HirExprKind::TaskRef { declaration, .. } => symbols.field(*declaration),
                    _ => None,
                };
                let is_binding = binding_field.is_some_and(|field| field.is_task);
                let awaitable_access =
                    required_access(layout, LogicalFieldId::Awaitable(*slot), context);
                let awaitable_active =
                    required_access(layout, LogicalFieldId::AwaitableActive(*slot), context);
                let result_access = layout
                    .and_then(|layout| layout.access(LogicalFieldId::AwaitResult(*slot), context));
                let operand = render_expression(
                    operand,
                    block,
                    &function.lifted_fields,
                    environment.layout,
                    async_context,
                    &self.config.prefix,
                    &self.static_stems,
                );
                let state = function
                    .coroutine
                    .state_by_block
                    .get(&block.id)
                    .copied()
                    .unwrap_or_default();
                let result_pointer = if await_types.contains_key(slot) {
                    format!(
                        "&{}",
                        result_access
                            .as_deref()
                            .expect("typed await has a result placement")
                    )
                } else {
                    "NULL".to_owned()
                };
                let status = format!("cr_await_{}_status", slot.0);
                let _ = writeln!(output, "    if (!{awaitable_active}) {{");
                if let Some(field) = binding_field.filter(|field| field.is_task) {
                    let binding_active = required_access(
                        layout,
                        LogicalFieldId::BindingActive(field.declaration),
                        context,
                    );
                    let _ = writeln!(output, "        if (!{binding_active}) {{");
                    self.write_async_error_at(
                        output,
                        layout,
                        context,
                        1109,
                        "inactive task binding",
                        "            ",
                    );
                    let _ = writeln!(output, "        }}");
                }
                let _ = writeln!(output, "        {awaitable_access} = {operand};");
                self.write_awaitable_activation_validation(
                    output,
                    layout,
                    context,
                    &awaitable_access,
                    await_types.get(slot).map(String::as_str),
                    "        ",
                    !is_binding,
                );
                let _ = writeln!(output, "        {awaitable_active} = true;");
                let _ = writeln!(output, "    }}");
                let state_access = state_access.as_deref().expect("async state placement");
                let error_access = error_access.as_deref().expect("async error placement");
                let cleanups_access = cleanups_access.as_deref().expect("async cleanup placement");
                let status_access = status_access.as_deref().expect("async status placement");
                let yielded_field_access = yielded_field_access
                    .as_deref()
                    .expect("async yielded placement");
                let _ = writeln!(output, "    {state_access} = {state}u;");
                let _ = writeln!(
                    output,
                    "    if (({awaitable_access}.vtable->required_context_capabilities & ~CR_POLL_KNOWN_CAPABILITIES) != 0u) {{"
                );
                self.write_await_release(output, layout, context, *slot, "        ", !is_binding);
                self.write_async_error(
                    output,
                    layout,
                    context,
                    1107,
                    "unsupported poll capability",
                );
                let _ = writeln!(output, "    }}");
                let _ = writeln!(
                    output,
                    "    if (({awaitable_access}.vtable->required_context_capabilities & ~(poll_context != NULL ? poll_context->available_capabilities : 0u)) != 0u) {{"
                );
                self.write_await_release(output, layout, context, *slot, "        ", !is_binding);
                self.write_async_error(output, layout, context, 1104, "missing poll capability");
                let _ = writeln!(output, "    }}");
                let _ = writeln!(
                    output,
                    "    cr_poll_status {status} = {awaitable_access}.vtable->poll({awaitable_access}.state, poll_context, {result_pointer});"
                );
                let _ = writeln!(
                    output,
                    "    if ({status} == CR_POLL_PENDING) return {status};"
                );
                let _ = writeln!(output, "    if ({status} == CR_POLL_READY) {{");
                self.write_await_release(output, layout, context, *slot, "        ", !is_binding);
                let _ = writeln!(output, "        goto cr_b{};", continuation.target.0);
                let _ = writeln!(output, "    }}");
                let _ = writeln!(output, "    if ({status} == CR_POLL_ERROR) {{");
                let _ = writeln!(
                    output,
                    "        const cr_error *cr_await_{}_error = {awaitable_access}.vtable->error != NULL ? {awaitable_access}.vtable->error({awaitable_access}.state) : NULL;",
                    slot.0
                );
                let _ = writeln!(
                    output,
                    "        {error_access} = cr_await_{}_error != NULL ? *cr_await_{}_error : (cr_error){{CR_ERROR_MISSING_CHILD_ERROR, \"awaitable error without details\"}};",
                    slot.0, slot.0
                );
                self.write_await_release(output, layout, context, *slot, "        ", !is_binding);
                let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
                let _ = writeln!(output, "        {status_access} = CR_POLL_ERROR;");
                let _ = writeln!(output, "        return {status_access};");
                let _ = writeln!(output, "    }}");
                let _ = writeln!(output, "    if ({status} == CR_POLL_CANCELED) {{");
                self.write_await_release(output, layout, context, *slot, "        ", !is_binding);
                let _ = writeln!(output, "        cr_cleanup_run_all(&{cleanups_access});");
                let _ = writeln!(output, "        {status_access} = CR_POLL_CANCELED;");
                let _ = writeln!(output, "        return {status_access};");
                let _ = writeln!(output, "    }}");
                let _ = writeln!(output, "    if ({status} == CR_POLL_YIELDED) {{");
                let await_type = await_types.get(slot).map(|value| value.trim());
                let return_type = function.coroutine.cfg.return_type.text.trim();
                if await_type.is_some_and(|value| value == return_type) && return_type != "void" {
                    let result_access = result_access
                        .as_deref()
                        .expect("yield-compatible await has a result placement");
                    let _ = writeln!(output, "        {yielded_field_access} = {result_access};");
                    let _ = writeln!(output, "        {status_access} = CR_POLL_YIELDED;");
                    let _ = writeln!(output, "        return {status_access};");
                } else {
                    self.write_await_release(
                        output,
                        layout,
                        context,
                        *slot,
                        "        ",
                        !is_binding,
                    );
                    self.write_async_error(
                        output,
                        layout,
                        context,
                        1005,
                        "yielded awaitable has an incompatible value type",
                    );
                }
                let _ = writeln!(output, "    }}");
                self.write_await_release(output, layout, context, *slot, "    ", !is_binding);
                self.write_async_error_at(
                    output,
                    layout,
                    context,
                    1106,
                    "invalid awaitable poll status",
                    "    ",
                );
            }
            CfgTerminator::Yield {
                value,
                continuation,
                span,
            } => {
                let Some(_context) = async_context else {
                    self.invalid_async_terminator(span, "yield");
                    return;
                };
                if let Some(value) = value {
                    let value = render_value(
                        value,
                        block,
                        &function.lifted_fields,
                        environment.layout,
                        async_context,
                        &self.config.prefix,
                        &self.static_stems,
                    );
                    let yielded_access = yielded_field_access
                        .as_deref()
                        .expect("async yielded placement");
                    let _ = writeln!(output, "    {yielded_access} = {value};");
                }
                let state = function
                    .coroutine
                    .state_by_block
                    .get(&continuation.target)
                    .copied()
                    .unwrap_or_default();
                let state_access = state_access.as_deref().expect("async state placement");
                let status_access = status_access.as_deref().expect("async status placement");
                let _ = writeln!(output, "    {state_access} = {state}u;");
                let _ = writeln!(output, "    {status_access} = CR_POLL_YIELDED;");
                let _ = writeln!(output, "    return {status_access};");
            }
            CfgTerminator::Return { value, span: _ } => {
                if async_context.is_some() {
                    let state_access = state_access.as_deref().expect("async state placement");
                    let cleanups_access =
                        cleanups_access.as_deref().expect("async cleanup placement");
                    let status_access = status_access.as_deref().expect("async status placement");
                    if let Some(value) = value {
                        let value = render_value(
                            value,
                            block,
                            &function.lifted_fields,
                            environment.layout,
                            async_context,
                            &self.config.prefix,
                            &self.static_stems,
                        );
                        let result_access = result_field_access
                            .as_deref()
                            .expect("async result placement");
                        let _ = writeln!(output, "    {result_access} = {value};");
                    }
                    let _ = writeln!(output, "    cr_cleanup_stack_destroy(&{cleanups_access});");
                    let _ = writeln!(output, "    {status_access} = CR_POLL_READY;");
                    let _ = writeln!(output, "    {state_access} = UINT32_MAX;");
                    let _ = writeln!(output, "    return {status_access};");
                } else {
                    let _ = writeln!(output, "    cr_cleanup_stack_destroy(&cr_cleanups);");
                    if let Some(value) = value {
                        let value = render_value(
                            value,
                            block,
                            &[],
                            None,
                            None,
                            &self.config.prefix,
                            &self.static_stems,
                        );
                        let _ = writeln!(output, "    return {value};");
                    } else {
                        let _ = writeln!(output, "    return;");
                    }
                }
            }
            CfgTerminator::Open => {
                self.diagnostics.push(Diagnostic {
                    code: "CRC5003",
                    severity: DiagnosticSeverity::Error,
                    message: "open CFG block reached C emission".to_owned(),
                    primary_span: function.coroutine.cfg.span.clone(),
                    related: Vec::new(),
                });
            }
            CfgTerminator::Unreachable => {
                let _ = writeln!(output, "    abort();");
            }
        }
        let _ = symbols;
    }

    fn cleanup_helpers(
        &mut self,
        function: &LivenessFunction,
        symbols: &FunctionSymbols,
    ) -> HashMap<CleanupId, CleanupHelper> {
        let mut helpers = HashMap::new();
        let stem = function
            .coroutine
            .poll_name
            .strip_suffix("_poll")
            .unwrap_or(&function.coroutine.poll_name);
        for block in &function.coroutine.cfg.blocks {
            for instruction in &block.instructions {
                let CfgInstruction::PushCleanup(registration) = instruction else {
                    continue;
                };
                let mut argument_types = Vec::new();
                for argument in &registration.arguments {
                    match infer_expression_type(argument, block, symbols) {
                        Some(ty) => argument_types.push(ty),
                        None => self.diagnostics.push(Diagnostic {
                            code: "CRC5004",
                            severity: DiagnosticSeverity::Error,
                            message: "unable to resolve portable defer argument type".to_owned(),
                            primary_span: argument.span.clone(),
                            related: Vec::new(),
                        }),
                    }
                }
                helpers.insert(
                    registration.id,
                    CleanupHelper {
                        registration: registration.clone(),
                        payload_type: format!("{stem}_cleanup_{}_payload", registration.id.0),
                        run_function: format!("{stem}_cleanup_{}_run", registration.id.0),
                        argument_types,
                    },
                );
            }
        }
        helpers
    }

    fn write_cleanup_helpers(
        &self,
        output: &mut String,
        helpers: &HashMap<CleanupId, CleanupHelper>,
    ) {
        let mut helpers: Vec<_> = helpers.values().collect();
        helpers.sort_by_key(|helper| helper.registration.id);
        for helper in helpers {
            let _ = writeln!(output, "typedef struct {} {{", helper.payload_type);
            if helper.argument_types.is_empty() {
                let _ = writeln!(output, "    unsigned char unused;");
            } else {
                for (index, ty) in helper.argument_types.iter().enumerate() {
                    let _ = writeln!(
                        output,
                        "    {};",
                        render_typed_name(ty, &format!("arg_{index}"))
                    );
                }
            }
            let _ = writeln!(output, "}} {};", helper.payload_type);
            let _ = writeln!(output, "static void {}(void *raw) {{", helper.run_function);
            let _ = writeln!(
                output,
                "    {} *payload = ({} *)raw;",
                helper.payload_type, helper.payload_type
            );
            let arguments = (0..helper.argument_types.len())
                .map(|index| format!("payload->arg_{index}"))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = writeln!(
                output,
                "    {}({});",
                helper.registration.function.text, arguments
            );
            let _ = writeln!(output, "}}\n");
        }
    }

    fn write_async_error(
        &self,
        output: &mut String,
        layout: Option<&FunctionContextLayout>,
        context: &str,
        code: i32,
        message: &str,
    ) {
        self.write_async_error_at(output, layout, context, code, message, "        ");
    }

    fn write_async_error_at(
        &self,
        output: &mut String,
        layout: Option<&FunctionContextLayout>,
        context: &str,
        code: i32,
        message: &str,
        indent: &str,
    ) {
        let error = required_access(layout, LogicalFieldId::Error, context);
        let cleanups = required_access(layout, LogicalFieldId::Cleanups, context);
        let status = required_access(layout, LogicalFieldId::Status, context);
        let _ = writeln!(
            output,
            "{indent}{error} = (cr_error){{{code}, \"{message}\"}};"
        );
        let _ = writeln!(output, "{indent}cr_cleanup_run_all(&{cleanups});");
        let _ = writeln!(output, "{indent}{status} = CR_POLL_ERROR;");
        let _ = writeln!(output, "{indent}return {status};");
    }

    #[allow(clippy::too_many_arguments)]
    fn write_awaitable_activation_validation(
        &self,
        output: &mut String,
        layout: Option<&FunctionContextLayout>,
        context: &str,
        awaitable: &str,
        expected_type: Option<&str>,
        indent: &str,
        drop_on_failure: bool,
    ) {
        let body_indent = format!("{indent}    ");
        let _ = writeln!(
            output,
            "{indent}if ({awaitable}.vtable == NULL || {awaitable}.vtable->abi_version < CR_AWAITABLE_VTABLE_ABI_VERSION || {awaitable}.vtable->struct_size < CR_AWAITABLE_VTABLE_DROP_PREFIX_SIZE) {{"
        );
        self.write_async_error_at(
            output,
            layout,
            context,
            1102,
            "invalid awaitable ABI",
            &body_indent,
        );
        let _ = writeln!(output, "{indent}}}");
        let _ = writeln!(
            output,
            "{indent}if ({awaitable}.vtable->struct_size < CR_AWAITABLE_VTABLE_V1_MIN_SIZE) {{"
        );
        self.write_awaitable_candidate_drop(output, awaitable, &body_indent, drop_on_failure);
        self.write_async_error_at(
            output,
            layout,
            context,
            1102,
            "invalid awaitable ABI",
            &body_indent,
        );
        let _ = writeln!(output, "{indent}}}");
        let _ = writeln!(
            output,
            "{indent}if ({awaitable}.vtable->poll == NULL || {awaitable}.vtable->drop == NULL) {{"
        );
        self.write_awaitable_candidate_drop(output, awaitable, &body_indent, drop_on_failure);
        self.write_async_error_at(
            output,
            layout,
            context,
            1103,
            "awaitable callback missing",
            &body_indent,
        );
        let _ = writeln!(output, "{indent}}}");
        if let Some(expected_type) = expected_type.filter(|ty| ty.trim() != "void") {
            let expected_type = expected_type.trim();
            let _ = writeln!(
                output,
                "{indent}if ({awaitable}.vtable->value_size != sizeof({expected_type}) || {awaitable}.vtable->value_align != _Alignof({expected_type})) {{"
            );
            self.write_awaitable_candidate_drop(output, awaitable, &body_indent, drop_on_failure);
            self.write_async_error_at(
                output,
                layout,
                context,
                1105,
                "awaitable value layout mismatch",
                &body_indent,
            );
            let _ = writeln!(output, "{indent}}}");
        } else {
            let _ = writeln!(
                output,
                "{indent}if ({awaitable}.vtable->value_size != 0u || {awaitable}.vtable->value_align != 0u) {{"
            );
            self.write_awaitable_candidate_drop(output, awaitable, &body_indent, drop_on_failure);
            self.write_async_error_at(
                output,
                layout,
                context,
                1105,
                "void awaitable exposes a value layout",
                &body_indent,
            );
            let _ = writeln!(output, "{indent}}}");
        }
    }

    fn write_awaitable_candidate_drop(
        &self,
        output: &mut String,
        awaitable: &str,
        indent: &str,
        drop_on_failure: bool,
    ) {
        if drop_on_failure {
            let _ = writeln!(
                output,
                "{indent}if ({awaitable}.vtable->drop != NULL) {awaitable}.vtable->drop({awaitable}.state);"
            );
        }
    }

    fn write_await_release(
        &self,
        output: &mut String,
        layout: Option<&FunctionContextLayout>,
        context: &str,
        slot: AwaitSlotId,
        indent: &str,
        drop: bool,
    ) {
        let awaitable = required_access(layout, LogicalFieldId::Awaitable(slot), context);
        let active = required_access(layout, LogicalFieldId::AwaitableActive(slot), context);
        if drop {
            let _ = writeln!(
                output,
                "{indent}if ({awaitable}.vtable != NULL && {awaitable}.vtable->drop != NULL) {awaitable}.vtable->drop({awaitable}.state);"
            );
        }
        let _ = writeln!(output, "{indent}{active} = false;");
    }

    fn write_invalid_state(
        &self,
        output: &mut String,
        layout: Option<&FunctionContextLayout>,
        context: &str,
    ) {
        let error = required_access(layout, LogicalFieldId::Error, context);
        let status = required_access(layout, LogicalFieldId::Status, context);
        let _ = writeln!(
            output,
            "        {error} = (cr_error){{1001, \"invalid coroutine state\"}};"
        );
        let _ = writeln!(output, "        {status} = CR_POLL_ERROR;");
        let _ = writeln!(output, "        return {status};");
    }

    fn invalid_async_terminator(&mut self, span: &SourceSpan, name: &str) {
        self.diagnostics.push(Diagnostic {
            code: "CRC5005",
            severity: DiagnosticSeverity::Error,
            message: format!("{name} terminator reached synchronous C emission"),
            primary_span: span.clone(),
            related: Vec::new(),
        });
    }

    fn reject_static_dynamic_fallback(
        &mut self,
        child: &CChildPlan,
        span: &SourceSpan,
        site: &str,
    ) -> bool {
        if !matches!(child.target, CChildTarget::Static(_)) {
            return false;
        }
        self.diagnostics.push(Diagnostic {
            code: "CRC6009",
            severity: DiagnosticSeverity::Error,
            message: format!("planned static {site} reached dynamic C emission"),
            primary_span: span.clone(),
            related: Vec::new(),
        });
        true
    }
}

struct CleanupHelper {
    registration: CleanupRegistration,
    payload_type: String,
    run_function: String,
    argument_types: Vec<String>,
}

struct FunctionEmission<'a> {
    function: &'a LivenessFunction,
    symbols: &'a FunctionSymbols<'a>,
    helpers: &'a HashMap<CleanupId, CleanupHelper>,
    await_types: &'a BTreeMap<AwaitSlotId, String>,
    layout: Option<&'a FunctionContextLayout>,
    async_context: Option<&'a str>,
}

fn is_same_unit_typed_child(child: &CChildPlan) -> bool {
    let CChildTarget::Static(callee) = &child.target else {
        return false;
    };
    match child.effective_storage {
        CChildStorage::Embedded(_) => true,
        CChildStorage::Boxed(_) => {
            callee.layout_visibility == crate::symbol_index::LayoutVisibility::Visible
        }
        CChildStorage::Opaque(_) => false,
    }
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

struct DeclarationSymbol {
    id: DeclarationId,
    name: String,
    ty: String,
    scope: ScopeId,
    start_byte: usize,
}

struct FunctionSymbols<'function> {
    declarations: Vec<DeclarationSymbol>,
    fields: HashMap<DeclarationId, &'function LiftedField>,
}

impl<'function> FunctionSymbols<'function> {
    fn new(function: &'function LivenessFunction) -> Self {
        let mut declarations: Vec<_> = function
            .coroutine
            .cfg
            .parameters
            .iter()
            .map(|parameter| DeclarationSymbol {
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
                    declarations.push(DeclarationSymbol {
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
        let fields = function
            .lifted_fields
            .iter()
            .map(|field| (field.declaration, field))
            .collect();
        Self {
            declarations,
            fields,
        }
    }

    fn field(&self, id: DeclarationId) -> Option<&LiftedField> {
        self.fields.get(&id).copied()
    }

    fn name(&self, id: DeclarationId) -> &str {
        self.declarations
            .iter()
            .find(|declaration| declaration.id == id)
            .map(|declaration| declaration.name.as_str())
            .unwrap_or("cr_missing_value")
    }

    fn resolve(&self, name: &str, byte: usize, scopes: &[ScopeId]) -> Option<&DeclarationSymbol> {
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

fn infer_expression_type(
    expression: &HirExpr,
    block: &BasicBlock,
    symbols: &FunctionSymbols<'_>,
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

fn render_expression(
    expression: &HirExpr,
    block: &BasicBlock,
    fields: &[LiftedField],
    layout: Option<&FunctionContextLayout>,
    async_context: Option<&str>,
    prefix: &str,
    static_stems: &BTreeMap<FunctionId, String>,
) -> String {
    match &expression.kind {
        HirExprKind::Source(source) => rewrite_source(
            source,
            expression.span.start_byte,
            &block.scope_stack,
            fields,
            layout,
            async_context,
        ),
        HirExprKind::AwaitResultRef(slot) => layout
            .and_then(|layout| {
                async_context
                    .and_then(|context| layout.access(LogicalFieldId::AwaitResult(*slot), context))
            })
            .expect("await result reference has a validated access path"),
        HirExprKind::Composite { .. } => render_composite_expression(
            expression,
            block,
            fields,
            layout,
            async_context,
            prefix,
            static_stems,
        ),
        HirExprKind::TaskRef { declaration, name } => fields
            .iter()
            .find(|field| field.declaration == *declaration)
            .and_then(|field| {
                layout.and_then(|layout| {
                    async_context.and_then(|context| {
                        layout.access(LogicalFieldId::Lifted(field.declaration), context)
                    })
                })
            })
            .unwrap_or_else(|| name.clone()),
        HirExprKind::Call {
            function,
            arguments,
        } => {
            let function = render_expression(
                function,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems,
            );
            let arguments = arguments
                .iter()
                .map(|argument| {
                    render_expression(
                        argument,
                        block,
                        fields,
                        layout,
                        async_context,
                        prefix,
                        static_stems,
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("{function}({arguments})")
        }
        HirExprKind::AsyncCall {
            target,
            callee,
            arguments,
            ..
        } => {
            let stem = target
                .and_then(|target| static_stems.get(&target))
                .cloned()
                .unwrap_or_else(|| format!("{}{}", c_identifier(prefix), c_identifier(callee)));
            let arguments = arguments
                .iter()
                .map(|argument| {
                    render_expression(
                        argument,
                        block,
                        fields,
                        layout,
                        async_context,
                        prefix,
                        static_stems,
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let create_arguments = if arguments.is_empty() {
                "NULL".to_owned()
            } else {
                format!("{arguments}, NULL")
            };
            format!("{stem}_into_awaitable({stem}_create({create_arguments}))")
        }
        HirExprKind::Binary {
            left,
            operator,
            right,
        } => format!(
            "({} {operator} {})",
            render_expression(
                left,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            ),
            render_expression(
                right,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            )
        ),
        HirExprKind::Conditional {
            condition,
            consequence,
            alternative,
        } => format!(
            "({} ? {} : {})",
            render_expression(
                condition,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            ),
            render_expression(
                consequence,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            ),
            render_expression(
                alternative,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            )
        ),
        HirExprKind::Comma { left, right } => format!(
            "({}, {})",
            render_expression(
                left,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            ),
            render_expression(
                right,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            )
        ),
        HirExprKind::Assignment {
            left,
            operator,
            right,
        } => format!(
            "({} {operator} {})",
            render_expression(
                left,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            ),
            render_expression(
                right,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            )
        ),
        HirExprKind::Unary { operator, operand } => format!(
            "{operator}({})",
            render_expression(
                operand,
                block,
                fields,
                layout,
                async_context,
                prefix,
                static_stems
            )
        ),
        HirExprKind::Await(operand) => render_expression(
            operand,
            block,
            fields,
            layout,
            async_context,
            prefix,
            static_stems,
        ),
        HirExprKind::Yield(value) => value
            .as_deref()
            .map(|value| {
                render_expression(
                    value,
                    block,
                    fields,
                    layout,
                    async_context,
                    prefix,
                    static_stems,
                )
            })
            .unwrap_or_default(),
    }
}

fn required_access(
    layout: Option<&FunctionContextLayout>,
    field: LogicalFieldId,
    context: &str,
) -> String {
    layout
        .and_then(|layout| layout.access(field, context))
        .expect("logical context field has a validated access path")
}

fn render_composite_expression(
    expression: &HirExpr,
    block: &BasicBlock,
    fields: &[LiftedField],
    layout: Option<&FunctionContextLayout>,
    async_context: Option<&str>,
    prefix: &str,
    static_stems: &BTreeMap<FunctionId, String>,
) -> String {
    let HirExprKind::Composite { source, extensions } = &expression.kind else {
        unreachable!("composite renderer requires a composite expression");
    };
    let mut extensions: Vec<_> = extensions.iter().collect();
    extensions.sort_by_key(|extension| extension.span.start_byte);
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    for extension in extensions {
        let start = extension
            .span
            .start_byte
            .saturating_sub(expression.span.start_byte);
        let end = extension
            .span
            .end_byte
            .saturating_sub(expression.span.start_byte);
        assert!(cursor <= start && start <= end && end <= source.len());
        output.push_str(&rewrite_source(
            &source[cursor..start],
            expression.span.start_byte + cursor,
            &block.scope_stack,
            fields,
            layout,
            async_context,
        ));
        output.push_str(&render_expression(
            extension,
            block,
            fields,
            layout,
            async_context,
            prefix,
            static_stems,
        ));
        cursor = end;
    }
    output.push_str(&rewrite_source(
        &source[cursor..],
        expression.span.start_byte + cursor,
        &block.scope_stack,
        fields,
        layout,
        async_context,
    ));
    output
}

fn render_value(
    value: &CfgValue,
    block: &BasicBlock,
    fields: &[LiftedField],
    layout: Option<&FunctionContextLayout>,
    async_context: Option<&str>,
    prefix: &str,
    static_stems: &BTreeMap<FunctionId, String>,
) -> String {
    match value {
        CfgValue::Expression(expression) => render_expression(
            expression,
            block,
            fields,
            layout,
            async_context,
            prefix,
            static_stems,
        ),
        CfgValue::AwaitResult(slot) => layout
            .and_then(|layout| {
                async_context
                    .and_then(|context| layout.access(LogicalFieldId::AwaitResult(*slot), context))
            })
            .expect("await result value has a validated access path"),
    }
}

fn rewrite_source(
    source: &str,
    absolute_start: usize,
    scopes: &[ScopeId],
    fields: &[LiftedField],
    layout: Option<&FunctionContextLayout>,
    async_context: Option<&str>,
) -> String {
    let Some(context) = async_context else {
        return source.to_owned();
    };
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' | b'\'' => {
                let start = index;
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
                output.push_str(&source[start..index]);
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                output.push_str(&source[index..]);
                break;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                let start = index;
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                output.push_str(&source[start..index]);
            }
            byte if byte == b'_' || byte.is_ascii_alphabetic() => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index] == b'_' || bytes[index].is_ascii_alphanumeric())
                {
                    index += 1;
                }
                let name = &source[start..index];
                let field = fields
                    .iter()
                    .filter(|field| {
                        field.source_name == name
                            && field.declaration_start <= absolute_start + start
                            && scopes.contains(&field.scope)
                    })
                    .max_by_key(|field| {
                        (
                            scopes
                                .iter()
                                .position(|scope| *scope == field.scope)
                                .unwrap_or_default(),
                            field.declaration_start,
                        )
                    });
                if let Some(field) = field {
                    let access = layout
                        .and_then(|layout| {
                            layout.access(LogicalFieldId::Lifted(field.declaration), context)
                        })
                        .expect("lifted field has a validated access path");
                    output.push_str(&access);
                } else {
                    output.push_str(name);
                }
            }
            _ => {
                output.push(bytes[index] as char);
                index += 1;
            }
        }
    }
    output
}

fn parameter_list(function: &CoroutineFunction, names: bool) -> String {
    if function.cfg.parameters.is_empty() {
        return "void".to_owned();
    }
    function
        .cfg
        .parameters
        .iter()
        .map(|parameter| {
            if names {
                render_typed_name(parameter.ty.text.trim(), &parameter.name)
            } else {
                parameter.ty.text.trim().to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn typed_parameter_declarations(callee: &StaticCallee) -> String {
    callee
        .parameters
        .iter()
        .enumerate()
        .map(|(index, parameter)| {
            render_typed_name(&parameter.adjusted_type, &format!("cr_arg_{index}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn create_parameter_list(function: &CoroutineFunction) -> String {
    if function.cfg.parameters.is_empty() {
        "cr_error *out_error".to_owned()
    } else {
        format!("{}, cr_error *out_error", parameter_list(function, true))
    }
}

fn write_runtime_abi_guard(output: &mut String) {
    let _ = writeln!(output, "#if CR_RUNTIME_ABI_VERSION != 3u");
    let _ = writeln!(
        output,
        "#error \"generated CR code requires runtime ABI version 3\""
    );
    let _ = writeln!(output, "#endif\n");
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

fn parenthesized_condition(condition: &str) -> String {
    let condition = condition.trim();
    if condition.starts_with('(') && condition.ends_with(')') {
        condition.to_owned()
    } else {
        format!("({condition})")
    }
}

fn reachable_blocks(function: &crate::control_flow::CfgFunction) -> HashSet<BlockId> {
    let mut reachable = HashSet::new();
    let mut pending = vec![function.entry];
    while let Some(block_id) = pending.pop() {
        if !reachable.insert(block_id) {
            continue;
        }
        let block = &function.blocks[block_id.0 as usize];
        match &block.terminator {
            CfgTerminator::Goto(edge) => pending.push(edge.target),
            CfgTerminator::Branch {
                consequence,
                alternative,
                ..
            } => {
                pending.push(consequence.target);
                pending.push(alternative.target);
            }
            CfgTerminator::Switch { cases, default, .. } => {
                pending.extend(cases.iter().map(|case| case.edge.target));
                pending.push(default.target);
            }
            CfgTerminator::Suspend { continuation, .. }
            | CfgTerminator::Yield { continuation, .. } => {
                pending.push(continuation.target);
            }
            CfgTerminator::Open | CfgTerminator::Return { .. } | CfgTerminator::Unreachable => {}
        }
    }
    reachable
}

fn reachable_task_bindings(function: &LivenessFunction) -> HashSet<DeclarationId> {
    let reachable = reachable_blocks(&function.coroutine.cfg);
    function
        .coroutine
        .cfg
        .blocks
        .iter()
        .filter(|block| reachable.contains(&block.id))
        .flat_map(|block| block.instructions.iter())
        .filter_map(|instruction| match instruction {
            CfgInstruction::Declaration(declaration) if declaration.is_task => Some(declaration.id),
            _ => None,
        })
        .collect()
}

fn parameter_list_with_leading_comma(function: &CoroutineFunction) -> String {
    if function.cfg.parameters.is_empty() {
        String::new()
    } else {
        format!(", {}", parameter_list(function, true))
    }
}

fn parameter_names(function: &CoroutineFunction) -> String {
    function
        .cfg
        .parameters
        .iter()
        .map(|parameter| parameter.name.clone())
        .collect::<Vec<_>>()
        .join(", ")
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

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use crate::context_layout::build_identity_context_layout;
    use crate::control_flow::build_cfg;
    use crate::coroutine::lower_coroutines;
    use crate::liveness::analyze_liveness;
    use crate::runtime_abi::runtime_header;
    use crate::scope_exit::lower_scope_exits;
    use crate::semantic::build_hir;
    use crate::slot_liveness::analyze_slot_liveness;
    use crate::syntax::SyntaxParser;

    use super::*;

    fn emit(source: &str) -> CEmission {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("emit.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);
        let cfg = build_cfg(&hir);
        let scoped = lower_scope_exits(&cfg);
        let coroutines = lower_coroutines(&scoped, "cr_");
        let slot_liveness = analyze_slot_liveness(&coroutines);
        let liveness = analyze_liveness(&coroutines);
        let layout = build_identity_context_layout(&liveness, None, &slot_liveness);
        emit_translation_unit(
            source,
            &liveness,
            None,
            &slot_liveness,
            &layout,
            &CEmitterConfig::default(),
        )
    }

    fn available_compiler() -> Option<&'static str> {
        ["clang", "gcc"].into_iter().find(|compiler| {
            Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
    }

    fn syntax_check(code: &str) {
        syntax_check_with_standard(code, "c11");
    }

    fn syntax_check_with_standard(code: &str, standard: &str) {
        let compiler = available_compiler().expect("Clang or GCC is required for this test");
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("cr_runtime.h"), runtime_header()).expect("runtime header");
        fs::write(directory.path().join("generated.c"), code).expect("generated source");
        let output = Command::new(compiler)
            .arg(format!("-std={standard}"))
            .arg("-Wall")
            .arg("-Wextra")
            .arg("-Werror")
            .arg("-fsyntax-only")
            .arg("generated.c")
            .current_dir(directory.path())
            .output()
            .expect("native compiler runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn compile_and_run(code: &str) {
        let compiler = available_compiler().expect("Clang or GCC is required for this test");
        let directory = tempfile::tempdir().expect("temporary directory");
        fs::write(directory.path().join("cr_runtime.h"), runtime_header()).expect("runtime header");
        fs::write(directory.path().join("generated.c"), code).expect("generated source");
        let executable_name = if cfg!(windows) {
            "generated.exe"
        } else {
            "generated"
        };
        let compilation = Command::new(compiler)
            .arg("-std=c11")
            .arg("-Wall")
            .arg("-Wextra")
            .arg("-Werror")
            .arg("generated.c")
            .arg("-o")
            .arg(executable_name)
            .current_dir(directory.path())
            .output()
            .expect("native compiler runs");
        assert!(
            compilation.status.success(),
            "{}",
            String::from_utf8_lossy(&compilation.stderr)
        );
        let execution = Command::new(directory.path().join(executable_name))
            .current_dir(directory.path())
            .output()
            .expect("generated executable runs");
        assert!(
            execution.status.success(),
            "stdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&execution.stdout),
            String::from_utf8_lossy(&execution.stderr)
        );
    }

    #[test]
    fn emits_portable_async_task_and_lifted_context() {
        let emission = emit(
            r#"
cr_awaitable next_value(int input);

__async int run(int input) {
    int value = __await next_value(input);
    return value;
}
"#,
        );

        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );
        assert!(emission.source.contains("typedef struct cr_run_task"));
        assert!(emission.source.contains("switch (ctx->state)"));
        assert!(emission.source.contains("cr_run_poll"));
        assert!(emission.source.contains("cr_v_"));
        syntax_check(&emission.source);
    }

    #[test]
    fn emits_sync_defer_with_dynamic_cleanup_stack() {
        let emission = emit(
            r#"
void close_handle(int handle);

int guarded(int handle) {
    __defer close_handle(handle);
    return handle;
}
"#,
        );

        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );
        assert!(emission.source.contains("cr_cleanup_push"));
        assert!(emission.source.contains("close_handle(payload->arg_0)"));
        syntax_check(&emission.source);
    }

    #[test]
    fn emits_gnu_computed_goto_dispatch() {
        let source = r#"
__async int run(void) {
    __yield 1;
    return 2;
}
"#;
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("computed.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);
        let cfg = build_cfg(&hir);
        let scoped = lower_scope_exits(&cfg);
        let coroutines = lower_coroutines(&scoped, "cr_");
        let slot_liveness = analyze_slot_liveness(&coroutines);
        let liveness = analyze_liveness(&coroutines);
        let layout = build_identity_context_layout(&liveness, None, &slot_liveness);
        let emission = emit_translation_unit(
            source,
            &liveness,
            None,
            &slot_liveness,
            &layout,
            &CEmitterConfig {
                backend: CBackend::ComputedGoto,
                ..CEmitterConfig::default()
            },
        );

        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );
        assert!(emission.source.contains("goto *cr_dispatch[ctx->state]"));
        syntax_check_with_standard(&emission.source, "gnu11");
    }

    #[test]
    fn preserves_unaffected_translation_unit_text() {
        let source = "int untouched(void) { return 7; }\n";
        let emission = emit(source);
        assert_eq!(emission.source, source);
        assert!(emission.diagnostics.is_empty());
    }

    #[test]
    fn rejects_missing_context_placement_without_emitting_fallback_source() {
        let source = "__async int run(void) { return 7; }\n";
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("missing-layout.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);
        let cfg = build_cfg(&hir);
        let scoped = lower_scope_exits(&cfg);
        let coroutines = lower_coroutines(&scoped, "cr_");
        let slot_liveness = analyze_slot_liveness(&coroutines);
        let liveness = analyze_liveness(&coroutines);
        let mut layout = build_identity_context_layout(&liveness, None, &slot_liveness);
        layout
            .functions
            .values_mut()
            .next()
            .expect("async layout")
            .fields
            .remove(&LogicalFieldId::State);

        let emission = emit_translation_unit(
            source,
            &liveness,
            None,
            &slot_liveness,
            &layout,
            &CEmitterConfig::default(),
        );

        assert!(emission.source.is_empty());
        assert!(
            emission
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CRC8004")
        );
    }

    #[test]
    fn runtime_header_is_valid_c11() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let header = directory.path().join("cr_runtime.h");
        fs::write(&header, runtime_header()).expect("runtime header");
        let source = directory.path().join("header_test.c");
        fs::write(
            &source,
            "#include \"cr_runtime.h\"\nint main(void) { return 0; }\n",
        )
        .expect("header test source");
        let compiler = available_compiler().expect("Clang or GCC is required for this test");
        let output = Command::new(compiler)
            .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-fsyntax-only"])
            .arg(file_name(&source))
            .current_dir(directory.path())
            .output()
            .expect("native compiler runs");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn generated_await_resumes_and_runs_defer_once() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

typedef struct test_await_state {
    int polls;
    int value;
    cr_error error;
} test_await_state;

static cr_poll_status test_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    (void)poll_context;
    test_await_state *state = (test_await_state *)raw;
    state->polls++;
    if (state->polls == 1) return CR_POLL_PENDING;
    *(int *)out_value = state->value + 1;
    return CR_POLL_READY;
}

static const cr_error *test_error(const void *raw) {
    return &((const test_await_state *)raw)->error;
}

static void test_drop(void *raw) { free(raw); }

static const cr_awaitable_vtable test_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    0u,
    test_poll,
    test_error,
    test_drop,
    sizeof(int),
    _Alignof(int)
};

static cr_awaitable next_value(int value) {
    test_await_state *state = (test_await_state *)calloc(1, sizeof(*state));
    state->value = value;
    return (cr_awaitable){state, &test_vtable};
}

static int cleanup_value;
static void record_cleanup(int value) { cleanup_value += value; }

__async int run(int input) {
    int value = __await next_value(input);
    __defer record_cleanup(value);
    return value;
}

int main(void) {
    cr_run_task task;
    cr_run_init(&task, 41);
    assert(cr_run_poll(&task, NULL) == CR_POLL_PENDING);
    assert(cleanup_value == 0);
    assert(cr_run_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_run_result(&task) == 42);
    assert(cleanup_value == 42);
    assert(cr_run_poll(&task, NULL) == CR_POLL_READY);
    assert(cleanup_value == 42);
    cr_run_drop(&task);
    assert(cleanup_value == 42);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_yield_resumes_to_final_result() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

__async int sequence(void) {
    __yield 5;
    return 9;
}

int main(void) {
    cr_sequence_task task;
    cr_sequence_init(&task);
    assert(cr_sequence_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_sequence_yielded(&task) == 5);
    assert(cr_sequence_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_sequence_result(&task) == 9);
    assert(cr_sequence_poll(&task, NULL) == CR_POLL_READY);
    cr_sequence_drop(&task);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_sync_defer_is_lifo() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

static int order;
static void record(int value) { order = order * 10 + value; }

void guarded(void) {
    __defer record(1);
    __defer record(2);
}

int main(void) {
    guarded();
    assert(order == 21);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_task_binding_and_direct_await_propagate_yield() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

__async int child(int value) {
    __yield value;
    return value + 1;
}

__async int bound_parent(int value) {
    __async int task = child(value);
    int first = __await task;
    int second = __await task;
    return first + second;
}

__async int direct_parent(int value) {
    return __await child(value);
}

int main(void) {
    cr_bound_parent_task bound;
    cr_bound_parent_init(&bound, 7);
    assert(cr_bound_parent_poll(&bound, NULL) == CR_POLL_YIELDED);
    assert(*cr_bound_parent_yielded(&bound) == 7);
    assert(cr_bound_parent_poll(&bound, NULL) == CR_POLL_READY);
    assert(*cr_bound_parent_result(&bound) == 16);
    cr_bound_parent_drop(&bound);

    cr_direct_parent_task direct;
    cr_direct_parent_init(&direct, 11);
    assert(cr_direct_parent_poll(&direct, NULL) == CR_POLL_YIELDED);
    assert(*cr_direct_parent_yielded(&direct) == 11);
    assert(cr_direct_parent_poll(&direct, NULL) == CR_POLL_READY);
    assert(*cr_direct_parent_result(&direct) == 12);
    cr_direct_parent_drop(&direct);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}",
            emission.diagnostics
        );
        assert!(emission.source.contains("cr_child_into_awaitable"));
        let binding_activation = emission
            .source
            .find("= cr_child_into_awaitable")
            .expect("task binding activation is emitted");
        let binding_active = emission.source[binding_activation..]
            .find("_active = true")
            .map(|offset| binding_activation + offset)
            .expect("task binding becomes active after validation");
        let binding_validation = &emission.source[binding_activation..binding_active];
        assert!(
            binding_validation.contains("CR_AWAITABLE_VTABLE_DROP_PREFIX_SIZE"),
            "{binding_validation}"
        );
        assert!(
            binding_validation.contains("vtable->value_size != sizeof(int)"),
            "{binding_validation}"
        );
        assert!(emission.source.contains("captured_generation"));
        assert!(emission.source.contains("static void cr_bound_parent_"));
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_switch_preserves_case_dispatch_fallthrough_and_defer() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

static int cleanup_order;
static void record(int value) { cleanup_order = cleanup_order * 10 + value; }

__async int choose(int value) {
    switch (value) {
    case 1:
        __defer record(1);
        __yield 3;
    case 2:
        return 4;
    default:
        return 5;
    }
}

int main(void) {
    cr_choose_task first;
    cr_choose_init(&first, 1);
    assert(cr_choose_poll(&first, NULL) == CR_POLL_YIELDED);
    assert(*cr_choose_yielded(&first) == 3);
    assert(cleanup_order == 0);
    assert(cr_choose_poll(&first, NULL) == CR_POLL_READY);
    assert(*cr_choose_result(&first) == 4);
    assert(cleanup_order == 1);
    cr_choose_drop(&first);

    cr_choose_task second;
    cr_choose_init(&second, 2);
    assert(cr_choose_poll(&second, NULL) == CR_POLL_READY);
    assert(*cr_choose_result(&second) == 4);
    assert(cleanup_order == 1);
    cr_choose_drop(&second);

    cr_choose_task fallback;
    cr_choose_init(&fallback, 9);
    assert(cr_choose_poll(&fallback, NULL) == CR_POLL_READY);
    assert(*cr_choose_result(&fallback) == 5);
    cr_choose_drop(&fallback);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}\n{}",
            emission.diagnostics,
            emission.source
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_direct_await_controls_if_while_and_switch() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

__async int identity(int value) { return value; }

__async int take_one(int *remaining) {
    if (*remaining == 0) return 0;
    (*remaining)--;
    return 1;
}

__async int control(int value, int *remaining) {
    int score = 0;
    if (__await identity(value)) score += 1;
    while (__await take_one(remaining)) score += 10;
    switch (__await identity(value)) {
    case 2:
        score += 100;
        break;
    default:
        score += 1000;
        break;
    }
    return score;
}

int main(void) {
    int remaining = 2;
    cr_control_task task;
    cr_control_init(&task, 2, &remaining);
    assert(cr_control_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_control_result(&task) == 121);
    assert(remaining == 0);
    cr_control_drop(&task);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}\n{}",
            emission.diagnostics,
            emission.source
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_linear_expression_preserves_multiple_await_results() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

__async int child(int value) {
    __yield value;
    return value;
}

__async int combine(void) {
    int result = 10 + __await child(2) + __await child(3);
    return result + (__await child(4)) * 2;
}

int main(void) {
    cr_combine_task task;
    cr_combine_init(&task);
    assert(cr_combine_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_combine_yielded(&task) == 2);
    assert(cr_combine_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_combine_yielded(&task) == 3);
    assert(cr_combine_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_combine_yielded(&task) == 4);
    assert(cr_combine_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_combine_result(&task) == 23);
    cr_combine_drop(&task);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}\n{}",
            emission.diagnostics,
            emission.source
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_short_circuit_await_preserves_lazy_rhs_evaluation() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

__async int child(int value) {
    __yield value;
    return value;
}

__async int logical_and(int ready) {
    return ready && __await child(5);
}

__async int logical_or(int ready) {
    return ready || __await child(7);
}

int main(void) {
    cr_logical_and_task skipped_and;
    cr_logical_and_init(&skipped_and, 0);
    assert(cr_logical_and_poll(&skipped_and, NULL) == CR_POLL_READY);
    assert(*cr_logical_and_result(&skipped_and) == 0);
    cr_logical_and_drop(&skipped_and);

    cr_logical_and_task evaluated_and;
    cr_logical_and_init(&evaluated_and, 1);
    assert(cr_logical_and_poll(&evaluated_and, NULL) == CR_POLL_YIELDED);
    assert(*cr_logical_and_yielded(&evaluated_and) == 5);
    assert(cr_logical_and_poll(&evaluated_and, NULL) == CR_POLL_READY);
    assert(*cr_logical_and_result(&evaluated_and) == 1);
    cr_logical_and_drop(&evaluated_and);

    cr_logical_or_task skipped_or;
    cr_logical_or_init(&skipped_or, 1);
    assert(cr_logical_or_poll(&skipped_or, NULL) == CR_POLL_READY);
    assert(*cr_logical_or_result(&skipped_or) == 1);
    cr_logical_or_drop(&skipped_or);

    cr_logical_or_task evaluated_or;
    cr_logical_or_init(&evaluated_or, 0);
    assert(cr_logical_or_poll(&evaluated_or, NULL) == CR_POLL_YIELDED);
    assert(*cr_logical_or_yielded(&evaluated_or) == 7);
    assert(cr_logical_or_poll(&evaluated_or, NULL) == CR_POLL_READY);
    assert(*cr_logical_or_result(&evaluated_or) == 1);
    cr_logical_or_drop(&evaluated_or);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}\n{}",
            emission.diagnostics,
            emission.source
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_conditional_and_comma_await_preserve_selected_order() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

__async int child(int value) {
    __yield value;
    return value;
}

__async int select_value(int choose_first) {
    return choose_first ? __await child(3) : __await child(4);
}

__async int comma_value(int *order) {
    return ((*order = 1), __await child(5));
}

int main(void) {
    cr_select_value_task first;
    cr_select_value_init(&first, 1);
    assert(cr_select_value_poll(&first, NULL) == CR_POLL_YIELDED);
    assert(*cr_select_value_yielded(&first) == 3);
    assert(cr_select_value_poll(&first, NULL) == CR_POLL_READY);
    assert(*cr_select_value_result(&first) == 3);
    cr_select_value_drop(&first);

    cr_select_value_task second;
    cr_select_value_init(&second, 0);
    assert(cr_select_value_poll(&second, NULL) == CR_POLL_YIELDED);
    assert(*cr_select_value_yielded(&second) == 4);
    assert(cr_select_value_poll(&second, NULL) == CR_POLL_READY);
    assert(*cr_select_value_result(&second) == 4);
    cr_select_value_drop(&second);

    int order = 0;
    cr_comma_value_task comma;
    cr_comma_value_init(&comma, &order);
    assert(cr_comma_value_poll(&comma, NULL) == CR_POLL_YIELDED);
    assert(order == 1);
    assert(*cr_comma_value_yielded(&comma) == 5);
    assert(cr_comma_value_poll(&comma, NULL) == CR_POLL_READY);
    assert(*cr_comma_value_result(&comma) == 5);
    cr_comma_value_drop(&comma);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}\n{}",
            emission.diagnostics,
            emission.source
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_assignment_and_call_arguments_await_in_source_order() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

static int sum3(int first, int second, int third) {
    return first + second + third;
}

__async int child(int value) {
    __yield value;
    return value;
}

__async int compose(void) {
    int result = 0;
    result = __await child(2);
    result = sum3(result, __await child(3), __await child(4));
    return result;
}

int main(void) {
    cr_compose_task task;
    cr_compose_init(&task);
    assert(cr_compose_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_compose_yielded(&task) == 2);
    assert(cr_compose_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_compose_yielded(&task) == 3);
    assert(cr_compose_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_compose_yielded(&task) == 4);
    assert(cr_compose_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_compose_result(&task) == 9);
    cr_compose_drop(&task);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}\n{}",
            emission.diagnostics,
            emission.source
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_error_cancel_and_layout_protocols_drop_once() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

typedef struct operation_state {
    cr_poll_status status;
    cr_error error;
} operation_state;

static int operation_polls;
static int operation_drops;
static int cleanup_count;

static cr_poll_status operation_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    (void)poll_context;
    operation_state *state = (operation_state *)raw;
    operation_polls++;
    if (out_value != NULL) *(int *)out_value = 42;
    return state->status;
}

static const cr_error *operation_error(const void *raw) {
    return &((const operation_state *)raw)->error;
}

static void operation_drop(void *raw) {
    operation_drops++;
    free(raw);
}

static const cr_awaitable_vtable operation_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    0u,
    operation_poll,
    operation_error,
    operation_drop,
    sizeof(int),
    _Alignof(int)
};

static const cr_awaitable_vtable operation_mismatch_vtable = {
    CR_AWAITABLE_VTABLE_ABI_VERSION,
    sizeof(cr_awaitable_vtable),
    0u,
    0u,
    operation_poll,
    operation_error,
    operation_drop,
    sizeof(short),
    _Alignof(int)
};

static cr_awaitable operation(cr_poll_status status, size_t value_size) {
    operation_state *state = (operation_state *)calloc(1, sizeof(*state));
    state->status = status;
    state->error = (cr_error){77, "operation failed"};
    const cr_awaitable_vtable *vtable =
        value_size == sizeof(int) ? &operation_vtable : &operation_mismatch_vtable;
    return (cr_awaitable){state, vtable};
}

static void cleanup(int value) { cleanup_count += value; }

__async int error_parent(void) {
    __defer cleanup(1);
    return __await operation(CR_POLL_ERROR, sizeof(int));
}

__async int canceled_parent(void) {
    __defer cleanup(1);
    return __await operation(CR_POLL_CANCELED, sizeof(int));
}

__async int mismatch_parent(void) {
    __defer cleanup(1);
    return __await operation(CR_POLL_READY, sizeof(short));
}

int main(void) {
    cr_error_parent_task failed;
    cr_error_parent_init(&failed);
    assert(cr_error_parent_poll(&failed, NULL) == CR_POLL_ERROR);
    assert(cr_error_parent_error(&failed)->code == 77);
    assert(operation_polls == 1);
    assert(operation_drops == 1);
    assert(cleanup_count == 1);
    assert(cr_error_parent_poll(&failed, NULL) == CR_POLL_ERROR);
    cr_error_parent_drop(&failed);
    assert(operation_drops == 1);
    assert(cleanup_count == 1);

    cr_canceled_parent_task canceled;
    cr_canceled_parent_init(&canceled);
    assert(cr_canceled_parent_poll(&canceled, NULL) == CR_POLL_CANCELED);
    assert(operation_polls == 2);
    assert(operation_drops == 2);
    assert(cleanup_count == 2);
    assert(cr_canceled_parent_poll(&canceled, NULL) == CR_POLL_CANCELED);
    cr_canceled_parent_drop(&canceled);
    assert(operation_drops == 2);
    assert(cleanup_count == 2);

    cr_mismatch_parent_task mismatch;
    cr_mismatch_parent_init(&mismatch);
    assert(cr_mismatch_parent_poll(&mismatch, NULL) == CR_POLL_ERROR);
    assert(cr_mismatch_parent_error(&mismatch)->code == CR_ERROR_AWAITABLE_LAYOUT_MISMATCH);
    assert(operation_polls == 2);
    assert(operation_drops == 3);
    assert(cleanup_count == 3);
    assert(cr_mismatch_parent_poll(&mismatch, NULL) == CR_POLL_ERROR);
    cr_mismatch_parent_drop(&mismatch);
    assert(operation_drops == 3);
    assert(cleanup_count == 3);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}\n{}",
            emission.diagnostics,
            emission.source
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_arrays_and_function_pointer_parameters_survive_yield() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

static int add_one(int value) { return value + 1; }

__async int arrays(int input[2]) {
    int values[2] = {input[0], input[1]};
    __yield values[0];
    return values[1];
}

__async int callback(int (*function)(int), int value) {
    __yield value;
    return function(value);
}

int main(void) {
    int input[2] = {4, 9};
    cr_arrays_task array_task;
    cr_arrays_init(&array_task, input);
    assert(cr_arrays_poll(&array_task, NULL) == CR_POLL_YIELDED);
    assert(*cr_arrays_yielded(&array_task) == 4);
    assert(cr_arrays_poll(&array_task, NULL) == CR_POLL_READY);
    assert(*cr_arrays_result(&array_task) == 9);
    cr_arrays_drop(&array_task);

    cr_callback_task callback_task;
    cr_callback_init(&callback_task, add_one, 10);
    assert(cr_callback_poll(&callback_task, NULL) == CR_POLL_YIELDED);
    assert(*cr_callback_yielded(&callback_task) == 10);
    assert(cr_callback_poll(&callback_task, NULL) == CR_POLL_READY);
    assert(*cr_callback_result(&callback_task) == 11);
    cr_callback_drop(&callback_task);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}\n{}",
            emission.diagnostics,
            emission.source
        );
        compile_and_run(&emission.source);
    }

    #[test]
    fn generated_for_initializer_update_and_do_while_can_suspend() {
        let emission = emit(
            r#"
#include "cr_runtime.h"
#include <assert.h>

__async int child(int value) {
    __yield value;
    return value;
}

__async int loops(void) {
    int sum = 0;
    for (int index = __await child(0); index < 2; index = __await child(index + 1)) {
        sum++;
    }
    do {
        sum++;
    } while (__await child(0));
    return sum;
}

int main(void) {
    cr_loops_task task;
    cr_loops_init(&task);
    assert(cr_loops_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_loops_yielded(&task) == 0);
    assert(cr_loops_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_loops_yielded(&task) == 1);
    assert(cr_loops_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_loops_yielded(&task) == 2);
    assert(cr_loops_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_loops_yielded(&task) == 0);
    assert(cr_loops_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_loops_result(&task) == 3);
    cr_loops_drop(&task);
    return 0;
}
"#,
        );
        assert!(
            emission.diagnostics.is_empty(),
            "{:?}\n{}",
            emission.diagnostics,
            emission.source
        );
        compile_and_run(&emission.source);
    }

    fn file_name(path: &Path) -> &str {
        path.file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 test path")
    }
}
