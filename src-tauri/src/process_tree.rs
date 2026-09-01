//! Keep Desktop-owned Windows children and all of their descendants together.
//!
//! Revka starts MCP sidecars. Killing only the root process can leave those
//! sidecars (and inherited socket handles) behind, so Windows children are put
//! in a kill-on-close Job Object. Unix PTYs are session leaders, so their whole
//! process group is terminated and verified when the embedded terminal closes.

#[cfg(windows)]
mod platform {
    use std::io;
    use std::mem::size_of;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    pub struct ProcessTree {
        handle: HANDLE,
        terminate_lock: std::sync::Mutex<()>,
    }

    // Job handles may be used from the lifecycle/PTY mutex owners on different
    // threads. Windows HANDLE operations used here are thread-safe.
    unsafe impl Send for ProcessTree {}
    unsafe impl Sync for ProcessTree {}

    impl ProcessTree {
        pub fn new() -> Result<Self, String> {
            // SAFETY: null security attributes create a non-inheritable,
            // unnamed Job Object owned only by this process.
            let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if handle.is_null() {
                return Err(format!(
                    "could not create a Windows process job: {}",
                    io::Error::last_os_error()
                ));
            }

            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `limits` has the exact structure and size required by
            // JobObjectExtendedLimitInformation and remains alive for the call.
            let configured = unsafe {
                SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const std::ffi::c_void,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                let error = io::Error::last_os_error();
                // SAFETY: `handle` was returned by CreateJobObjectW above.
                unsafe { CloseHandle(handle) };
                return Err(format!(
                    "could not configure a Windows process job: {error}"
                ));
            }
            Ok(Self {
                handle,
                terminate_lock: std::sync::Mutex::new(()),
            })
        }

        fn assign_handle(&self, process: HANDLE) -> Result<(), String> {
            // SAFETY: both handles are live and retained by their Rust owners.
            if unsafe { AssignProcessToJobObject(self.handle, process) } == 0 {
                return Err(format!(
                    "could not attach a child to its Windows process job: {}",
                    io::Error::last_os_error()
                ));
            }
            Ok(())
        }

        pub fn assign_std_child(&self, child: &std::process::Child) -> Result<(), String> {
            use std::os::windows::io::AsRawHandle;
            self.assign_handle(child.as_raw_handle() as HANDLE)
        }

        pub fn prepare_std_command(&self, command: &mut std::process::Command) {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::CREATE_SUSPENDED;
            // `util::command` normally applies CREATE_NO_WINDOW. CommandExt
            // replaces (rather than adds to) the flags, so preserve it here.
            command.creation_flags(0x0800_0000 | CREATE_SUSPENDED);
        }

        pub fn resume_std_child(&self, child: &std::process::Child) -> Result<(), String> {
            use windows_sys::Win32::System::Threading::{
                OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
            };

            // SAFETY: the snapshot is read only until its handle is closed.
            let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
            if snapshot == INVALID_HANDLE_VALUE {
                return Err(format!(
                    "could not enumerate the suspended Revka thread: {}",
                    io::Error::last_os_error()
                ));
            }

            let mut entry = THREADENTRY32 {
                dwSize: size_of::<THREADENTRY32>() as u32,
                ..Default::default()
            };
            // SAFETY: `entry` has the documented size and lives for each call.
            let mut found = unsafe { Thread32First(snapshot, &mut entry) } != 0;
            let mut resumed = false;
            while found {
                if entry.th32OwnerProcessID == child.id() {
                    // SAFETY: the ID came from the live snapshot; the returned
                    // handle is non-inheritable and closed below.
                    let thread =
                        unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                    if thread.is_null() {
                        unsafe { CloseHandle(snapshot) };
                        return Err(format!(
                            "could not open the suspended Revka thread: {}",
                            io::Error::last_os_error()
                        ));
                    }
                    let result = unsafe { ResumeThread(thread) };
                    unsafe { CloseHandle(thread) };
                    if result == u32::MAX {
                        unsafe { CloseHandle(snapshot) };
                        return Err(format!(
                            "could not resume Revka after assigning its process job: {}",
                            io::Error::last_os_error()
                        ));
                    }
                    resumed = true;
                    break;
                }
                found = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
            }
            unsafe { CloseHandle(snapshot) };
            if !resumed {
                return Err("could not find the suspended Revka thread".into());
            }
            Ok(())
        }

        pub fn assign_pty_child(&self, child: &dyn portable_pty::Child) -> Result<(), String> {
            let handle = child
                .as_raw_handle()
                .ok_or("the Windows terminal child has no process handle")?;
            self.assign_handle(handle as HANDLE)
        }

        pub fn terminate(&self) -> Result<(), String> {
            let _termination = self.terminate_lock.lock().map_err(|e| e.to_string())?;
            // SAFETY: the Job Object stays live through `self` for this call.
            if unsafe { TerminateJobObject(self.handle, 1) } == 0 {
                return Err(format!(
                    "could not stop the Windows process tree: {}",
                    io::Error::last_os_error()
                ));
            }
            Ok(())
        }

