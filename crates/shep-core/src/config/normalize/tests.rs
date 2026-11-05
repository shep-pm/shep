use super::{NormalizeError, normalize};
use crate::config::{AppConfig, CpuCores, MemoryEnforce};

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

fn capped_cpu(cores: f64) -> AppConfig {
    let mut app = AppConfig::minimal("web", "./srv");
    app.max_cpu_cores = Some(CpuCores::from_cores(cores));
    app
}

#[test]
fn max_cpu_cores_defaults_to_unset() {
    // fails if the default drifts to a cap: every existing Flockfile would
    // start throttled, or refused off Linux.
    assert_eq!(AppConfig::minimal("web", "./srv").max_cpu_cores, None);
}

#[test]
fn max_cpu_cores_must_be_positive() {
    // fails if a quota of nothing is accepted: `0 100000` in `cpu.max` stalls
    // the whole tree, and a negative or NaN value is no quota at all.
    for cores in [0.0, -0.5, f64::NAN, f64::INFINITY] {
        let err = normalize(capped_cpu(cores)).unwrap_err();
        assert_eq!(
            err,
            NormalizeError::MaxCpuCoresOutOfRange {
                name: "web".to_string()
            },
            "{cores}"
        );
        assert!(err.to_string().contains("max_cpu_cores"), "{err}");
    }
}

#[test]
fn max_cpu_cores_below_the_kernel_minimum_is_refused() {
    // fails if 0.005 is accepted: `cpu.max` refuses a quota under 1000 us, so
    // a live change would report applied while the old quota stayed in force.
    for cores in [0.005, 0.000_001] {
        assert_eq!(
            normalize(capped_cpu(cores)).unwrap_err(),
            NormalizeError::MaxCpuCoresOutOfRange {
                name: "web".to_string()
            },
            "{cores}"
        );
    }
}

#[test]
#[cfg(not(target_os = "linux"))]
fn max_cpu_cores_is_refused_on_macos_and_windows() {
    // fails if macOS or Windows accepts a quota only a cgroup can honour.
    let err = normalize(capped_cpu(1.5)).unwrap_err();
    assert_eq!(
        err,
        NormalizeError::MaxCpuCoresUnsupported {
            name: "web".to_string()
        }
    );
    assert!(err.to_string().contains("Linux"), "{err}");
}

#[test]
#[cfg(target_os = "linux")]
fn max_cpu_cores_is_accepted_on_linux() {
    // fails if the platform check refuses the one platform that supports it.
    normalize(capped_cpu(1.5)).expect("a CPU quota on Linux");
}

fn task_capped(tasks: u32) -> AppConfig {
    let mut app = AppConfig::minimal("web", "./srv");
    app.max_tasks = Some(tasks);
    app
}

#[test]
fn max_tasks_defaults_to_unset() {
    // fails if the default drifts to a cap: every existing Flockfile would
    // start limited, or refused off Linux.
    assert_eq!(AppConfig::minimal("web", "./srv").max_tasks, None);
}

#[test]
fn max_tasks_must_be_positive() {
    // fails if a limit of zero is accepted: `pids.max` of 0 lets the sheep
    // run nothing, not even the process the shepherd just started.
    let err = normalize(task_capped(0)).unwrap_err();
    assert_eq!(
        err,
        NormalizeError::MaxTasksOutOfRange {
            name: "web".to_string()
        }
    );
    assert!(err.to_string().contains("max_tasks"), "{err}");
}

#[test]
fn max_tasks_above_what_pids_max_can_hold_is_refused() {
    // fails if a value the kernel rejects with EINVAL passes `normalize`: a
    // live change would report applied while the old limit stayed in force.
    for tasks in [4_194_305, u32::MAX] {
        assert_eq!(
            normalize(task_capped(tasks)).unwrap_err(),
            NormalizeError::MaxTasksOutOfRange {
                name: "web".to_string()
            },
            "{tasks}"
        );
    }
}

#[test]
#[cfg(not(target_os = "linux"))]
fn max_tasks_is_refused_off_linux() {
    // fails if macOS or Windows accepts a limit only a cgroup can honour.
    let err = normalize(task_capped(64)).unwrap_err();
    assert_eq!(
        err,
        NormalizeError::MaxTasksUnsupported {
            name: "web".to_string()
        }
    );
    assert!(err.to_string().contains("Linux"), "{err}");
}

#[test]
#[cfg(target_os = "linux")]
fn max_tasks_is_accepted_on_linux() {
    // fails if the platform check refuses the one platform that supports it,
    // or the ceiling is set one too low.
    normalize(task_capped(1)).expect("the smallest limit on Linux");
    normalize(task_capped(4_194_304)).expect("the largest limit on Linux");
}

#[test]
#[cfg(unix)]
fn pty_is_accepted_on_unix() {
    // fails if the platform check refuses the platforms that can open a
    // pseudo-terminal.
    let mut app = AppConfig::minimal("web", "./srv");
    app.pty = true;
    normalize(app).expect("a pty on Unix");
}

#[test]
#[cfg(windows)]
fn pty_is_refused_on_windows_by_name() {
    // fails if Windows accepts a key nothing there can honour, or refuses it
    // without saying which key.
    let mut app = AppConfig::minimal("web", "./srv");
    app.pty = true;
    let err = normalize(app).unwrap_err();
    assert_eq!(
        err,
        NormalizeError::PtyUnsupported {
            name: "web".to_string()
        }
    );
    assert!(err.to_string().contains("pty"), "{err}");
}
