//! The descriptor flags a handover flips, and the one `unsafe` block: taking
//! ownership of a descriptor number the predecessor left open.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

use nix::fcntl::{FcntlArg, FdFlag, fcntl};

/// Descriptors 0 to 2 are the standard streams, which the new image already
/// owns through `std`.
pub(crate) const RESERVED_FD_FLOOR: RawFd = 3;

fn flags(fd: RawFd) -> io::Result<FdFlag> {
    Ok(FdFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFD)?))
}

fn set_cloexec(fd: RawFd, on: bool) -> io::Result<()> {
    let mut current = flags(fd)?;
    current.set(FdFlag::FD_CLOEXEC, on);
    fcntl(fd, FcntlArg::F_SETFD(current))?;
    Ok(())
}

/// Clear `FD_CLOEXEC` on `fd`, so it survives this process's next `execve`.
/// Returns whether the bit was set, so whether this call changed anything: a
/// caller that undoes it must not set the bit on a descriptor that was
/// inherited without it.
///
/// # Errors
///
/// `fcntl` failed, which is what a number naming no open descriptor looks like.
pub(crate) fn keep_across_exec(fd: RawFd) -> io::Result<bool> {
    let was_set = flags(fd)?.contains(FdFlag::FD_CLOEXEC);
    if was_set {
        set_cloexec(fd, false)?;
    }
    Ok(was_set)
}

/// Close `fd` if it is an inherited descriptor, as a refused handover must:
/// the same checks as [`adopt_inherited`], then dropped. A number that names
/// nothing, or that this process opened, is left alone.
pub(crate) fn release_inherited(fd: RawFd) {
    drop(adopt_inherited(fd));
}

/// Set `FD_CLOEXEC` on `fd`, undoing a [`keep_across_exec`] whose exec never
/// happened.
///
/// # Errors
///
/// `fcntl` failed.
pub(crate) fn close_on_exec(fd: RawFd) -> io::Result<()> {
    set_cloexec(fd, true)
}

/// Whether `fd` is close-on-exec.
///
/// # Errors
///
/// `fcntl` failed.
#[cfg(test)]
pub(crate) fn is_close_on_exec(fd: RawFd) -> io::Result<bool> {
    Ok(flags(fd)?.contains(FdFlag::FD_CLOEXEC))
}

/// Take ownership of a descriptor a predecessor carried across an `execve`,
/// and make it close-on-exec again so this image's own children do not
/// inherit it.
///
/// A number proves it was inherited by not being close-on-exec: this process
/// opens everything close-on-exec, and a carried descriptor had the bit
/// cleared before the exec. A close-on-exec number is refused, which also
/// refuses adopting one number twice.
///
/// # Errors
///
/// - [`io::ErrorKind::InvalidInput`] if `fd` is a standard stream, or is close-on-exec.
/// - The `fcntl` error if `fd` names no open descriptor.
pub(crate) fn adopt_inherited(fd: RawFd) -> io::Result<OwnedFd> {
    if fd < RESERVED_FD_FLOOR {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a standard stream cannot be adopted",
        ));
    }
    if flags(fd)?.contains(FdFlag::FD_CLOEXEC) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the descriptor is close-on-exec, so this image opened it and nothing carried it",
        ));
    }
    // SAFETY: `fd` is open and not close-on-exec. Everything this process
    // opens is close-on-exec, so a predecessor cleared the bit before the
    // `execve`. It is set again below. A second call for the same number is
    // refused above, so this is its only owner.
    let owned = unsafe { OwnedFd::from_raw_fd(fd) };
    set_cloexec(fd, true)?;
    Ok(owned)
}

#[cfg(test)]
mod tests {
    use std::os::fd::{AsRawFd, IntoRawFd};

    use super::*;

    /// A descriptor with `FD_CLOEXEC` clear, standing in for an inherited one.
    fn inherited() -> RawFd {
        let (read, _write) = std::io::pipe().unwrap();
        let fd = read.into_raw_fd();
        keep_across_exec(fd).unwrap();
        fd
    }

    #[test]
    fn keeping_clears_the_bit_and_closing_sets_it_again() {
        let (read, _write) = std::io::pipe().unwrap();
        let fd = read.as_raw_fd();
        assert!(
            is_close_on_exec(fd).unwrap(),
            "std opens everything close-on-exec"
        );
        keep_across_exec(fd).unwrap();
        assert!(!is_close_on_exec(fd).unwrap());
        close_on_exec(fd).unwrap();
        assert!(is_close_on_exec(fd).unwrap());
    }

    #[test]
    fn keeping_reports_whether_it_changed_the_bit() {
        let (read, _write) = std::io::pipe().unwrap();
        let fd = read.as_raw_fd();
        assert!(
            keep_across_exec(fd).unwrap(),
            "it was set, and is clear now"
        );
        assert!(
            !keep_across_exec(fd).unwrap(),
            "already clear, so no change"
        );
    }

    #[test]
    fn releasing_closes_an_inherited_descriptor_and_spares_one_opened_here() {
        let fd = inherited();
        release_inherited(fd);
        assert!(flags(fd).is_err(), "closed");

        let (read, _write) = std::io::pipe().unwrap();
        release_inherited(read.as_raw_fd());
        assert!(is_close_on_exec(read.as_raw_fd()).unwrap(), "still open");
    }

    #[test]
    fn an_inherited_descriptor_is_adopted_and_made_close_on_exec() {
        let fd = inherited();
        let owned = adopt_inherited(fd).unwrap();
        assert_eq!(owned.as_raw_fd(), fd);
        assert!(is_close_on_exec(fd).unwrap());
    }

    #[test]
    fn the_same_number_cannot_be_adopted_twice() {
        let fd = inherited();
        let _first = adopt_inherited(fd).unwrap();
        let err = adopt_inherited(fd).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_descriptor_this_process_opened_is_refused() {
        let (read, _write) = std::io::pipe().unwrap();
        let err = adopt_inherited(read.as_raw_fd()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn a_standard_stream_is_refused() {
        for fd in 0..3 {
            assert_eq!(
                adopt_inherited(fd).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn a_number_naming_nothing_is_refused() {
        assert!(adopt_inherited(1_000_000).is_err());
    }
}
