use std::fs;
use std::process::Command;

use crc_lib::runtime_abi::{CR_RUNTIME_ABI_VERSION, runtime_header};

const LAYOUT_SOURCE: &str = r#"
#include "cr_runtime.h"

_Static_assert(CR_RUNTIME_ABI_VERSION == 3u, "runtime ABI version");
_Static_assert(sizeof(cr_poll_status) == sizeof(uint32_t), "poll width");
_Static_assert(sizeof(((cr_error *)0)->code) == sizeof(int32_t), "error width");
_Static_assert(
    sizeof(cr_awaitable) == 2u * sizeof(void *),
    "awaitable must contain two machine words"
);
_Static_assert(
    CR_POLL_CONTEXT_V1_MIN_SIZE == sizeof(cr_poll_context),
    "poll context v1 prefix"
);
_Static_assert(
    CR_AWAITABLE_VTABLE_V1_MIN_SIZE == sizeof(cr_awaitable_vtable),
    "awaitable vtable v1 prefix"
);
_Static_assert(
    offsetof(cr_awaitable, vtable) == sizeof(void *),
    "awaitable vtable offset"
);
_Static_assert(
    (CR_POLL_KNOWN_CAPABILITIES & CR_POLL_CAP_WAKER) != 0u,
    "known waker capability"
);

int main(void) { return 0; }
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
        .expect("Clang or GCC is required for ABI layout tests")
}

#[test]
fn public_v3_layout_is_valid_c11() {
    assert_eq!(CR_RUNTIME_ABI_VERSION, 3);
    let directory = tempfile::tempdir().expect("temporary directory");
    fs::write(directory.path().join("cr_runtime.h"), runtime_header())
        .expect("runtime header is written");
    fs::write(directory.path().join("layout.c"), LAYOUT_SOURCE).expect("layout source is written");
    let executable = if cfg!(windows) {
        "layout.exe"
    } else {
        "layout"
    };
    let output = Command::new(available_compiler())
        .arg("-std=c11")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("layout.c")
        .arg("-o")
        .arg(executable)
        .current_dir(directory.path())
        .output()
        .expect("native compiler runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
