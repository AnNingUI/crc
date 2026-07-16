use std::fs;
use std::path::Path;
use std::process::Command;

use crc_lib::Compiler;
use crc_lib::config::Config;
use crc_lib::runtime_abi::runtime_header;

const SOURCE: &str = r#"
#include <assert.h>
#include <stdlib.h>

typedef struct protocol_state {
    int mode;
    int polls;
} protocol_state;
static int protocol_drops;
static const cr_poll_context *observed_poll_context;
static cr_awaitable_vtable protocol_vtable;

static cr_poll_status protocol_poll(
    void *raw,
    const cr_poll_context *poll_context,
    void *out_value
) {
    observed_poll_context = poll_context;
    protocol_state *state = (protocol_state *)raw;
    state->polls++;
    if ((state->mode == 9 || state->mode == 10) && state->polls == 1) {
        *(int *)out_value = 41;
        return CR_POLL_YIELDED;
    }
    if (state->mode == 5) return 99u;
    if (state->mode == 6) return CR_POLL_ERROR;
    *(int *)out_value = 42;
    return CR_POLL_READY;
}

static const cr_error *protocol_error(const void *raw) {
    (void)raw;
    return NULL;
}

static void protocol_drop(void *raw) {
    protocol_drops++;
    free(raw);
}

static cr_awaitable protocol_operation(int mode) {
    protocol_state *state = (protocol_state *)malloc(sizeof(*state));
    assert(state != NULL);
    state->mode = mode;
    state->polls = 0;
    protocol_vtable = (cr_awaitable_vtable){
        CR_AWAITABLE_VTABLE_ABI_VERSION,
        sizeof(cr_awaitable_vtable),
        0u,
        0u,
        protocol_poll,
        protocol_error,
        protocol_drop,
        sizeof(int),
        _Alignof(int)
    };
    if (mode == 1) protocol_vtable.poll = NULL;
    if (mode == 2) protocol_vtable.value_size = sizeof(short);
    if (mode == 3) protocol_vtable.required_context_capabilities = UINT64_C(2);
    if (mode == 4) protocol_vtable.required_context_capabilities = CR_POLL_CAP_WAKER;
    if (mode == 7) return (cr_awaitable){state, NULL};
    if (mode == 8) protocol_vtable.struct_size = CR_AWAITABLE_VTABLE_V1_MIN_SIZE - 1u;
    if (mode == 10) protocol_vtable.provided_flags = CR_AWAITABLE_CAN_YIELD;
    if (mode == 11) protocol_vtable.struct_size = CR_AWAITABLE_VTABLE_DROP_PREFIX_SIZE - 1u;
    if (mode == 12) protocol_vtable.abi_version = 0u;
    if (mode == 13) protocol_vtable.drop = NULL;
    return (cr_awaitable){state, &protocol_vtable};
}

__async int protocol_parent(int mode) {
    return __await protocol_operation(mode);
}

static int binding_drops;
static int binding_order;
static void binding_child_cleanup(int value) {
    (void)value;
    binding_drops++;
    binding_order = binding_order * 10 + 2;
}
static void binding_parent_cleanup(int unused) {
    (void)unused;
    binding_order = binding_order * 10 + 1;
}

__async int binding_child(int value) {
    __defer binding_child_cleanup(value);
    return value;
}

__async int repeated_binding(void) {
    __defer binding_parent_cleanup(0);
    __async int task = binding_child(5);
    int first = __await task;
    int second = __await task;
    return first + second;
}

__async int never_awaited_binding(void) {
    __defer binding_parent_cleanup(0);
    __async int task = binding_child(7);
    return 0;
}

__async int reexecuted_binding(int iteration) {
again:
    __async int task = binding_child(iteration);
    if (iteration++ == 0) goto again;
    return __await task;
}

__async int inactive_binding(void) {
    goto skipped;
    __async int task = binding_child(9);
skipped:
    return __await task;
}

