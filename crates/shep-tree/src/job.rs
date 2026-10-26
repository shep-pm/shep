//! A Windows job object: the substitute for a unix process group.
//!
//! Windows has no process groups, so where unix spawns a child with
//! `process_group(0)`, here the child is assigned to a job object: a kernel
//! container its children inherit and which terminates as a unit. A member
//! cannot escape a job created without `JOB_OBJECT_LIMIT_BREAKAWAY_OK`, so
//! [`Job::terminate`] reaches more of the tree here than a group kill does on
//! unix, where a `setsid` grandchild escapes.
//!
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` is not set: a caller that holds the
//! handle would take every member down with itself. On unix a child outlives
//! a killed parent, and the platforms stay aligned.

#![allow(unsafe_code)] // every unsafe site in this crate is in this file

use std::io;
use std::os::windows::io::RawHandle;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};

/// An anonymous job object owning a process and everything it spawns.
///
/// Closed on drop. Dropping does not terminate the members: this module's
/// doc covers why `KILL_ON_JOB_CLOSE` is not set.
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
    /// Creates an anonymous job object.
    ///
    /// Anonymous (a null name): a named job is visible in the kernel object
    /// namespace, where another process could open it and terminate the
    /// members.
    ///
    /// # Errors
    ///
    /// Whatever the OS says if the job cannot be created or its limits set.
    pub fn create() -> io::Result<Self> {
        // SAFETY: both arguments are the documented "no security attributes,
        // no name" nulls. `CreateJobObjectW` returns a null handle on
        // failure, which is checked immediately below before the value is
        // stored or used.
        let handle = unsafe { CreateJobObjectW(core::ptr::null(), core::ptr::null()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let job = Self(handle);

        // Zeroed, then left zeroed: every limit flag stays off. Setting the
        // block explicitly makes adding a limit later a one-line edit at a
        // site that already handles its own errors.
        //
        // `JOB_OBJECT_LIMIT_BREAKAWAY_OK` and
        // `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK` are the two flags whose
        // absence is load-bearing: without them a member cannot escape the
        // job, which is what makes `terminate` complete.
        // SAFETY: `JOBOBJECT_EXTENDED_LIMIT_INFORMATION` is a plain Win32 POD
        // struct holding no reference, no `NonNull` and no range-restricted
        // enum, so all-zeroes is a valid inhabitant, and is the value wanted:
        // every limit flag off.
        let info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { core::mem::zeroed() };
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
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::*;

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
    ///
    /// The child would otherwise outrun the test, so an exit can only be the
    /// termination.
    #[test]
    fn a_child_assigned_to_a_job_is_killed_by_terminate() {
        let job = Job::create().unwrap();
        // `ping`, not `timeout /t`: `timeout.exe` refuses a non-console stdin
        // and exits in milliseconds, so it would assert nothing here.
        let mut child = Command::new("cmd")
            .args(["/C", "ping", "-n", "600", "127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .spawn()
            .expect("cmd must be spawnable on Windows");

        job.assign(child.as_raw_handle())
            .expect("assignment must succeed");

        job.terminate(9).unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "a terminated job member must not outlive its job"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            !status.success(),
            "a job-terminated child must not report success"
        );
    }
}
