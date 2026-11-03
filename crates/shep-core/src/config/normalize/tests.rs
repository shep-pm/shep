use super::{NormalizeError, normalize};
use crate::config::{AppConfig, MemoryEnforce};

fn kernel_enforced(max_memory: Option<&str>) -> AppConfig {
    let mut app = AppConfig::minimal("web", "./srv");
    app.memory_enforce = MemoryEnforce::Kernel;
    app.max_memory = max_memory.map(|limit| limit.parse().unwrap());
    app
}

#[test]
fn memory_enforce_defaults_to_poll() {
    // fails if the default drifts to kernel: every existing Flockfile would
    // start needing cgroups.
    assert_eq!(
        AppConfig::minimal("web", "./srv").memory_enforce,
        MemoryEnforce::Poll
    );
}

#[test]
fn memory_enforce_kernel_needs_max_memory() {
    // fails if kernel enforcement is accepted with nothing to write into
    // `memory.max`, which would start an unlimited sheep that reads as limited.
    let err = normalize(kernel_enforced(None)).unwrap_err();
    assert_eq!(
        err,
        NormalizeError::MemoryEnforceNeedsMaxMemory {
            name: "web".to_string()
        }
    );
    assert!(err.to_string().contains("max_memory"), "{err}");
}

#[test]
#[cfg(not(target_os = "linux"))]
fn memory_enforce_kernel_is_refused_off_linux() {
    // fails if macOS or Windows accepts a key only a cgroup can honour.
    let err = normalize(kernel_enforced(Some("512M"))).unwrap_err();
    assert_eq!(
        err,
        NormalizeError::MemoryEnforceKernelUnsupported {
            name: "web".to_string()
        }
    );
    assert!(err.to_string().contains("Linux"), "{err}");
}

#[test]
#[cfg(target_os = "linux")]
fn memory_enforce_kernel_is_accepted_on_linux() {
    // fails if the platform check refuses the one platform that supports it.
    normalize(kernel_enforced(Some("512M"))).expect("kernel enforcement on Linux");
}

#[test]
fn memory_enforce_poll_needs_no_max_memory() {
    // fails if the check fires on the default, refusing every app with no limit.
    normalize(AppConfig::minimal("web", "./srv")).expect("poll with no limit");
}
