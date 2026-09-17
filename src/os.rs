//! Narrow operating-system boundary for audited libc and Win32 process calls.

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

/// Whether a process with `pid` is alive, on every supported platform.
pub(crate) fn process_exists(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unix::process_exists(pid)
    }
    #[cfg(windows)]
    {
        windows::process_exists(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(windows)]
pub(crate) mod windows {
    use std::ffi::c_void;
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, HANDLE, STILL_ACTIVE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, TerminateProcess, CREATE_BREAKAWAY_FROM_JOB,
        CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    /// Kernel handle closed on drop.
    struct OwnedHandle(HANDLE);

    // SAFETY: a Win32 handle is a process-wide token, not thread-affine memory;
    // it may be used and closed from any thread.
    unsafe impl Send for OwnedHandle {}
    // SAFETY: the job/process calls made through the handle are thread-safe.
    unsafe impl Sync for OwnedHandle {}

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: the handle was returned by a successful Win32 call and is
            // closed exactly once here.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn open_process(pid: u32, access: u32) -> io::Result<OwnedHandle> {
        // SAFETY: OpenProcess takes integer arguments only and returns a null
        // handle on failure, which is checked before use.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            Err(io::Error::last_os_error())
        } else {
            Ok(OwnedHandle(handle))
        }
    }

    /// Return whether `pid` names a live process. Access denied means the
    /// process exists but belongs to another user, matching the Unix `EPERM`
    /// rule.
    pub(crate) fn process_exists(pid: u32) -> bool {
        if pid == 0 {
            return false;
        }
        match open_process(pid, PROCESS_QUERY_LIMITED_INFORMATION) {
            Ok(handle) => {
                let mut code = 0u32;
                // SAFETY: `code` outlives the call and the handle is valid.
                let queried =
                    unsafe { GetExitCodeProcess(handle.0, std::ptr::from_mut(&mut code)) } != 0;
                queried && code == STILL_ACTIVE as u32
            }
            Err(error) => error.raw_os_error() == Some(ERROR_ACCESS_DENIED.cast_signed()),
        }
    }

    /// Forcefully end one process. A process that already exited counts as
    /// success, like the Unix `ESRCH` rule.
    pub(crate) fn terminate_process(pid: u32) -> io::Result<()> {
        let handle = match open_process(pid, PROCESS_TERMINATE) {
            Ok(handle) => handle,
            Err(error) if !process_exists(pid) => {
                let _ = error;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        // SAFETY: valid handle with PROCESS_TERMINATE; integer exit code.
        if unsafe { TerminateProcess(handle.0, 1) } == 0 {
            let error = io::Error::last_os_error();
            if process_exists(pid) {
                return Err(error);
            }
        }
        Ok(())
    }

    /// Detach a child from this console, process group, and, when the job
    /// permits it, from the job object that may kill this process's children
    /// (MCP clients commonly wrap servers in such a job).
    pub(crate) fn configure_detached(command: &mut std::process::Command, breakaway: bool) {
        use std::os::windows::process::CommandExt as _;
        let mut flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
        if breakaway {
            flags |= CREATE_BREAKAWAY_FROM_JOB;
        }
        command.creation_flags(flags);
    }

    /// Job object that kills every assigned process, and their descendants,
    /// when the last handle closes.
    pub(crate) struct ProcessTreeJob(OwnedHandle);

    impl ProcessTreeJob {
        pub(crate) fn new() -> io::Result<Self> {
            // SAFETY: null attributes and name request an anonymous job with
            // default security; a null result is checked before use.
            let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = OwnedHandle(job);
            // SAFETY: a zeroed limit block is a valid all-defaults value for
            // this POD struct; only the kill-on-close flag is set.
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let size = u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                .map_err(|_| io::Error::other("job limit block too large"))?;
            // SAFETY: the pointer references a live, correctly sized struct.
            let configured = unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    std::ptr::from_ref(&limits).cast::<c_void>(),
                    size,
                )
            };
            if configured == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(job))
        }

        pub(crate) fn assign(&self, pid: u32) -> io::Result<()> {
            let process = open_process(pid, PROCESS_SET_QUOTA | PROCESS_TERMINATE)?;
            // SAFETY: both handles are valid for the duration of the call.
            if unsafe { AssignProcessToJobObject(self.0 .0, process.0) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        pub(crate) fn terminate(&self) {
            // SAFETY: valid job handle; integer exit code.
            unsafe {
                TerminateJobObject(self.0 .0, 1);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        #![allow(clippy::expect_used)]

        #[test]
        fn current_process_exists_and_pid_zero_does_not() {
            assert!(super::process_exists(std::process::id()));
            assert!(!super::process_exists(0));
        }

        #[test]
        fn job_object_can_be_created_and_owns_a_child() {
            let job = super::ProcessTreeJob::new().expect("job object");
            let child = std::process::Command::new("cmd.exe")
                .args(["/c", "ping -n 30 127.0.0.1 >nul"])
                .spawn()
                .expect("spawn child");
            job.assign(child.id()).expect("assign child to job");
            job.terminate();
            std::thread::sleep(std::time::Duration::from_millis(200));
            assert!(!super::process_exists(child.id()));
        }
    }
}
