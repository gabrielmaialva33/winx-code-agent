//! Narrow operating-system boundary for audited libc calls.

#![allow(unsafe_code)]

#[cfg(unix)]
pub(crate) mod unix {
    use std::io;

    pub(crate) fn effective_uid() -> u32 {
        // SAFETY: geteuid has no preconditions and reads process credentials only.
        unsafe { libc::geteuid() }
    }

    /// Return whether a positive process ID still names a live process. EPERM
    /// means the process exists but is not signalable by this caller.
    pub(crate) fn process_exists(pid: u32) -> bool {
        let Ok(pid) = i32::try_from(pid) else { return false };
        if pid <= 1 {
            return false;
        }
        // SAFETY: signal 0 performs permission/existence checking only and uses
        // no pointers.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return true;
        }
        io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    /// Signal one process. A process that has already exited is treated as a
    /// successful cleanup operation.
    pub(crate) fn signal_process(pid: u32, signal: i32) -> io::Result<()> {
        let pid = i32::try_from(pid)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "pid does not fit pid_t"))?;
        signal_raw(pid, signal)
    }

    pub(crate) fn signal_raw(pid: i32, signal: i32) -> io::Result<()> {
        // SAFETY: kill(2) takes integer process/signal identifiers and no pointers.
        if unsafe { libc::kill(pid, signal) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    /// Resolve a process group only when the child is its own group leader.
    pub(crate) fn owned_process_group(pid: u32) -> Option<i32> {
        let pid = i32::try_from(pid).ok()?;
        if pid <= 1 {
            return None;
        }
        // SAFETY: getpgid reads kernel metadata for an integer pid and returns -1
        // on error; no memory is accessed through pointers.
        let group = unsafe { libc::getpgid(pid) };
        (group == pid).then_some(group)
    }

    pub(crate) fn signal_group(group: i32, signal: i32) -> io::Result<()> {
        if group <= 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "process group must be greater than 1",
            ));
        }
        signal_raw(-group, signal)
    }

    /// Configure a child command to become its own session immediately after
    /// fork and before exec.
    pub(crate) fn configure_detached(command: &mut std::process::Command) {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: the hook invokes only the async-signal-safe `create_session`
        // helper and touches no allocator-backed state between fork and exec.
        unsafe {
            command.pre_exec(create_session);
        }
    }

    /// Async-signal-safe child setup hook used immediately after fork and before
    /// exec by `Command::pre_exec`.
    fn create_session() -> io::Result<()> {
        // SAFETY: setsid has no pointer arguments and is async-signal-safe.
        if unsafe { libc::setsid() } == -1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn current_process_exists_and_reserved_ids_are_rejected() {
            assert!(super::process_exists(std::process::id()));
            assert!(!super::process_exists(0));
            assert!(!super::process_exists(1));
        }
    }
}
