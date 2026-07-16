use std::collections::BTreeMap;
use std::fs;
use std::process::Command;

use crc_lib::config::TargetConfig;
use crc_lib::runtime_abi::runtime_header;
use crc_lib::target_layout::{LayoutKnowledge, TargetLayoutModel, TypeLayout};

fn available_compiler() -> &'static str {
    ["clang", "gcc"]
        .into_iter()
        .find(|compiler| {
            Command::new(compiler)
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        })
        .expect("Clang or GCC is required for target-layout tests")
}

fn exact_type(model: &TargetLayoutModel, c_type: &str) -> TypeLayout {
    model
        .type_layout(c_type, &BTreeMap::new())
        .exact()
        .copied()
        .unwrap_or_else(|| panic!("exact layout for {c_type}"))
}

#[test]
fn host_model_matches_c_size_alignment_and_offsets() {
    let model = match TargetLayoutModel::for_target(&TargetConfig::Host) {
        LayoutKnowledge::Exact(model) => model,
        LayoutKnowledge::Unknown(reason) => panic!("host model is unknown: {reason:?}"),
    };
    let field_types = [
        "uint8_t",
        "long",
        "void *",
        "uint16_t[3][4]",
        "int (*)(void)",
        "cr_error",
        "cr_cleanup_stack",
        "cr_awaitable",
    ];
    let fields: Vec<_> = field_types
        .iter()
        .map(|c_type| exact_type(&model, c_type))
        .collect();
    let aggregate = model
        .struct_layout(fields)
        .exact()
        .cloned()
        .expect("exact aggregate layout");
    let union = model
        .union_layout([
            exact_type(&model, "uint16_t[3][4]"),
            exact_type(&model, "void *"),
        ])
        .exact()
        .copied()
        .expect("exact union layout");

    let directory = tempfile::tempdir().expect("temporary directory");
    fs::write(directory.path().join("cr_runtime.h"), runtime_header()).expect("runtime header");
    fs::write(
        directory.path().join("layout-model.c"),
        r#"#include "cr_runtime.h"
#include <stddef.h>
#include <stdio.h>

typedef int (*probe_callback)(void);
typedef struct layout_probe {
    uint8_t byte;
    long count;
    void *pointer;
    uint16_t samples[3][4];
    probe_callback callback;
    cr_error error;
    cr_cleanup_stack cleanups;
    cr_awaitable awaitable;
} layout_probe;

typedef union layout_union {
    uint16_t samples[3][4];
    void *pointer;
} layout_union;

int main(void) {
    printf(
        "%zu %zu %zu %zu %zu %zu %zu %zu %zu %zu %zu %zu\n",
        sizeof(layout_probe),
        _Alignof(layout_probe),
        offsetof(layout_probe, byte),
        offsetof(layout_probe, count),
        offsetof(layout_probe, pointer),
        offsetof(layout_probe, samples),
        offsetof(layout_probe, callback),
        offsetof(layout_probe, error),
        offsetof(layout_probe, cleanups),
        offsetof(layout_probe, awaitable),
        sizeof(layout_union),
        _Alignof(layout_union)
    );
    return 0;
}
"#,
    )
    .expect("layout probe source");
    let executable = if cfg!(windows) {
        "layout-model.exe"
    } else {
        "layout-model"
    };
    let compilation = Command::new(available_compiler())
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "layout-model.c",
            "-o",
        ])
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
        .expect("layout probe runs");
    assert!(execution.status.success());
    let actual: Vec<u64> = String::from_utf8(execution.stdout)
        .expect("probe output is UTF-8")
        .split_whitespace()
        .map(|value| value.parse().expect("numeric probe output"))
        .collect();
    let mut expected = vec![aggregate.size, aggregate.align];
    expected.extend(aggregate.offsets);
    expected.extend([union.size, union.align]);
    assert_eq!(actual, expected);
}
