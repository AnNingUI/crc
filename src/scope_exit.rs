//! Scope-exit and dynamic defer lowering for the identity-based CFG.

use std::collections::HashSet;

use crate::control_flow::{
    BasicBlock, BlockId, CfgEdge, CfgFunction, CfgInstruction, CfgTerminator, CfgUnit, CleanupId,
    CleanupRegistration, EdgeKind,
};
use crate::semantic::{HirDefer, HirExprKind, ScopeId, SourceFragment};
use crate::syntax::{Diagnostic, DiagnosticSeverity, SourceSpan};

/// Validates defer registrations and inserts cleanup operations on scope exits.
#[must_use]
pub fn lower_scope_exits(cfg: &CfgUnit) -> CfgUnit {
    let mut diagnostics = cfg.diagnostics.clone();
    let functions = cfg
        .functions
        .iter()
        .map(|function| lower_function(function, &mut diagnostics))
        .collect();
    CfgUnit {
        functions,
        diagnostics,
    }
}

fn lower_function(function: &CfgFunction, diagnostics: &mut Vec<Diagnostic>) -> CfgFunction {
    let mut function = function.clone();
    let mut next_cleanup = 0;
    let mut registered_scopes = HashSet::new();
    let variably_modified_scopes: HashSet<_> = function
        .blocks
        .iter()
        .flat_map(|block| &block.instructions)
        .filter_map(|instruction| {
            let CfgInstruction::Declaration(declaration) = instruction else {
                return None;
            };
            is_variably_modified(&declaration.ty.text).then_some(declaration.scope)
        })
        .collect();

    for block in &mut function.blocks {
        for instruction in &mut block.instructions {
            let CfgInstruction::RegisterDefer(defer) = instruction else {
                continue;
            };
            match lower_registration(defer, CleanupId(next_cleanup)) {
                Ok(registration) => {
                    registered_scopes.insert(registration.scope);
                    *instruction = CfgInstruction::PushCleanup(registration);
                    next_cleanup += 1;
                }
                Err(diagnostic) => diagnostics.push(*diagnostic),
            }
        }
    }

    let original_blocks = function.blocks.len();
    for index in 0..original_blocks {
        let block_scopes = function.blocks[index].scope_stack.clone();
        let terminator = function.blocks[index].terminator.clone();
        function.blocks[index].terminator = match terminator {
            CfgTerminator::Goto(edge) => CfgTerminator::Goto(rewrite_edge(
                &mut function.blocks,
                edge,
                &registered_scopes,
                &variably_modified_scopes,
                diagnostics,
            )),
            CfgTerminator::Branch {
                condition,
                consequence,
                alternative,
            } => CfgTerminator::Branch {
                condition,
                consequence: rewrite_edge(
                    &mut function.blocks,
                    consequence,
                    &registered_scopes,
                    &variably_modified_scopes,
                    diagnostics,
                ),
                alternative: rewrite_edge(
                    &mut function.blocks,
                    alternative,
                    &registered_scopes,
                    &variably_modified_scopes,
                    diagnostics,
                ),
            },
            CfgTerminator::Switch {
                expression,
                cases,
                default,
            } => CfgTerminator::Switch {
                expression,
                cases: cases
                    .into_iter()
                    .map(|case| crate::control_flow::CfgSwitchCase {
                        value: case.value,
                        edge: rewrite_edge(
                            &mut function.blocks,
                            case.edge,
                            &registered_scopes,
                            &variably_modified_scopes,
                            diagnostics,
                        ),
                    })
                    .collect(),
                default: rewrite_edge(
                    &mut function.blocks,
                    default,
                    &registered_scopes,
                    &variably_modified_scopes,
                    diagnostics,
                ),
            },
            CfgTerminator::Suspend {
                edge,
                operand,
                slot,
                continuation,
                span,
            } => {
                diagnose_suspend_exit(&continuation, &span, diagnostics);
                CfgTerminator::Suspend {
                    edge,
                    operand,
                    slot,
                    continuation,
                    span,
                }
            }
            CfgTerminator::Yield {
                value,
                continuation,
                span,
            } => {
                diagnose_suspend_exit(&continuation, &span, diagnostics);
                CfgTerminator::Yield {
                    value,
                    continuation,
                    span,
                }
            }
            CfgTerminator::Return { value, span } => {
                let exited_scopes = registered_exits(&block_scopes, &[], &registered_scopes);
                if !exited_scopes.is_empty() {
                    function.blocks[index]
                        .instructions
                        .push(CfgInstruction::RunCleanups { exited_scopes });
                }
                CfgTerminator::Return { value, span }
            }
            CfgTerminator::Open => {
                diagnostics.push(Diagnostic {
                    code: "CRC3004",
                    severity: DiagnosticSeverity::Error,
                    message: "scope-exit lowering received an open CFG block".to_owned(),
                    primary_span: function.span.clone(),
                    related: Vec::new(),
                });
                CfgTerminator::Unreachable
            }
            CfgTerminator::Unreachable => CfgTerminator::Unreachable,
        };
    }
    function
}

