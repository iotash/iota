//! Waiting on the terminal's descriptor without reading from it (unix). The startup round-trip
//! (`super::osc::exchange`) reads a byte only once one is there; the loop's hang-up watch
//! (`super::event_loop`) must tell a hung-up terminal from a live one before crossterm reads it.
//!
//! `select(2)`, not `poll(2)`: macOS's `poll` does not work on a device. On `/dev/tty` it returns
//! at once with `POLLNVAL` whether the terminal is alive or gone, so a wait written with it is no
//! wait at all, and `POLLNVAL` says nothing about a hang-up (measured 2026-11-18 under `script(1)`;
//! the `filedescriptor` crate crossterm reads the tty through uses `select` on macOS for the same
//! reason). `select` waits properly on every unix terminal, and reports a hung-up one as readable.

use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd};
use std::time::Duration;

/// Waits up to `timeout` for `fd` to have something to read: `Ok(true)` when a read will not
/// block (a byte, or the end of a hung-up terminal — [`hung_up`] tells which), `Ok(false)` when
/// the time ran out. A signal that cuts the wait short is an `Err` of kind
/// [`io::ErrorKind::Interrupted`], the standard library's "make the call again": the caller comes
/// back with what is left of its time.
pub(crate) fn readable(fd: BorrowedFd<'_>, timeout: Duration) -> io::Result<bool> {
    use nix::sys::select::{FD_SETSIZE, FdSet, select};
    use nix::sys::time::{TimeVal, TimeValLike as _};

    // `FdSet` cannot hold a descriptor past its size (and panics on one).
    if usize::try_from(fd.as_raw_fd()).map_or(true, |n| n >= FD_SETSIZE) {
        return Err(io::Error::other(
            "the terminal's descriptor is out of select's range",
        ));
    }
    let mut set = FdSet::new();
    set.insert(fd);
    let mut wait = TimeVal::microseconds(i64::try_from(timeout.as_micros()).unwrap_or(i64::MAX));
    select(None, &mut set, None, None, &mut wait)?;
    Ok(set.contains(fd))
}

/// Whether a terminal [`readable`] just reported has hung up rather than sent a byte, asked
/// without reading (crossterm reads it, and a byte taken here would be a lost key): readable
/// with nothing queued is the end-of-file a read would return (macOS answers `FIONREAD` with 0),
/// and Linux answers the `FIONREAD` of a hung-up terminal with `EIO`.
pub(crate) fn hung_up(fd: BorrowedFd<'_>) -> io::Result<bool> {
    match rustix::io::ioctl_fionread(fd) {
        Ok(queued) => Ok(queued == 0),
        Err(rustix::io::Errno::IO) => Ok(true),
        Err(e) => Err(e.into()),
    }
}
