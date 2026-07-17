//! Experimental Stage 6 backend runtime artifacts.

mod common;
mod epoll;
mod iocp;
mod kqueue;
mod memory;
mod net;

use crate::config::TargetConfig;

/// Compiler-owned experimental backend artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendArtifact {
    pub path: &'static str,
    pub contents: &'static str,
    pub kind: &'static str,
    pub is_source: bool,
}

/// Returns the common backend headers and owner-driven core source.
#[must_use]
pub fn common_artifacts() -> Vec<BackendArtifact> {
    vec![
        BackendArtifact {
            path: "include/cr_backend.h",
            contents: crate::backend_abi::backend_header(),
            kind: "backend-header",
            is_source: false,
        },
        BackendArtifact {
            path: "include/cr_net.h",
            contents: crate::backend_abi::net_header(),
            kind: "backend-header",
            is_source: false,
        },
        BackendArtifact {
            path: "runtime/cr_backend_internal.h",
            contents: common::INTERNAL_HEADER,
            kind: "backend-internal",
            is_source: false,
        },
        BackendArtifact {
            path: "runtime/cr_backend_common.c",
            contents: common::COMMON_SOURCE,
            kind: "backend-source",
            is_source: true,
        },
    ]
}

/// Returns the complete portable memory-conformance provider artifact set.
#[must_use]
pub fn memory_artifacts() -> Vec<BackendArtifact> {
    let mut artifacts = common_artifacts();
    artifacts.push(BackendArtifact {
        path: "runtime/cr_backend_memory.c",
        contents: memory::MEMORY_SOURCE,
        kind: "backend-source",
        is_source: true,
    });
    artifacts
}

/// Returns the experimental reference net-receive awaitable source artifact.
#[must_use]
pub fn net_awaitable_artifact() -> BackendArtifact {
    BackendArtifact {
        path: "runtime/cr_net_recv.c",
        contents: net::NET_RECEIVE_SOURCE,
        kind: "backend-awaitable-source",
        is_source: true,
    }
}

/// Returns the portable memory provider plus the reference receive awaitable.
#[must_use]
pub fn memory_net_awaitable_artifacts() -> Vec<BackendArtifact> {
    let mut artifacts = memory_artifacts();
    artifacts.push(net_awaitable_artifact());
    artifacts
}

/// Returns the Windows IOCP provider and its shared experimental core.
#[must_use]
pub fn iocp_artifacts() -> Vec<BackendArtifact> {
    let mut artifacts = common_artifacts();
    artifacts.push(BackendArtifact {
        path: "runtime/cr_backend_iocp.c",
        contents: iocp::IOCP_SOURCE,
        kind: "backend-source",
        is_source: true,
    });
    artifacts
}

/// Returns the Windows IOCP provider plus the reference receive awaitable.
#[must_use]
pub fn iocp_net_awaitable_artifacts() -> Vec<BackendArtifact> {
    let mut artifacts = iocp_artifacts();
    artifacts.push(net_awaitable_artifact());
    artifacts
}

/// Returns the Linux epoll provider and its shared experimental core.
#[must_use]
pub fn epoll_artifacts() -> Vec<BackendArtifact> {
    let mut artifacts = common_artifacts();
    artifacts.push(BackendArtifact {
        path: "runtime/cr_backend_epoll.c",
        contents: epoll::EPOLL_SOURCE,
        kind: "backend-source",
        is_source: true,
    });
    artifacts
}

/// Returns the Linux epoll provider plus the reference receive awaitable.
#[must_use]
pub fn epoll_net_awaitable_artifacts() -> Vec<BackendArtifact> {
    let mut artifacts = epoll_artifacts();
    artifacts.push(net_awaitable_artifact());
    artifacts
}

/// Returns the macOS kqueue provider and its shared experimental core.
#[must_use]
pub fn kqueue_artifacts() -> Vec<BackendArtifact> {
    let mut artifacts = common_artifacts();
    artifacts.push(BackendArtifact {
        path: "runtime/cr_backend_kqueue.c",
        contents: kqueue::KQUEUE_SOURCE,
        kind: "backend-source",
        is_source: true,
    });
    artifacts
}

/// Returns the macOS kqueue provider plus the reference receive awaitable.
#[must_use]
pub fn kqueue_net_awaitable_artifacts() -> Vec<BackendArtifact> {
    let mut artifacts = kqueue_artifacts();
    artifacts.push(net_awaitable_artifact());
    artifacts
}