fn lower_registration(
    defer: &HirDefer,
    id: CleanupId,
) -> Result<CleanupRegistration, Box<Diagnostic>> {
    let HirExprKind::Call {
        function,
        arguments,
    } = &defer.call.kind
    else {
        return Err(Box::new(invalid_defer(
            defer,
            "`__defer` requires a direct function call",
        )));
    };
    let HirExprKind::Source(name) = &function.kind else {
        return Err(Box::new(invalid_defer(
            defer,
            "portable `__defer` requires a directly named function",
        )));
    };
    if !is_identifier(name) {
        return Err(Box::new(invalid_defer(
            defer,
            "portable `__defer` can't capture an indirect or member call",
        )));
    }

    Ok(CleanupRegistration {
        id,
        scope: defer.scope,
        function: SourceFragment {
            text: name.clone(),
            span: function.span.clone(),
        },
        arguments: arguments.clone(),
        span: defer.span.clone(),
    })
}

fn invalid_defer(defer: &HirDefer, message: &str) -> Diagnostic {
    Diagnostic {
        code: "CRC3001",
        severity: DiagnosticSeverity::Error,
        message: message.to_owned(),
        primary_span: defer.span.clone(),
        related: Vec::new(),
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_alphabetic())
        && chars.all(|character| character == '_' || character.is_alphanumeric())
}

fn rewrite_edge(
    blocks: &mut Vec<BasicBlock>,
    edge: CfgEdge,
    registered_scopes: &HashSet<ScopeId>,
    variably_modified_scopes: &HashSet<ScopeId>,
    diagnostics: &mut Vec<Diagnostic>,
) -> CfgEdge {
    let common = common_scope_prefix(&edge.source_scopes, &edge.target_scopes);
    if edge.kind == EdgeKind::UserGoto
        && edge.target_scopes[common..]
            .iter()
            .any(|scope| variably_modified_scopes.contains(scope))
    {
        diagnostics.push(Diagnostic {
            code: "CRC3002",
            severity: DiagnosticSeverity::Error,
            message: "goto enters the scope of a variably modified declaration".to_owned(),
            primary_span: edge.span.clone(),
            related: Vec::new(),
        });
        return edge;
    }
    let exited_scopes =
        registered_exits(&edge.source_scopes, &edge.target_scopes, registered_scopes);
    if exited_scopes.is_empty() {
        return edge;
    }

    let cleanup_block = BlockId(blocks.len() as u32);
    let final_edge = CfgEdge {
        target: edge.target,
        kind: EdgeKind::Cleanup,
        span: edge.span.clone(),
        source_scopes: edge.target_scopes.clone(),
        target_scopes: edge.target_scopes.clone(),
    };
    blocks.push(BasicBlock {
        id: cleanup_block,
        scope_stack: edge.target_scopes.clone(),
        instructions: vec![CfgInstruction::RunCleanups { exited_scopes }],
        terminator: CfgTerminator::Goto(final_edge),
    });

    CfgEdge {
        target: cleanup_block,
        kind: edge.kind,
        span: edge.span,
        source_scopes: edge.source_scopes,
        target_scopes: edge.target_scopes,
    }
}

