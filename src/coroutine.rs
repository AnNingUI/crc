//! Coroutine state assignment and ABI metadata for the new CFG.

use std::collections::{BTreeMap, BTreeSet};

use crate::await_plan::{FunctionAwaitPlan, build_function_await_plan};
use crate::control_flow::{BlockId, CfgFunction, CfgTerminator, CfgUnit};
use crate::semantic::AwaitSlotId;
use crate::syntax::{Diagnostic, DiagnosticSeverity, SourceSpan};

/// Runtime poll states shared by generated task entry points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollStatus {
    Pending,
    Yielded,
    Ready,
    Error,
    Canceled,
}

impl PollStatus {
    #[must_use]
    pub const fn c_name(self) -> &'static str {
        match self {
            Self::Pending => "CR_POLL_PENDING",
            Self::Yielded => "CR_POLL_YIELDED",
            Self::Ready => "CR_POLL_READY",
            Self::Error => "CR_POLL_ERROR",
            Self::Canceled => "CR_POLL_CANCELED",
        }
    }
}

/// One stable state dispatched by a generated poll function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResumeState {
    pub state: u32,
    pub block: BlockId,
}

/// Storage required for an await expression that survives pending polls.
#[derive(Debug, Clone)]
pub struct AwaitSlot {
    pub id: AwaitSlotId,
    pub span: SourceSpan,
}

/// A CFG plus deterministic state and task ABI names.
#[derive(Debug, Clone)]
pub struct CoroutineFunction {
    pub cfg: CfgFunction,
    pub await_plan: FunctionAwaitPlan,
    pub task_type: String,
    pub init_name: String,
    pub poll_name: String,
    pub drop_name: String,
    pub states: Vec<ResumeState>,
    pub state_by_block: BTreeMap<BlockId, u32>,
    pub await_slots: Vec<AwaitSlot>,
}

/// Coroutine-lowered translation-unit functions.
#[derive(Debug, Clone)]
pub struct CoroutineUnit {
    pub functions: Vec<CoroutineFunction>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Assigns resume states without changing CFG control-flow semantics.
#[must_use]
pub fn lower_coroutines(cfg: &CfgUnit, prefix: &str) -> CoroutineUnit {
    let mut diagnostics = cfg.diagnostics.clone();
    let functions = cfg
        .functions
        .iter()
        .map(|function| lower_function(function, prefix, &mut diagnostics))
        .collect();
    CoroutineUnit {
        functions,
        diagnostics,
    }
}

fn lower_function(
    function: &CfgFunction,
    prefix: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> CoroutineFunction {
    let stem = c_identifier(&function.name);
    let prefix = c_identifier(prefix);
    let symbol_prefix = if prefix.is_empty() {
        stem.clone()
    } else {
        format!("{prefix}{stem}")
    };

    let mut resume_blocks = BTreeSet::new();
    resume_blocks.insert(function.entry);
    let mut await_slots = BTreeMap::new();
    for block in &function.blocks {
        for instruction in &block.instructions {
            if let crate::control_flow::CfgInstruction::AssignExpressionSlot {
                slot, span, ..
            } = instruction
            {
                await_slots.entry(*slot).or_insert_with(|| AwaitSlot {
                    id: *slot,
                    span: span.clone(),
                });
            }
        }
        match &block.terminator {
            CfgTerminator::Suspend { slot, span, .. } => {
                if !function.is_async {
                    diagnostics.push(extension_in_sync_function(function, "await", span));
                }
                resume_blocks.insert(block.id);
                await_slots.entry(*slot).or_insert_with(|| AwaitSlot {
                    id: *slot,
                    span: span.clone(),
                });
            }
            CfgTerminator::Yield {
                continuation, span, ..
            } => {
                if !function.is_async {
                    diagnostics.push(extension_in_sync_function(function, "yield", span));
                }
                resume_blocks.insert(continuation.target);
            }
            _ => {}
        }
    }

    let states: Vec<_> = resume_blocks
        .into_iter()
        .enumerate()
        .map(|(state, block)| ResumeState {
            state: state as u32,
            block,
        })
        .collect();
    let state_by_block = states
        .iter()
        .map(|resume| (resume.block, resume.state))
        .collect();

    CoroutineFunction {
        await_plan: build_function_await_plan(function),
        cfg: function.clone(),
        task_type: format!("{symbol_prefix}_task"),
        init_name: format!("{symbol_prefix}_init"),
        poll_name: format!("{symbol_prefix}_poll"),
        drop_name: format!("{symbol_prefix}_drop"),
        states,
        state_by_block,
        await_slots: await_slots.into_values().collect(),
    }
}

fn extension_in_sync_function(
    function: &CfgFunction,
    extension: &str,
    span: &SourceSpan,
) -> Diagnostic {
    Diagnostic {
        code: "CRC4001",
        severity: DiagnosticSeverity::Error,
        message: format!(
            "{extension} suspension reached non-async function `{}`",
            function.name
        ),
        primary_span: span.clone(),
        related: Vec::new(),
    }
}

fn c_identifier(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for (index, character) in value.chars().enumerate() {
        let valid = if index == 0 {
            character == '_' || character.is_ascii_alphabetic()
        } else {
            character == '_' || character.is_ascii_alphanumeric()
        };
        if valid {
            output.push(character);
        } else {
            output.push('_');
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::control_flow::build_cfg;
    use crate::scope_exit::lower_scope_exits;
    use crate::semantic::build_hir;
    use crate::syntax::SyntaxParser;

    use super::*;

    fn lowered(source: &str) -> CoroutineUnit {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("coroutine.cr"), source)
            .expect("source parses");
        let hir = build_hir(&syntax);
        let cfg = build_cfg(&hir);
        lower_coroutines(&lower_scope_exits(&cfg), "cr_")
    }

    #[test]
    fn assigns_stable_states_to_entry_and_resume_blocks() {
        let unit = lowered(
            r#"
__async int run(int task) {
    __await task;
    __yield 1;
    return 2;
}
"#,
        );

        assert!(unit.diagnostics.is_empty(), "{:?}", unit.diagnostics);
        let function = &unit.functions[0];
        assert_eq!(function.states.len(), 3);
        assert_eq!(function.states[0].state, 0);
        assert_eq!(function.states[0].block, function.cfg.entry);
        assert_eq!(function.await_slots.len(), 1);
        assert_eq!(function.task_type, "cr_run_task");
        assert_eq!(function.poll_name, "cr_run_poll");
    }

    #[test]
    fn synchronous_defer_function_has_only_entry_state() {
        let unit = lowered(
            r#"
int guarded(int handle) {
    __defer close_handle(handle);
    return handle;
}
"#,
        );

        assert!(unit.diagnostics.is_empty(), "{:?}", unit.diagnostics);
        assert_eq!(unit.functions[0].states.len(), 1);
        assert!(unit.functions[0].await_slots.is_empty());
    }

    #[test]
    fn poll_status_names_match_runtime_abi() {
        assert_eq!(PollStatus::Pending.c_name(), "CR_POLL_PENDING");
        assert_eq!(PollStatus::Yielded.c_name(), "CR_POLL_YIELDED");
        assert_eq!(PollStatus::Ready.c_name(), "CR_POLL_READY");
        assert_eq!(PollStatus::Error.c_name(), "CR_POLL_ERROR");
        assert_eq!(PollStatus::Canceled.c_name(), "CR_POLL_CANCELED");
    }
}
