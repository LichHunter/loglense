//! The runner's only OS surface beyond std (D-016): `setsid(2)` for the
//! detached supervisor, `kill(2)` for process-group signaling, `statvfs(3)`
//! for the pre-spawn free-space floor, and a TERM/INT trap so an external
//! signal to the supervisor is forwarded to the child group instead of
//! orphaning it. Every `unsafe` block in the crate lives here, each wrapping
//! a single libc call; the two that run in fork/signal context (`setsid`,
//! the handler's atomic store) are async-signal-safe.

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};

pub(crate) use libc::{SIGKILL, SIGTERM};

/// Signal number caught by the stop trap; 0 while none arrived.
static STOP_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// Makes the spawned process a new session leader (no controlling
/// terminal, own process group) before it execs. The child of the fork is
/// never a group leader, so `setsid` cannot fail with EPERM — the guarantee
/// the double-fork form exists for, in one fork.
pub(crate) fn new_session(cmd: &mut Command) -> &mut Command {
    // SAFETY: the closure runs between fork and exec and only calls
    // `setsid`, which is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                Err(io::Error::last_os_error())
            } else {
                Ok(())
            }
        })
    }
}

/// Sends `signal` to every process in group `pgid`. A group that is already
/// gone (ESRCH) counts as success: the caller wanted it dead.
pub(crate) fn signal_group(pgid: u32, signal: i32) -> io::Result<()> {
    let Ok(pgid) = i32::try_from(pgid) else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    };
    if pgid <= 1 {
        // -1 would signal every process we may signal; never do that.
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    // SAFETY: plain syscall wrapper; no memory is shared with the kernel.
    if unsafe { libc::kill(-pgid, signal) } == -1 {
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::ESRCH) {
            return Err(err);
        }
    }
    Ok(())
}

/// Bytes available to unprivileged writers on the filesystem holding
/// `path` (`statvfs` `f_bavail × f_frsize`).
pub(crate) fn free_bytes(path: &std::path::Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    // SAFETY: `c_path` is a valid NUL-terminated string and `stat` is a
    // zeroed out-parameter the kernel fills.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) == -1 {
            return Err(io::Error::last_os_error());
        }
        // Both fields are u64 on 64-bit Linux but narrower on some 32-bit
        // targets; the conversion keeps this portable.
        #[allow(clippy::useless_conversion)]
        let free = u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize));
        Ok(free)
    }
}

/// Installs the TERM/INT trap: the handler only records the signal number
/// (an atomic store — async-signal-safe); the capture loop polls
/// [`stop_signal`] and runs the forward-to-group kill sequence.
pub(crate) fn trap_stop_signals() -> io::Result<()> {
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: `action` is fully initialized before use; the handler is
        // an `extern "C"` fn doing one atomic store.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = record_stop as extern "C" fn(libc::c_int) as libc::sighandler_t;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            if libc::sigaction(signal, &action, std::ptr::null_mut()) == -1 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

extern "C" fn record_stop(signal: libc::c_int) {
    STOP_SIGNAL.store(signal, Ordering::SeqCst);
}

/// The trapped stop signal, if one arrived.
pub(crate) fn stop_signal() -> Option<i32> {
    match STOP_SIGNAL.load(Ordering::SeqCst) {
        0 => None,
        signal => Some(signal),
    }
}
