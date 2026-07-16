use std::fs;
use std::path::Path;
use std::process::Command;

use crc_lib::Compiler;
use crc_lib::config::{Config, OptimizationLevel};
use crc_lib::control_flow::build_cfg;
use crc_lib::coroutine::lower_coroutines;
use crc_lib::coroutine_opt::optimize_coroutine_cfg;
use crc_lib::runtime_abi::runtime_header;
use crc_lib::scope_exit::lower_scope_exits;
use crc_lib::semantic::build_hir;
use crc_lib::slot_liveness::{LogicalStorage, analyze_slot_liveness};
use crc_lib::syntax::SyntaxParser;

const OPTIMIZATION_SOURCE: &str = r#"
#include <assert.h>

static int lifecycle[5];
static int lifecycle_len;

static void record_lifecycle(int event) {
    lifecycle[lifecycle_len++] = event;
}

__async int child(int value) {
    record_lifecycle(2);
    __yield value + 1;
    record_lifecycle(3);
    return value + 2;
}

__async int optimized_flow(int value) {
    record_lifecycle(1);
    if (value) {
        __yield value;
    }
    record_lifecycle(4);
    int result = __await child(value);
    record_lifecycle(5);
    return result;

dead:
    return -1;
}

int main(void) {
    cr_optimized_flow_task task;
    cr_optimized_flow_init(&task, 7);
    assert(cr_optimized_flow_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_optimized_flow_yielded(&task) == 7);
    assert(cr_optimized_flow_poll(&task, NULL) == CR_POLL_YIELDED);
    assert(*cr_optimized_flow_yielded(&task) == 8);
    assert(cr_optimized_flow_poll(&task, NULL) == CR_POLL_READY);
    assert(*cr_optimized_flow_result(&task) == 9);
    assert(lifecycle_len == 5);
    assert(lifecycle[0] == 1);
    assert(lifecycle[1] == 4);
    assert(lifecycle[2] == 2);
    assert(lifecycle[3] == 3);
    assert(lifecycle[4] == 5);
    cr_optimized_flow_drop(&task);
    return 0;
}
"#;

fn available_compiler() -> &'static str {
    ["clang", "gcc"]
        .into_iter()
        .find(|compiler| {
            Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("Clang or GCC is required for coroutine optimization tests")
}

fn compile(level: OptimizationLevel) -> crc_lib::CompilationOutput {
    let mut config = Config::default();
    config.build.optimization = level;
    Compiler::new(config)
        .compile_source_with_report(OPTIMIZATION_SOURCE, Path::new("coroutine-optimization.cr"))
        .expect("optimization fixture compiles")
}

fn compile_and_run_c(source: &str, level: OptimizationLevel) {
    let directory = tempfile::tempdir().expect("temporary directory");
    fs::write(directory.path().join("cr_runtime.h"), runtime_header())
        .expect("runtime header is written");
    fs::write(directory.path().join("optimization.c"), source).expect("generated C is written");
    let executable = if cfg!(windows) {
        "optimization.exe"
    } else {
        "optimization"
    };
    let compilation = Command::new(available_compiler())
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "optimization.c",
            "-o",
        ])
        .arg(executable)
        .current_dir(directory.path())
        .output()
        .expect("native C compiler runs");
    assert!(
        compilation.status.success(),
        "{level:?} compilation failed:\n{}",
        String::from_utf8_lossy(&compilation.stderr)
    );
    let execution = Command::new(directory.path().join(executable))
        .current_dir(directory.path())
        .output()
        .expect("optimization executable runs");
    assert!(
        execution.status.success(),
        "{level:?} execution failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
}

#[test]
fn optimization_metrics_are_verified_reductions_and_deterministic() {
    let none = compile(OptimizationLevel::None);
    assert_eq!(none.optimization.level, OptimizationLevel::None);
    assert_eq!(
        none.optimization.input_blocks,
        none.optimization.output_blocks
    );
    assert_eq!(none.optimization.passes.len(), 1);
    assert_eq!(none.optimization.passes[0].pass, "verify-none");
    assert!(!none.optimization.passes[0].changed);
    assert_eq!(
        none.optimization.input_blocks,
        none.optimization.passes[0].input_blocks
    );
    assert_eq!(
        none.optimization.output_blocks,
        none.optimization.passes[0].output_blocks
    );

    let mut optimized_metrics = Vec::new();
    for level in [
        OptimizationLevel::Speed,
        OptimizationLevel::Size,
        OptimizationLevel::Aggressive,
    ] {
        let first = compile(level);
        let repeated = compile(level);
        assert_eq!(first.source, repeated.source);
        assert_eq!(first.optimization, repeated.optimization);
        assert_eq!(first.optimization.level, level);
        assert_eq!(
            first.optimization.input_blocks,
            none.optimization.input_blocks
        );
        assert!(first.optimization.output_blocks <= none.optimization.output_blocks);
        assert!(first.optimization.resume_states <= none.optimization.resume_states);
        assert_eq!(
            first
                .optimization
                .passes
                .iter()
                .map(|report| report.pass)
                .collect::<Vec<_>>(),
            [
                "remove-unreachable",
                "thread-trivial-jumps",
                "merge-linear-blocks"
            ]
        );
        assert_eq!(
            first.optimization.input_blocks,
            first.optimization.passes[0].input_blocks
        );
        assert_eq!(
            first.optimization.output_blocks,
            first
                .optimization
                .passes
                .last()
                .expect("optimized pass report")
                .output_blocks
        );
        assert!(
            first
                .optimization
                .passes
                .windows(2)
                .all(|pair| pair[0].output_blocks == pair[1].input_blocks)
        );
        optimized_metrics.push(first.optimization);
    }

    assert!(
        optimized_metrics
            .iter()
            .any(|metrics| metrics.output_blocks < none.optimization.output_blocks)
    );
    assert!(
        optimized_metrics
            .windows(2)
            .all(|pair| pair[0].passes == pair[1].passes)
    );
}

