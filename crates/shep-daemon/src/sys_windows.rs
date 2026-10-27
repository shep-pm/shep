//! The Windows counterpart to `sys`: the daemon's own Win32 calls
//!
//! The job object that stands in for a unix process group is `shep-tree`'s
//! `Job`, not this file's: a sheep's containment is the same code a dog or a
//! build script uses.
//!
//! What stays is what only the shepherd needs: sealing its own stdio handles
//! against inheritance ([`seal_std_handles`]) and the security descriptor
//! behind its own pipes, [`UserOnly`].

#![allow(unsafe_code)] // every unsafe site on this platform is in this file

use std::ffi::{OsStr, c_void};
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};

use shep_core::transport::PipeInstances;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, LocalFree, SetHandleInformation,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::Console::{
    GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Marks this process's own stdio handles non-inheritable.
///
/// The Windows counterpart to `shep-cli`'s `seal_inherited_fds`.
/// `CreateProcess` with `bInheritHandles = TRUE`, which `std` passes whenever
/// any stdio is redirected, inherits every inheritable handle the parent
/// holds, not only the ones `std` prepared. A shepherd spawned from a shell
/// that gave `shep` a pipe for stdout would hold that pipe open for life, so
/// `shep start | Out-Null` never returns.
///
/// Called immediately before a spawn: it changes only the inherit flag, so
/// this process goes on using its own stdio. Best-effort, nothing to report.
pub fn seal_std_handles() {
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: `GetStdHandle` takes one of the three documented standard
        // handle ids and returns a handle this process already owns, not a
        // new reference, so it must not be closed. Null or
        // `INVALID_HANDLE_VALUE` is filtered before use.
        let handle = unsafe { GetStdHandle(id) };
        if handle.is_null() || handle == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            continue;
        }
        // SAFETY: `handle` is a live handle owned by this process, checked
        // just above. Clearing `HANDLE_FLAG_INHERIT` changes only whether a
        // future `CreateProcess` passes it on; it neither closes the handle
        // nor affects I/O through it. A failure leaves one spawn inheriting a
        // handle it should not, which is not worth refusing a start over.
        unsafe {
            let _ = SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

/// A security descriptor that admits the shepherd's own user and nobody else
///
/// A named pipe made with `CreateNamedPipe` and no descriptor gets the
/// default one, which grants `Everyone` read, and the single accept behind a
/// per-spawn pipe authenticates nobody. Created under this one, a pipe is
/// refused to every other account, administrators and `SYSTEM` included: the
/// DACL holds one ACE, `GENERIC_ALL` for the user the shepherd runs as, and is
/// protected, so nothing inherits into it.
///
/// It confines nothing on the same account. A dog runs as the operator and
/// can open any pipe the operator can; this keeps a different local account
/// from connecting first and reading a dog's or a sheep's frames (#350).
///
/// Built once per pipe and handed to [`shep_core::transport::Listener::bind_with`]
/// as its [`PipeInstances`], so the first instance and every replacement are
/// created under it.
pub(crate) struct UserOnly {
    /// A self-relative descriptor `LocalAlloc`ed by the converter, freed on drop.
    descriptor: *mut c_void,
}

// SAFETY: the descriptor is written once by the converter and only read after
// that, by `CreateNamedPipe`, which copies it. No Win32 call here mutates it,
// and a raw pointer to memory nothing else aliases is plain data.
unsafe impl Send for UserOnly {}
// SAFETY: shared access only reads the descriptor, as above.
unsafe impl Sync for UserOnly {}

impl core::fmt::Debug for UserOnly {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UserOnly").finish_non_exhaustive()
    }
}

impl UserOnly {
    /// Builds the descriptor for the user this process runs as.
    ///
    /// # Errors
    ///
    /// Whatever the OS says if the process token cannot be read or the
    /// descriptor cannot be built from the user's SID.
    pub(crate) fn current_user() -> io::Result<Self> {
        let sid = current_user_sid()?;
        // `D:P` is a protected DACL, `A` an allow, `GA` generic-all. Narrower
        // rights would refuse the shepherd its own next instance: creating
        // one needs `FILE_CREATE_PIPE_INSTANCE`, which is in generic-all.
        let mut sddl: Vec<u16> = "D:P(A;;GA;;;".encode_utf16().collect();
        sddl.extend_from_slice(&sid);
        sddl.extend(")\0".encode_utf16());

        let mut descriptor: *mut c_void = core::ptr::null_mut();
        // SAFETY: `sddl` is a NUL-terminated UTF-16 string that outlives the
        // call, and `descriptor` is a live local for the out pointer. The
        // size out parameter is documented optional and passed null. On
        // success the descriptor is `LocalAlloc`ed and handed to `Self`,
        // which frees it; on failure nothing was allocated.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                core::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { descriptor })
    }
}

impl PipeInstances for UserOnly {
    fn create(&self, options: &ServerOptions, addr: &OsStr) -> io::Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(core::mem::size_of::<SECURITY_ATTRIBUTES>())
                .expect("the size of a fixed Win32 struct fits in u32"),
            lpSecurityDescriptor: self.descriptor,
            // The handle stays out of the child: it is the shepherd's end.
            bInheritHandle: 0,
        };
        // SAFETY: `attributes` is a fully initialised `SECURITY_ATTRIBUTES`
        // on this stack frame, so the pointer is valid for the whole call,
        // and its descriptor pointer is the live allocation `self` owns.
        // `CreateNamedPipeW` copies what it needs and keeps neither pointer.
        unsafe { options.create_with_security_attributes_raw(addr, (&raw mut attributes).cast()) }
    }
}

