//! A Windows job object: the substitute for a unix process group.
//!
//! Windows has no process groups, so where unix spawns a child with
//! `process_group(0)`, here the child is assigned to a job object: a kernel
//! container its children inherit and which terminates as a unit. A member
//! cannot escape a job created without `JOB_OBJECT_LIMIT_BREAKAWAY_OK`, so
//! [`Job::terminate`] reaches a `setsid` grandchild a unix group kill misses.
//!
//! [`Job::create`] leaves `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` off: a caller
//! that holds the handle would take every member down with itself, and on unix
//! a child outlives a killed parent. [`Job::create_kill_on_close`] sets it, so
//! the kernel ends the members with the caller.

#![allow(unsafe_code)] // every unsafe site in this crate is in this file

use std::io;
use std::os::windows::io::RawHandle;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};

/// An anonymous job object owning a process and everything it spawns.
///
/// Closed on drop. A job from [`Job::create`] leaves its members running; one
/// from [`Job::create_kill_on_close`] terminates them once the last handle to
/// it closes, a handle a member inherited or duplicated included.
#[derive(Debug)]
pub struct Job(HANDLE);

// SAFETY: a job object HANDLE is plain data, not a thread-affine resource;
// Win32 permits any thread to call `AssignProcessToJobObject`,
// `TerminateJobObject` or `CloseHandle` on it. A `Job` may move to the thread
// that owns the process it holds.
unsafe impl Send for Job {}
// SAFETY: every Win32 call this module makes on the handle is itself
// thread-safe, and `Job` exposes no interior mutability.
unsafe impl Sync for Job {}

impl Job {
    /// Creates an anonymous job object with no limit flag set.
    ///
    /// Anonymous (a null name): a named job is visible in the kernel object
    /// namespace, where another process could open it and terminate the
    /// members.
    ///
    /// # Errors
    ///
    /// Whatever the OS says if the job cannot be created or its limits set.
    pub fn create() -> io::Result<Self> {
        Self::create_with(0)
    }

    /// Creates an anonymous job object that ends its members when the last
    /// handle to it closes.
    ///
    /// [`Job::create`] with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` set: the
    /// kernel terminates every member once no handle to the job is left open,
    /// a handle a member inherited or duplicated included, and a crash of the
    /// creator closes the handle it held.
    ///
    /// # Errors
    ///
    /// Whatever the OS says if the job cannot be created or its limits set.
    pub fn create_kill_on_close() -> io::Result<Self> {
        Self::create_with(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE)
    }