fn is_variably_modified(ty: &str) -> bool {
    let Some(open) = ty.find('[') else {
        return false;
    };
    let Some(close) = ty[open + 1..].find(']').map(|index| open + 1 + index) else {
        return true;
    };
    let length = ty[open + 1..close].trim();
    length == "*" || !length.chars().all(|character| character.is_ascii_digit())
}

fn diagnose_suspend_exit(
    continuation: &CfgEdge,
    span: &SourceSpan,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if continuation.source_scopes != continuation.target_scopes {
        diagnostics.push(Diagnostic {
            code: "CRC3003",
            severity: DiagnosticSeverity::Error,
            message: "suspension can't leave a lexical scope".to_owned(),
            primary_span: span.clone(),
            related: Vec::new(),
        });
    }
}

fn registered_exits(
    source: &[ScopeId],
    target: &[ScopeId],
    registered_scopes: &HashSet<ScopeId>,
) -> Vec<ScopeId> {
    let common = common_scope_prefix(source, target);
    source[common..]
        .iter()
        .rev()
        .filter(|scope| registered_scopes.contains(scope))
        .copied()
        .collect()
}

fn common_scope_prefix(left: &[ScopeId], right: &[ScopeId]) -> usize {
    left.iter()
        .zip(right)
        .take_while(|(left, right)| left == right)
        .count()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::control_flow::build_cfg;
    use crate::semantic::build_hir;
    use crate::syntax::SyntaxParser;

    use super::*;

    fn lowered(source: &str) -> CfgUnit {
        let mut parser = SyntaxParser::new().expect("grammar loads");
        let syntax = parser
            .parse(PathBuf::from("defer.cr"), source)
            .expect("source parses");
        lower_scope_exits(&build_cfg(&build_hir(&syntax)))
    }

    #[test]
    fn lowers_sync_defer_registration_and_return_cleanup() {
        let cfg = lowered(
            r#"
int guarded(int handle) {
    __defer close_handle(handle);
    return handle;
}
"#,
        );

        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
        let instructions: Vec<_> = cfg.functions[0]
            .blocks
            .iter()
            .flat_map(|block| &block.instructions)
            .collect();
        assert!(
            instructions
                .iter()
                .any(|instruction| matches!(instruction, CfgInstruction::PushCleanup(_)))
        );
        assert!(
            instructions
                .iter()
                .any(|instruction| matches!(instruction, CfgInstruction::RunCleanups { .. }))
        );
    }

    #[test]
    fn suspend_does_not_run_defer() {
        let cfg = lowered(
            r#"
__async int fetch(int socket) {
    __defer close_socket(socket);
    __await read_socket(socket);
    return 0;
}
"#,
        );

        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
        let suspend_block = cfg.functions[0]
            .blocks
            .iter()
            .find(|block| matches!(block.terminator, CfgTerminator::Suspend { .. }))
            .expect("suspend block");
        assert!(
            !suspend_block
                .instructions
                .iter()
                .any(|instruction| matches!(instruction, CfgInstruction::RunCleanups { .. }))
        );
    }

    #[test]
    fn outward_goto_runs_nested_cleanup() {
        let cfg = lowered(
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
        assert!(cfg.functions[0].blocks.iter().any(|block| {
            matches!(
                block.instructions.as_slice(),
                [CfgInstruction::RunCleanups { .. }]
            ) && matches!(block.terminator, CfgTerminator::Goto(_))
        }));
    }

    #[test]
    fn allows_goto_that_skips_dynamic_defer_registration() {
        let cfg = lowered(
            r#"
__async int valid(int value) {
    goto inside;
    {
        __defer release(value);
inside:
        return 0;
    }
}
"#,
        );

        assert!(cfg.diagnostics.is_empty(), "{:?}", cfg.diagnostics);
    }

    #[test]
    fn rejects_goto_into_variably_modified_scope() {
        let cfg = lowered(
            r#"
__async int invalid(int size) {
    goto inside;
    {
        int values[size];
inside:
        return values[0];
    }
}
"#,
        );

        assert!(
            cfg.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CRC3002")
        );
    }
}
