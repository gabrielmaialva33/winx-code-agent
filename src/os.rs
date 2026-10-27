//! Narrow operating-system boundary for audited libc and Win32 process calls.

#![allow(unsafe_code)]

#[cfg(target_os = "linux")]
pub(crate) mod temporary {
    //! Directory-relative Linux claims for explicitly released managed helpers.
    #![allow(unsafe_code)]

    use std::ffi::{CString, OsStr};
    use std::fs::File;
    use std::io;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt as _;
    use std::path::{Component, Path};

    fn name(value: &OsStr) -> io::Result<CString> {
        CString::new(value.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
    }

    fn open_at(parent: &File, leaf: &CString, flags: i32) -> io::Result<File> {
        // SAFETY: parent and the NUL-terminated name remain alive for this call.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                leaf.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: openat returned a newly owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(crate) fn directory(path: &Path) -> io::Result<File> {
        if !path.is_absolute() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "absolute directory required"));
        }
        let mut dir = File::open("/")?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(part) => {
                    dir = open_at(&dir, &name(part)?, libc::O_RDONLY | libc::O_DIRECTORY)?;
                }
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "noncanonical directory component",
                    ))
                }
            }
        }
        Ok(dir)
    }

    /// Enumerate a pinned directory through its kernel descriptor alias. The
    /// caller keeps the descriptor alive for the iterator's complete lifetime.
    pub(crate) fn directory_entries(directory: &File) -> io::Result<std::fs::ReadDir> {
        std::fs::read_dir(format!("/proc/self/fd/{}", directory.as_raw_fd()))
    }

    pub(crate) fn directory_child(directory: &File, child: &OsStr) -> io::Result<File> {
        open_at(directory, &name(child)?, libc::O_RDONLY | libc::O_DIRECTORY)
    }

    pub(crate) struct Opened {
        pub(crate) file: File,
        parent: File,
        leaf: CString,
    }

    pub(crate) fn open(path: &Path) -> io::Result<Opened> {
        let parent = directory(
            path.parent()
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "parent required"))?,
        )?;
        let leaf =
            name(path.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "filename required")
            })?)?;
        let file = open_at(&parent, &leaf, libc::O_RDONLY | libc::O_NONBLOCK)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "regular file required"));
        }
        Ok(Opened { file, parent, leaf })
    }

    fn rename_noreplace(from: &File, old: &CString, to: &File, new: &CString) -> io::Result<()> {
        // SAFETY: borrowed descriptors and CString pointers remain valid. RENAME_NOREPLACE
        // never overwrites a concurrently installed destination.
        if unsafe {
            libc::renameat2(
                from.as_raw_fd(),
                old.as_ptr(),
                to.as_raw_fd(),
                new.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } == 0
        {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    fn unlink(parent: &File, leaf: &CString, flags: i32) -> io::Result<()> {
        // SAFETY: directory descriptor and NUL-terminated name are valid for this call.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), leaf.as_ptr(), flags) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    pub(crate) struct Claim {
        parent: File,
        original: CString,
        quarantine: File,
        quarantine_name: CString,
        captured_name: CString,
        captured: bool,
    }

    impl Opened {
        pub(crate) fn claim(&self) -> io::Result<Claim> {
            let parent = self.parent.try_clone()?;
            let quarantine_name =
                CString::new(format!(".winx-release-{:032x}", rand::random::<u128>()))
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            // SAFETY: parent and the generated single component are valid; a collision
            // fails without opening or modifying the pre-existing directory.
            if unsafe { libc::mkdirat(self.parent.as_raw_fd(), quarantine_name.as_ptr(), 0o700) }
                != 0
            {
                return Err(io::Error::last_os_error());
            }
            let quarantine =
                match open_at(&self.parent, &quarantine_name, libc::O_RDONLY | libc::O_DIRECTORY) {
                    Ok(directory) => directory,
                    Err(error) => {
                        let _ = unlink(&self.parent, &quarantine_name, libc::AT_REMOVEDIR);
                        return Err(error);
                    }
                };
            let mut claim = Claim {
                parent,
                original: self.leaf.clone(),
                quarantine,
                quarantine_name,
                captured_name: c"artifact".to_owned(),
                captured: false,
            };
            rename_noreplace(&self.parent, &self.leaf, &claim.quarantine, &claim.captured_name)?;
            claim.captured = true;
            Ok(claim)
        }
    }

    impl Claim {
        pub(crate) fn open_captured(&self) -> io::Result<File> {
            open_at(&self.quarantine, &self.captured_name, libc::O_RDONLY | libc::O_NONBLOCK)
        }

        pub(crate) fn recovery_name(&self) -> String {
            format!("{}/artifact", self.quarantine_name.to_string_lossy())
        }

        pub(crate) fn remove(mut self) -> io::Result<()> {
            unlink(&self.quarantine, &self.captured_name, 0)?;
            self.captured = false;
            Ok(())
        }
    }

    impl Drop for Claim {
        fn drop(&mut self) {
            if self.captured {
                // A failed recheck preserves the captured object. Restore only if
                // its original name is vacant; otherwise retain it in quarantine.
                if rename_noreplace(
                    &self.quarantine,
                    &self.captured_name,
                    &self.parent,
                    &self.original,
                )
                .is_err()
                {
                    return;
                }
            }
            let _ = unlink(&self.parent, &self.quarantine_name, libc::AT_REMOVEDIR);
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn restoration_never_overwrites_a_concurrent_original_name() -> io::Result<()> {
            let directory = tempfile::tempdir()?;
            let root = directory.path().canonicalize()?;
            let path = root.join("helper");
            std::fs::write(&path, "captured")?;
            let opened = open(&path)?;
            let claim = opened.claim()?;
            let recovery = root.join(claim.recovery_name());
            std::fs::write(&path, "replacement")?;
            drop(claim);
            assert_eq!(std::fs::read_to_string(&path)?, "replacement");
            assert_eq!(std::fs::read_to_string(recovery)?, "captured");
            Ok(())
        }
    }
}

#[cfg(unix)]
pub(crate) mod unix {
    use std::io;

    pub(crate) fn effective_uid() -> u32 {
        // SAFETY: geteuid has no preconditions and reads process credentials only.
        unsafe { libc::geteuid() }
    }

    /// Take an exclusive, non-blocking advisory lock on `file`. Returns
    /// `Ok(false)` when another process holds it. The lock lives as long as
    /// the file descriptor, so callers keep the `File` open to stay locked.
    pub(crate) fn try_lock_exclusive(file: &std::fs::File) -> io::Result<bool> {
        use std::os::unix::io::AsRawFd as _;
        // SAFETY: the descriptor belongs to a live `File` borrowed for the call.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::WouldBlock {
            return Ok(false);
        }
        Err(error)
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
/// Process-wide hardening applied at the top of every binary's `main`.
/// On Windows it narrows the DLL search path (see
/// `windows::restrict_dll_search_path`); Unix has nothing equivalent to do.
pub fn harden_process() {
    #[cfg(windows)]
    windows::restrict_dll_search_path();
}

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

    use windows_sys::Win32::Foundation::{
        CloseHandle, LocalFree, SetHandleInformation, ERROR_ACCESS_DENIED, HANDLE,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, STILL_ACTIVE,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TokenUser, SECURITY_ATTRIBUTES, TOKEN_ELEVATION,
        TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::LibraryLoader::{
        SetDefaultDllDirectories, LOAD_LIBRARY_SEARCH_DEFAULT_DIRS,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetExitCodeProcess, OpenProcess, OpenProcessToken, TerminateProcess,
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
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

    /// Restrict implicit DLL loading to the application directory and
    /// System32.
    ///
    /// `portable-pty` probes a bare `conpty.dll` through the default search
    /// order, which includes the current directory and every `PATH` entry, so
    /// a planted DLL would be loaded into the process that owns the shells.
    /// After this call, a bare name resolves only beside the executable or in
    /// System32 and otherwise falls back to the `ConPTY` exports already in
    /// `kernel32.dll`. Failure is logged and ignored: the process still works,
    /// just without the narrower search path.
    pub(crate) fn restrict_dll_search_path() {
        // SAFETY: the flag is a documented constant; the call has no pointer
        // arguments and only changes this process's loader policy.
        if unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_DEFAULT_DIRS) } == 0 {
            tracing::warn!(
                error = %io::Error::last_os_error(),
                "could not restrict the DLL search path"
            );
        }
    }

    /// Owner-only security descriptor for the daemon named pipes.
    ///
    /// The default pipe DACL lets other local users open the pipe read-only
    /// and lets lower-integrity processes of the same user connect, which
    /// would hand a sandboxed process a shell. This descriptor grants full
    /// access to SYSTEM and read/write to the current account SID only, and a
    /// mandatory integrity label refuses readers and writers below the
    /// daemon's own level (`High` when the daemon runs elevated, otherwise
    /// `Medium`), so an unelevated client cannot drive an elevated daemon.
    pub(crate) struct PipeSecurity {
        descriptor: *mut c_void,
        attributes: SECURITY_ATTRIBUTES,
    }

    // SAFETY: the descriptor is a LocalAlloc block that is only read after
    // construction; nothing about it is thread-affine.
    unsafe impl Send for PipeSecurity {}
    // SAFETY: see the `Send` impl; shared references only read the block.
    unsafe impl Sync for PipeSecurity {}

    impl Drop for PipeSecurity {
        fn drop(&mut self) {
            // SAFETY: the pointer came from
            // ConvertStringSecurityDescriptorToSecurityDescriptorW, whose
            // contract is a single LocalFree.
            unsafe {
                LocalFree(self.descriptor);
            }
        }
    }

    impl PipeSecurity {
        pub(crate) fn owner_only() -> io::Result<Self> {
            let sddl = owner_only_sddl()?;
            let descriptor = parse_sddl(&sddl)?;
            let attributes = SECURITY_ATTRIBUTES {
                nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>())
                    .map_err(|_| io::Error::other("security attributes too large"))?,
                lpSecurityDescriptor: descriptor,
                bInheritHandle: 0,
            };
            Ok(Self { descriptor, attributes })
        }

        /// Create one server instance of `name` carrying this descriptor.
        pub(crate) fn create_pipe_instance(
            &self,
            options: &tokio::net::windows::named_pipe::ServerOptions,
            name: &str,
        ) -> io::Result<tokio::net::windows::named_pipe::NamedPipeServer> {
            let attributes: *const SECURITY_ATTRIBUTES = &raw const self.attributes;
            // SAFETY: the attributes block and its descriptor outlive the
            // call, and the descriptor was produced by the SDDL parser.
            unsafe {
                options.create_with_security_attributes_raw(name, attributes.cast_mut().cast())
            }
        }
    }

    /// Build the SDDL string for [`PipeSecurity::owner_only`].
    fn owner_only_sddl() -> io::Result<String> {
        let token = current_process_token()?;
        let sid = token_user_sid(&token)?;
        let integrity = if token_is_elevated(&token)? { "HI" } else { "ME" };
        Ok(format!("D:P(A;;GA;;;SY)(A;;GRGW;;;{sid})S:(ML;;NRNW;;;{integrity})"))
    }

    fn current_process_token() -> io::Result<OwnedHandle> {
        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo handle from GetCurrentProcess is always valid and
        // `token` is a live out-pointer; a zero return is checked.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedHandle(token))
    }

    fn token_user_sid(token: &OwnedHandle) -> io::Result<String> {
        let mut needed = 0u32;
        // SAFETY: a null buffer with zero length is the documented way to ask
        // for the required size; the result is deliberately ignored.
        unsafe {
            GetTokenInformation(token.0, TokenUser, std::ptr::null_mut(), 0, &raw mut needed);
        }
        if needed == 0 {
            return Err(io::Error::last_os_error());
        }
        // `usize` storage keeps TOKEN_USER aligned and its trailing SID alive.
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        // SAFETY: the buffer is at least `needed` bytes and suitably aligned.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &raw mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: GetTokenInformation initialized a TOKEN_USER at the start of
        // the buffer, and the SID it points to lives inside the same buffer.
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut sid_text: *mut u16 = std::ptr::null_mut();
        // SAFETY: the SID pointer is valid while `buffer` lives; the out
        // pointer receives a LocalAlloc string freed below.
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &raw mut sid_text) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let sid = wide_to_string(sid_text);
        // SAFETY: ConvertSidToStringSidW documents a single LocalFree.
        unsafe {
            LocalFree(sid_text.cast());
        }
        Ok(sid)
    }

    fn token_is_elevated(token: &OwnedHandle) -> io::Result<bool> {
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut needed = 0u32;
        let size = u32::try_from(std::mem::size_of::<TOKEN_ELEVATION>())
            .map_err(|_| io::Error::other("token elevation block too large"))?;
        // SAFETY: the out-pointer references a live, correctly sized struct.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                (&raw mut elevation).cast(),
                size,
                &raw mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(elevation.TokenIsElevated != 0)
    }

    /// Parse an SDDL string into a self-relative security descriptor that the
    /// caller must release with `LocalFree`.
    fn parse_sddl(sddl: &str) -> io::Result<*mut c_void> {
        let wide: Vec<u16> = sddl.encode_utf16().chain(std::iter::once(0)).collect();
        let mut descriptor: *mut c_void = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and outlives the call; the out
        // pointer receives a LocalAlloc block; a zero return is checked.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(descriptor)
    }

    fn wide_to_string(text: *const u16) -> String {
        let mut len = 0usize;
        // SAFETY: the string is NUL-terminated by its producer; the loop stops
        // at the terminator without reading past it.
        while unsafe { *text.add(len) } != 0 {
            len += 1;
        }
        // SAFETY: `len` characters were just read successfully.
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) })
    }

    /// Stop this process's standard handles from being inherited by children.
    ///
    /// `std::process::Command` calls `CreateProcessW` with `bInheritHandles`
    /// set, so every inheritable handle leaks into the child even when its own
    /// stdio is `Stdio::null()`. A detached daemon that inherits the pipes an
    /// MCP client or a shell gave this process keeps their write ends open,
    /// and the parent waiting for EOF hangs until the daemon exits. Children
    /// that should inherit stdio still do: `Stdio::inherit` re-duplicates the
    /// handle as inheritable at spawn time.
    pub(crate) fn disable_std_handle_inheritance() {
        for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            // SAFETY: GetStdHandle takes a constant and returns a handle or a
            // sentinel; the sentinels are filtered before the handle is used.
            let handle = unsafe { GetStdHandle(id) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                continue;
            }
            // SAFETY: the handle is one this process owns; clearing a flag on
            // it is not destructive, and a failure is deliberately ignored
            // (a console handle without the flag reports an error).
            unsafe {
                SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
            }
        }
    }

    /// Detach a child from this console, process group, and, when the job
    /// permits it, from the job object that may kill this process's children
    /// (MCP clients commonly wrap servers in such a job). Also stops the
    /// child from inheriting this process's stdio pipes.
    pub(crate) fn configure_detached(command: &mut std::process::Command, breakaway: bool) {
        use std::os::windows::process::CommandExt as _;
        disable_std_handle_inheritance();
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
            let mut child = std::process::Command::new("cmd.exe")
                .args(["/c", "ping -n 30 127.0.0.1 >nul"])
                .spawn()
                .expect("spawn child");
            job.assign(child.id()).expect("assign child to job");
            job.terminate();
            let status = child.wait().expect("child reaped after job termination");
            assert!(!status.success());
            assert!(!super::process_exists(child.id()));
        }

        #[test]
        fn owner_only_sddl_names_the_current_user_and_an_integrity_label() {
            let sddl = super::owner_only_sddl().expect("sddl");
            assert!(sddl.starts_with("D:P(A;;GA;;;SY)(A;;GRGW;;;S-1-"), "{sddl}");
            assert!(
                sddl.ends_with("S:(ML;;NRNW;;;ME)") || sddl.ends_with("S:(ML;;NRNW;;;HI)"),
                "{sddl}"
            );
            let security = super::PipeSecurity::owner_only().expect("descriptor");
            assert!(!security.descriptor.is_null());
        }

        #[test]
        fn dll_search_path_restriction_is_accepted() {
            super::restrict_dll_search_path();
        }

        #[test]
        fn std_handles_stop_being_inheritable() {
            use windows_sys::Win32::Foundation::{
                GetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
            };
            use windows_sys::Win32::System::Console::{
                GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
            };

            super::disable_std_handle_inheritance();
            for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
                // SAFETY: constant argument; sentinels are filtered below.
                let handle = unsafe { GetStdHandle(id) };
                if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                    continue;
                }
                let mut flags = 0u32;
                // SAFETY: valid handle and a live out-pointer for the flags.
                let queried =
                    unsafe { GetHandleInformation(handle, std::ptr::from_mut(&mut flags)) };
                if queried == 0 {
                    // Console handles on some hosts reject the query; nothing to assert.
                    continue;
                }
                assert_eq!(flags & HANDLE_FLAG_INHERIT, 0, "std handle {id} still inheritable");
            }
        }
    }
}

/// Open a regular file without following its final path component. Callers must
/// establish the parent directory's trust separately and bound their reads.
#[cfg(unix)]
pub(crate) fn open_read_nofollow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    Ok(file)
}

#[cfg(windows)]
pub(crate) fn open_read_nofollow(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    // Win32 flags are kept here to avoid exposing platform mechanics to callers.
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "expected a regular file without a reparse point",
        ));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn open_read_nofollow(_path: &std::path::Path) -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "safe no-follow record reads are unsupported on this platform",
    ))
}