#[test]
fn every_optimization_level_has_identical_native_behavior() {
    for level in [
        OptimizationLevel::None,
        OptimizationLevel::Speed,
        OptimizationLevel::Size,
        OptimizationLevel::Aggressive,
    ] {
        let output = compile(level);
        compile_and_run_c(&output.source, level);
    }
}

#[test]
fn optimization_levels_keep_child_active_flags_independent() {
    for level in [
        OptimizationLevel::None,
        OptimizationLevel::Speed,
        OptimizationLevel::Size,
        OptimizationLevel::Aggressive,
    ] {
        let output = compile(level);
        let layout_start = output
            .source
            .find("struct cr_optimized_flow_task {")
            .expect("optimized-flow context layout");
        let layout_end = output.source[layout_start..]
            .find("};")
            .map(|offset| layout_start + offset)
            .expect("optimized-flow context layout end");
        let layout = &output.source[layout_start..layout_end];

        assert!(layout.contains("cr_child_task cr_child_0;"), "{level:?}");
        assert!(layout.contains("bool cr_child_0_active;"), "{level:?}");
        assert!(layout.contains("int cr_await_0_result;"), "{level:?}");
        if matches!(level, OptimizationLevel::None | OptimizationLevel::Speed) {
            assert!(!layout.contains("union"), "{level:?}");
            assert!(!layout.contains("cr_slot_"), "{level:?}");
        } else {
            assert!(layout.contains("union"), "{level:?}");
            assert!(layout.contains("cr_slot_"), "{level:?}");
        }
    }
}

#[test]
fn ownership_interference_is_stable_after_cfg_optimization() {
    let source = r#"
__async int child(int value) { return value; }
__async int sequential(void) {
    int first = __await child(1);
    int second = __await child(2);
    return first + second;
}
__async int nested(void) {
    return (__await child(3)) + (__await child(4));
}
"#;
    let mut parser = SyntaxParser::new().expect("grammar loads");
    let syntax = parser
        .parse(Path::new("ownership.cr").to_path_buf(), source)
        .expect("ownership source parses");
    let cfg = lower_scope_exits(&build_cfg(&build_hir(&syntax)));
    let optimized = optimize_coroutine_cfg(&cfg, OptimizationLevel::Speed);
    assert!(
        optimized.diagnostics.is_empty(),
        "{:?}",
        optimized.diagnostics
    );
    let coroutines = lower_coroutines(
        optimized.unit.as_ref().expect("verified optimized CFG"),
        "cr_",
    );
    let first = analyze_slot_liveness(&coroutines);
    let repeated = analyze_slot_liveness(&coroutines);
    assert_eq!(first, repeated);

    let sequential = first
        .functions
        .iter()
        .find(|function| function.function_name == "sequential")
        .expect("sequential ownership");
    let sequential_children: Vec<_> = sequential.direct_children.keys().copied().collect();
    assert_eq!(sequential_children.len(), 2);
    assert!(!sequential.interferes(
        LogicalStorage::DirectChild(sequential_children[0]),
        LogicalStorage::DirectChild(sequential_children[1])
    ));

    let nested = first
        .functions
        .iter()
        .find(|function| function.function_name == "nested")
        .expect("nested ownership");
    let nested_children: Vec<_> = nested.direct_children.values().collect();
    assert_eq!(nested_children.len(), 2);
    assert!(nested.interferes(
        LogicalStorage::DirectChild(nested_children[1].child),
        LogicalStorage::AwaitResult(nested_children[0].result_slot)
    ));
    assert!(first.functions.iter().all(|function| {
        function.ownership_live.values().all(|storages| {
            storages
                .iter()
                .filter(|storage| matches!(storage, LogicalStorage::DirectChild(_)))
                .count()
                <= 1
        })
    }));
}