/// Resolves the native-net artifact family implemented for `target` so far.
///
/// Stage 6 keeps this query separate from publication until all native models
/// have validated the shared prefix. `None` means that the target's reference
/// provider belongs to a later Stage 6 task.
#[must_use]
pub fn native_net_artifacts_for_target(target: &TargetConfig) -> Option<Vec<BackendArtifact>> {
    match target {
        TargetConfig::WindowsMsvc | TargetConfig::WindowsGnu => Some(iocp_artifacts()),
        TargetConfig::LinuxGnu | TargetConfig::LinuxMusl => Some(epoll_artifacts()),
        TargetConfig::Macos => Some(kqueue_artifacts()),
        TargetConfig::Host if cfg!(windows) => Some(iocp_artifacts()),
        TargetConfig::Host if cfg!(target_os = "linux") => Some(epoll_artifacts()),
        TargetConfig::Host if cfg!(target_os = "macos") => Some(kqueue_artifacts()),
        TargetConfig::Host | TargetConfig::Wasm32Wasi | TargetConfig::Custom(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_artifacts_are_complete_portable_and_not_published_implicitly() {
        let artifacts = memory_artifacts();
        assert_eq!(
            artifacts
                .iter()
                .map(|artifact| artifact.path)
                .collect::<Vec<_>>(),
            vec![
                "include/cr_backend.h",
                "include/cr_net.h",
                "runtime/cr_backend_internal.h",
                "runtime/cr_backend_common.c",
                "runtime/cr_backend_memory.c",
            ]
        );
        let sources = artifacts
            .iter()
            .filter(|artifact| artifact.is_source)
            .map(|artifact| artifact.contents)
            .collect::<String>()
            .to_ascii_lowercase();
        assert!(sources.contains("stdatomic.h"));
        for forbidden in [
            "windows.h",
            "winsock2.h",
            "sys/epoll.h",
            "sys/event.h",
            "pthread_",
            "createthread",
            "thrd_create",
        ] {
            assert!(!sources.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn reference_awaitable_keeps_provider_operation_storage_indirect() {
        let artifacts = memory_net_awaitable_artifacts();
        assert_eq!(
            artifacts.last().map(|artifact| artifact.path),
            Some("runtime/cr_net_recv.c")
        );
        let source = net_awaitable_artifact().contents;
        assert!(source.contains("cr_net_receive_operation *operation;"));
        assert!(!source.contains("cr_net_receive_operation operation;"));
        assert!(!source.contains("cr_net_receive_operation operation["));
    }

    #[test]
    fn iocp_is_selected_only_for_windows_targets() {
        for target in [TargetConfig::WindowsMsvc, TargetConfig::WindowsGnu] {
            let artifacts =
                native_net_artifacts_for_target(&target).expect("Windows IOCP artifacts");
            assert!(
                artifacts
                    .iter()
                    .any(|artifact| { artifact.path == "runtime/cr_backend_iocp.c" })
            );
        }
        for target in [
            TargetConfig::Macos,
            TargetConfig::Wasm32Wasi,
            TargetConfig::Custom("portable-vendor".to_owned()),
        ] {
            let artifacts = native_net_artifacts_for_target(&target);
            assert!(artifacts.as_ref().is_none_or(|artifacts| {
                artifacts
                    .iter()
                    .all(|artifact| artifact.path != "runtime/cr_backend_iocp.c")
            }));
        }

        let portable = memory_artifacts()
            .into_iter()
            .map(|artifact| artifact.contents)
            .collect::<String>()
            .to_ascii_lowercase();
        assert!(!portable.contains("winsock2.h"));
        assert!(!portable.contains("windows.h"));
        assert!(!portable.contains("createiocompletionport"));
    }

    #[test]
    fn epoll_is_selected_only_for_linux_targets() {
        for target in [TargetConfig::LinuxGnu, TargetConfig::LinuxMusl] {
            let artifacts =
                native_net_artifacts_for_target(&target).expect("Linux epoll artifacts");
            assert!(
                artifacts
                    .iter()
                    .any(|artifact| artifact.path == "runtime/cr_backend_epoll.c")
            );
            assert!(
                artifacts
                    .iter()
                    .all(|artifact| artifact.path != "runtime/cr_backend_iocp.c")
            );
        }
        for target in [
            TargetConfig::WindowsMsvc,
            TargetConfig::WindowsGnu,
            TargetConfig::Macos,
            TargetConfig::Wasm32Wasi,
            TargetConfig::Custom("portable-vendor".to_owned()),
        ] {
            let artifacts = native_net_artifacts_for_target(&target);
            assert!(artifacts.as_ref().is_none_or(|artifacts| {
                artifacts
                    .iter()
                    .all(|artifact| artifact.path != "runtime/cr_backend_epoll.c")
            }));
        }
    }

    #[test]
    fn kqueue_is_selected_only_for_macos_targets() {
        let artifacts =
            native_net_artifacts_for_target(&TargetConfig::Macos).expect("macOS kqueue artifacts");
        assert!(
            artifacts
                .iter()
                .any(|artifact| artifact.path == "runtime/cr_backend_kqueue.c")
        );
        assert!(artifacts.iter().all(|artifact| {
            artifact.path != "runtime/cr_backend_iocp.c"
                && artifact.path != "runtime/cr_backend_epoll.c"
        }));
        for target in [
            TargetConfig::WindowsMsvc,
            TargetConfig::WindowsGnu,
            TargetConfig::LinuxGnu,
            TargetConfig::LinuxMusl,
            TargetConfig::Wasm32Wasi,
            TargetConfig::Custom("portable-vendor".to_owned()),
        ] {
            let artifacts = native_net_artifacts_for_target(&target);
            assert!(artifacts.as_ref().is_none_or(|artifacts| {
                artifacts
                    .iter()
                    .all(|artifact| artifact.path != "runtime/cr_backend_kqueue.c")
            }));
        }
    }
}