static void expect_error(int mode, int code, int expected_drops) {
    cr_protocol_parent_task task;
    cr_protocol_parent_init(&task, mode);
    assert(cr_protocol_parent_poll(&task, NULL) == CR_POLL_ERROR);
    assert(cr_protocol_parent_error(&task)->code == code);
    assert(protocol_drops == expected_drops);
    cr_poll_context malformed_context = {0u, 0u, 0u, NULL};
    assert(cr_protocol_parent_poll(&task, &malformed_context) == CR_POLL_ERROR);
    cr_protocol_parent_drop(&task);
    assert(protocol_drops == expected_drops);
}

static void expect_context_error(const cr_poll_context *poll_context, int expected_drops) {
    cr_protocol_parent_task task;
    cr_protocol_parent_init(&task, 0);
    assert(cr_protocol_parent_poll(&task, poll_context) == CR_POLL_ERROR);
    assert(cr_protocol_parent_error(&task)->code == CR_ERROR_INVALID_POLL_CONTEXT);
    assert(protocol_drops == expected_drops);
    assert(cr_protocol_parent_poll(&task, NULL) == CR_POLL_ERROR);
    cr_protocol_parent_drop(&task);
    assert(protocol_drops == expected_drops);
}

int main(void) {
    cr_poll_context malformed_context = {0u, 0u, 0u, NULL};
    cr_protocol_parent_task ready;
    cr_protocol_parent_init(&ready, 0);
    assert(cr_protocol_parent_poll(&ready, NULL) == CR_POLL_READY);
    assert(*cr_protocol_parent_result(&ready) == 42);
    assert(protocol_drops == 1);
    assert(cr_protocol_parent_poll(&ready, &malformed_context) == CR_POLL_READY);
    cr_protocol_parent_drop(&ready);

    expect_error(1, CR_ERROR_MISSING_AWAITABLE_CALLBACK, 2);
    expect_error(2, CR_ERROR_AWAITABLE_LAYOUT_MISMATCH, 3);
    expect_error(3, CR_ERROR_UNSUPPORTED_POLL_CAPABILITY, 4);
    expect_error(4, CR_ERROR_MISSING_POLL_CAPABILITY, 5);
    expect_error(5, CR_ERROR_INVALID_POLL_STATUS, 6);
    expect_error(6, CR_ERROR_MISSING_CHILD_ERROR, 7);
    expect_error(7, CR_ERROR_INVALID_AWAITABLE_ABI, 7);
    expect_error(8, CR_ERROR_INVALID_AWAITABLE_ABI, 8);
    expect_error(11, CR_ERROR_INVALID_AWAITABLE_ABI, 8);
    expect_error(12, CR_ERROR_INVALID_AWAITABLE_ABI, 8);
    expect_error(13, CR_ERROR_MISSING_AWAITABLE_CALLBACK, 8);

    cr_poll_context short_context = {
        CR_POLL_CONTEXT_ABI_VERSION,
        CR_POLL_CONTEXT_V1_MIN_SIZE - 1u,
        0u,
        NULL
    };
    cr_poll_context missing_waker = {
        CR_POLL_CONTEXT_ABI_VERSION,
        sizeof(cr_poll_context),
        CR_POLL_CAP_WAKER,
        NULL
    };
    expect_context_error(&malformed_context, 8);
    expect_context_error(&short_context, 8);
    expect_context_error(&missing_waker, 8);

    cr_poll_context mirrored_unknown = {
        CR_POLL_CONTEXT_ABI_VERSION,
        sizeof(cr_poll_context),
        UINT64_C(2),
        NULL
    };
    cr_protocol_parent_task unsupported;
    cr_protocol_parent_init(&unsupported, 3);
    assert(cr_protocol_parent_poll(&unsupported, &mirrored_unknown) == CR_POLL_ERROR);
    assert(cr_protocol_parent_error(&unsupported)->code == CR_ERROR_UNSUPPORTED_POLL_CAPABILITY);
    assert(protocol_drops == 9);
    cr_protocol_parent_drop(&unsupported);

    cr_poll_context valid_context = {
        CR_POLL_CONTEXT_ABI_VERSION,
        sizeof(cr_poll_context),
        0u,
        NULL
    };
    observed_poll_context = NULL;
    cr_protocol_parent_task propagated;
    cr_protocol_parent_init(&propagated, 0);
    assert(cr_protocol_parent_poll(&propagated, &valid_context) == CR_POLL_READY);
    assert(observed_poll_context == &valid_context);
    assert(protocol_drops == 10);
    cr_protocol_parent_drop(&propagated);

    cr_protocol_parent_task yield_without_flag;
    cr_protocol_parent_init(&yield_without_flag, 9);
    assert(cr_protocol_parent_poll(&yield_without_flag, NULL) == CR_POLL_YIELDED);
    assert(*cr_protocol_parent_yielded(&yield_without_flag) == 41);
    assert(protocol_drops == 10);
    assert(cr_protocol_parent_poll(&yield_without_flag, NULL) == CR_POLL_READY);
    assert(*cr_protocol_parent_result(&yield_without_flag) == 42);
    assert(protocol_drops == 11);
    cr_protocol_parent_drop(&yield_without_flag);

    cr_protocol_parent_task yield_with_flag;
    cr_protocol_parent_init(&yield_with_flag, 10);
    assert(cr_protocol_parent_poll(&yield_with_flag, NULL) == CR_POLL_YIELDED);
    assert(*cr_protocol_parent_yielded(&yield_with_flag) == 41);
    assert(protocol_drops == 11);
    assert(cr_protocol_parent_poll(&yield_with_flag, NULL) == CR_POLL_READY);
    assert(*cr_protocol_parent_result(&yield_with_flag) == 42);
    assert(protocol_drops == 12);
    cr_protocol_parent_drop(&yield_with_flag);

    binding_drops = 0;
    binding_order = 0;
    cr_repeated_binding_task repeated;
    cr_repeated_binding_init(&repeated);
    assert(cr_repeated_binding_poll(&repeated, NULL) == CR_POLL_READY);
    assert(*cr_repeated_binding_result(&repeated) == 10);
    assert(binding_drops == 1);
    assert(binding_order == 21);
    cr_repeated_binding_drop(&repeated);
    assert(binding_drops == 1);

    binding_order = 0;
    cr_never_awaited_binding_task never_awaited;
    cr_never_awaited_binding_init(&never_awaited);
    assert(cr_never_awaited_binding_poll(&never_awaited, NULL) == CR_POLL_READY);
    assert(binding_drops == 1);
    assert(binding_order == 1);
    assert(!never_awaited.cr_v_5_task_active);
    cr_never_awaited_binding_drop(&never_awaited);

    cr_reexecuted_binding_task reexecuted;
    cr_reexecuted_binding_init(&reexecuted, 0);
    assert(cr_reexecuted_binding_poll(&reexecuted, NULL) == CR_POLL_READY);
    assert(*cr_reexecuted_binding_result(&reexecuted) == 1);
    assert(binding_drops == 2);
    assert(!reexecuted.cr_v_6_task_active);
    cr_reexecuted_binding_drop(&reexecuted);

    cr_inactive_binding_task inactive;
    cr_inactive_binding_init(&inactive);
    assert(cr_inactive_binding_poll(&inactive, NULL) == CR_POLL_ERROR);
    assert(cr_inactive_binding_error(&inactive)->code == CR_ERROR_INACTIVE_TASK_BINDING);
    assert(binding_drops == 2);
    cr_inactive_binding_drop(&inactive);
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
        .expect("Clang or GCC is required for ABI protocol tests")
}

#[test]
fn malformed_v3_objects_fail_with_sticky_protocol_errors() {
    let generated = Compiler::new(Config::default())
        .compile_source(SOURCE, Path::new("abi-v3-protocol.cr"))
        .expect("protocol source compiles");
    let directory = tempfile::tempdir().expect("temporary directory");
    fs::write(directory.path().join("cr_runtime.h"), runtime_header())
        .expect("runtime header is written");
    fs::write(directory.path().join("protocol.c"), generated)
        .expect("generated protocol source is written");
    let executable = if cfg!(windows) {
        "protocol.exe"
    } else {
        "protocol"
    };
    let compilation = Command::new(available_compiler())
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Werror",
            "protocol.c",
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
        .expect("protocol executable runs");
    assert!(
        execution.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&execution.stdout),
        String::from_utf8_lossy(&execution.stderr)
    );
}