        pub fn terminate_after_leader_exit(&self) -> Result<(), String> {
            // A Windows Job handle retains object identity and cannot be
            // retargeted through PID reuse, so the ordinary verified call is
            // safe both before and after the root process exits.
            self.terminate()
        }
    }

    impl Drop for ProcessTree {
        fn drop(&mut self) {
            // KILL_ON_JOB_CLOSE retires any descendants that survived a normal
            // root exit. The handle itself is never inherited by children.
            unsafe { CloseHandle(self.handle) };
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::sync::atomic::{AtomicI32, Ordering};
    use std::time::{Duration, Instant};

    const TERM_GRACE: Duration = Duration::from_millis(750);
    const KILL_GRACE: Duration = Duration::from_secs(1);

    pub struct ProcessTree {
        pty_process_group: AtomicI32,
        terminate_lock: std::sync::Mutex<()>,
    }

    impl ProcessTree {
        pub fn new() -> Result<Self, String> {
            Ok(Self {
                pty_process_group: AtomicI32::new(0),
                terminate_lock: std::sync::Mutex::new(()),
            })
        }

        pub fn assign_std_child(&self, _child: &std::process::Child) -> Result<(), String> {
            Ok(())
        }

        pub fn prepare_std_command(&self, _command: &mut std::process::Command) {}

        pub fn resume_std_child(&self, _child: &std::process::Child) -> Result<(), String> {
            Ok(())
        }

        pub fn assign_pty_child(&self, child: &dyn portable_pty::Child) -> Result<(), String> {
            let pid = child
                .process_id()
                .ok_or("the terminal child has no process id")? as i32;
            // portable-pty calls setsid() before exec on Unix, so this child is
            // both the session leader and process-group leader. Its Revka child
            // remains in that foreground group even for non-POSIX shells.
            self.pty_process_group.store(pid, Ordering::Release);
            Ok(())
        }

        pub fn terminate(&self) -> Result<(), String> {
            let _termination = self.terminate_lock.lock().map_err(|e| e.to_string())?;
            self.terminate_group()
        }

        pub fn terminate_after_leader_exit(&self) -> Result<(), String> {
            let _termination = self.terminate_lock.lock().map_err(|e| e.to_string())?;
            let result = self.terminate_group();
            if result.is_err() {
                // Once the leader has been reaped, a raw PGID cannot safely be
                // retained for a delayed retry: the numeric ID may be reused.
                // Surface the failure to UI but disarm all future signalling.
                self.pty_process_group.store(0, Ordering::Release);
            }
            result
        }

        fn terminate_group(&self) -> Result<(), String> {
            let group = self.pty_process_group.load(Ordering::Acquire);
            if group <= 0 {
                return Ok(());
            }

            // Negative PID targets the full process group, not only the shell.
            // Revka's daemon deliberately ignores SIGHUP, so use SIGTERM for a
            // graceful shutdown and prove the group disappeared before we
            // report success. Escalate to SIGKILL when a child ignores TERM.
            if !signal_group(group, libc::SIGTERM)? {
                self.pty_process_group.store(0, Ordering::Release);
                return Ok(());
            }
            if !wait_for_group(group, TERM_GRACE)? {
                signal_group(group, libc::SIGKILL)?;
                if !wait_for_group(group, KILL_GRACE)? {
                    return Err("the terminal process group survived SIGKILL".into());
                }
            }
            self.pty_process_group.store(0, Ordering::Release);
            Ok(())
        }
    }

    impl Drop for ProcessTree {
        fn drop(&mut self) {
            let group = self.pty_process_group.swap(0, Ordering::AcqRel);
            if group > 0 {
                // Last-resort app-shutdown cleanup. Interactive closes use the
                // verified TERM/KILL path above and clear the group first.
                unsafe { libc::kill(-group, libc::SIGKILL) };
            }
        }
    }

    /// Signal a process group, returning false only when it no longer exists.
    fn signal_group(group: i32, signal: i32) -> Result<bool, String> {
        let result = unsafe { libc::kill(-group, signal) };
        if result == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(format!(
                "could not signal the terminal process group: {error}"
            ))
        }
    }

    fn wait_for_group(group: i32, timeout: Duration) -> Result<bool, String> {
        let deadline = Instant::now() + timeout;
        loop {
            // Signal zero performs the existence/permission check without
            // changing process state. EPERM proves the group still exists.
            let result = unsafe { libc::kill(-group, 0) };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                match error.raw_os_error() {
                    Some(libc::ESRCH) => return Ok(true),
                    Some(libc::EPERM) => {}
                    _ => {
                        return Err(format!(
                            "could not verify the terminal process group stopped: {error}"
                        ))
                    }
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

pub use platform::ProcessTree;