impl Drop for UserOnly {
    fn drop(&mut self) {
        // SAFETY: `descriptor` was allocated by the converter, which
        // documents `LocalFree` as its release, and nothing else frees it:
        // `UserOnly` is neither `Copy` nor `Clone`, so this runs once. A
        // failing free in a destructor has no caller to report to.
        unsafe {
            let _ = LocalFree(self.descriptor);
        }
    }
}

/// The string form of the SID of the user this process runs as, as UTF-16
/// without a terminator.
fn current_user_sid() -> io::Result<Vec<u16>> {
    let mut token: HANDLE = core::ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
    // closing, and `token` is a live local for the out pointer. Success hands
    // back a real handle that this process owns.
    let opened = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
    if opened == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` is a handle `OpenProcessToken` just returned and
    // nothing else owns, so `OwnedHandle` closes it exactly once.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };

    let mut needed = 0_u32;
    // SAFETY: a null buffer of length zero is the documented way to ask for
    // the size, and `needed` is a live local. The call is expected to fail
    // with `ERROR_INSUFFICIENT_BUFFER`; only `needed` is read after it.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            core::ptr::null_mut(),
            0,
            &raw mut needed,
        );
    }
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    // `u64`s so the buffer is aligned for the `TOKEN_USER` read below.
    let mut buffer = vec![0_u64; usize::try_from(needed).unwrap_or(0).div_ceil(8)];
    // SAFETY: `buffer` is at least `needed` bytes, which is the length passed,
    // and `token` is a live token handle opened with `TOKEN_QUERY`.
    let filled = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
        )
    };
    if filled == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call above filled `buffer` with a `TOKEN_USER` for the
    // `TokenUser` class, and the buffer is 8-byte aligned. Its `Sid` points
    // into the same buffer, which outlives every use below.
    let sid = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };

    // SAFETY: `sid` points into `buffer`, which is alive until this function
    // returns, and the call above filled it with a valid SID.
    unsafe { sid_to_wide(sid) }
}

/// The string form of `sid`, as UTF-16 without a terminator.
///
/// # Safety
///
/// `sid` must point at a valid SID that stays alive for the call.
unsafe fn sid_to_wide(sid: PSID) -> io::Result<Vec<u16>> {
    let mut text: *mut u16 = core::ptr::null_mut();
    // SAFETY: `sid` is valid by this function's contract, and `text` is a
    // live local for the out pointer. Success hands back a `LocalAlloc`ed
    // NUL-terminated string that is freed below.
    let converted = unsafe { ConvertSidToStringSidW(sid, &raw mut text) };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut len = 0;
    // SAFETY: `text` is NUL-terminated, so the scan stops at the terminator,
    // and every index read is before or at it.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `text` holds `len` initialised `u16`s, read before the free.
    let wide = unsafe { core::slice::from_raw_parts(text, len) }.to_vec();
    // SAFETY: `text` came from `ConvertSidToStringSidW`, which documents
    // `LocalFree` as its release, and `wide` is a copy, so nothing reads it
    // after.
    unsafe {
        let _ = LocalFree(text.cast());
    }
    Ok(wide)
}

#[cfg(test)]
mod pipe_security_tests;
