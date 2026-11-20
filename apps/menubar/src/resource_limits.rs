//! Raises `RLIMIT_NOFILE` before the server starts accepting connections.
//!
//! One Cursor conversation holds several sockets at once (Cursor, the provider,
//! MCP servers); macOS defaults to 256, which a busy machine exhausts.

/// The soft limit this build asks for; the hard limit is never raised.
pub(crate) const REQUESTED_OPEN_FILE_LIMIT: u64 = 65_536;

/// Raises the soft limit to `REQUESTED_OPEN_FILE_LIMIT` when the hard limit allows it.
pub(crate) fn raise_open_file_limit() -> std::io::Result<()> {
    // SAFETY: both `rlimit` values are initialized and only read by `setrlimit`,
    // which accepts them by value.
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut current) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let target = current
        .rlim_max
        .min(REQUESTED_OPEN_FILE_LIMIT as libc::rlim_t);
    if current.rlim_cur >= target {
        return Ok(());
    }
    let raised = libc::rlimit {
        rlim_cur: target,
        rlim_max: current.rlim_max,
    };
    // SAFETY: `raised` keeps the hard limit unchanged, so it cannot fail by EPERM
    // for an unprivileged process.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raised) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