    /// Creates an anonymous job object with `limit_flags` set and every other
    /// limit off.
    ///
    /// `JOB_OBJECT_LIMIT_BREAKAWAY_OK` and `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK`
    /// are set by neither constructor: without them a member cannot escape the
    /// job, which is what makes [`Job::terminate`] complete.
    fn create_with(limit_flags: JOB_OBJECT_LIMIT) -> io::Result<Self> {
        // SAFETY: both arguments are the documented "no security attributes,
        // no name" nulls. `CreateJobObjectW` returns a null handle on
        // failure, which is checked immediately below before the value is
        // stored or used.
        let handle = unsafe { CreateJobObjectW(core::ptr::null(), core::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);

        // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a plain Win32 POD
        // struct holding no reference, no `NonNull` and no range-restricted
        // enum, so all-zeroes is a valid inhabitant; `limit_flags` is a
        // `JOB_OBJECT_LIMIT`, the plain u32 alias the field takes.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { core::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = limit_flags;
        // SAFETY: `job.0` is a live job handle, checked non-null above.
        // `JobObjectExtendedLimitInformation` is the class that pairs with
        // `JOBOBJECT_EXTENDED_LIMIT_INFORMATION`, and the pointer and length
        // describe a stack local that outlives the call.
        let ok = unsafe {
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                core::ptr::from_ref(&info).cast(),
                u32::try_from(core::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .expect("the size of a fixed Win32 struct fits in u32"),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Assigns an already-spawned process to this job.
    ///
    /// Every process the assigned one creates is a member too, and that
    /// inheritance is what makes this the process-group substitute. A child
    /// that forks a grandchild between the spawn and the assignment leaves it
    /// outside the job; closing that window needs `CREATE_SUSPENDED`, which
    /// `std`'s `Command` cannot express.
    ///
    /// `process` is borrowed, not consumed: it must be a live process handle
    /// for the duration of the call, such as
    /// [`AsRawHandle::as_raw_handle`](std::os::windows::io::AsRawHandle::as_raw_handle)
    /// on a [`std::process::Child`].
    ///
    /// # Errors
    ///
    /// Whatever the OS says. `ERROR_ACCESS_DENIED` usually means the calling
    /// process itself runs in a job that does not permit breakaway.
    pub fn assign(&self, process: RawHandle) -> io::Result<()> {
        // SAFETY: `self.0` is a live job handle for as long as `self` lives,
        // and `process` is the caller's live child-process handle, borrowed
        // from a `Child` that outlives this call. Neither handle is consumed.
        let ok = unsafe { AssignProcessToJobObject(self.0, process as HANDLE) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Terminates every process in the job.
    ///
    /// Unblockable, like the `SIGKILL` it stands in for. `exit_code` becomes
    /// the observed exit code of every member.
    ///
    /// # Errors
    ///
    /// Whatever the OS says. A job whose members have all exited is not an
    /// error: this succeeds and does nothing.
    pub fn terminate(&self, exit_code: u32) -> io::Result<()> {
        // SAFETY: `self.0` is a live job handle for as long as `self` lives.
        let ok = unsafe { TerminateJobObject(self.0, exit_code) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // SAFETY: `self.0` was returned non-null by `CreateJobObjectW` and
        // nothing else in this module closes it; `Job` is neither `Copy` nor
        // `Clone`, so this runs exactly once. A failing `CloseHandle` in a
        // destructor has no caller to report to.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::io::AsRawHandle as _;
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

    /// A child that runs for ten minutes, so an exit in a test can only be the
    /// job's doing.
    ///
    /// `ping`, not `timeout /t`: `timeout.exe` refuses a non-console stdin and
    /// exits in milliseconds, so it would assert nothing here.
    fn spawn_a_long_child() -> Child {
        Command::new("cmd")
            .args(["/C", "ping", "-n", "600", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .expect("cmd must be spawnable on Windows")
    }

    /// The status `child` exited with, or a failed assertion once `patience`
    /// runs out.
    fn exit_within(child: &mut Child, patience: Duration) -> ExitStatus {
        let deadline = Instant::now() + patience;
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "the child outlived the {patience:?} the test allows"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Whether `child` is still running for the whole of `window`.
    fn still_running(child: &mut Child, window: Duration) -> bool {
        let deadline = Instant::now() + window;
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// fails if a job cannot be created and terminated on this machine.
    ///
    /// An empty job is the case worth pinning: a kill runs against a child
    /// that may already have exited, so no members must read as success.
    #[test]
    fn an_empty_job_is_created_and_terminated_without_error() {
        let job = Job::create().expect("a job object must be creatable");
        job.terminate(1)
            .expect("terminating a job with no members must succeed");
    }

    /// fails if a real child assigned to a job survives `terminate`.
    #[test]
    fn a_child_assigned_to_a_job_is_killed_by_terminate() {
        let job = Job::create().unwrap();
        let mut child = spawn_a_long_child();

        job.assign(child.as_raw_handle())
            .expect("assignment must succeed");

        job.terminate(9).unwrap();

        let status = exit_within(&mut child, Duration::from_secs(10));
        assert!(
            !status.success(),
            "a job-terminated child must not report success"
        );
    }

    /// fails if a kill-on-close job leaves its child running once the last
    /// handle to the job closes.
    #[test]
    fn a_kill_on_close_job_ends_its_child_when_dropped() {
        let job = Job::create_kill_on_close().expect("a kill-on-close job must be creatable");
        let mut child = spawn_a_long_child();

        job.assign(child.as_raw_handle())
            .expect("assignment must succeed");

        drop(job);

        // The kernel picks this exit code, not the caller as `terminate` does,
        // so the exit itself is the verdict.
        let _ = exit_within(&mut child, Duration::from_secs(10));
    }

    /// fails if a job from [`Job::create`] takes its child down with its handle.
    #[test]
    fn a_default_job_leaves_its_child_running_when_dropped() {
        let job = Job::create().unwrap();
        let mut child = spawn_a_long_child();

        job.assign(child.as_raw_handle())
            .expect("assignment must succeed");

        drop(job);

        assert!(
            still_running(&mut child, Duration::from_millis(500)),
            "a default job member must outlive the job handle"
        );
        child.kill().expect("the child must be killable");
        child.wait().expect("the child must be waitable");
    }
}
