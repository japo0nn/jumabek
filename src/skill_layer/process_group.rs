use tokio::process::Command;

pub struct ProcessGroup {
    #[cfg(windows)]
    job: Option<windows::Job>,
    #[cfg(not(windows))]
    leader: Option<u32>,
}

impl ProcessGroup {
    pub fn new() -> Self {
        ProcessGroup {
            #[cfg(windows)]
            job: None,
            #[cfg(not(windows))]
            leader: None,
        }
    }

    pub fn prepare(&self, command: &mut Command) {
        #[cfg(unix)]
        {
            use std::io;
            unsafe {
                command.pre_exec(|| {
                    if libc::setpgid(0, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        #[cfg(not(unix))]
        {
            let _ = command;
        }
    }

    pub fn adopt(&mut self, pid: u32) {
        #[cfg(windows)]
        {
            match windows::Job::containing(pid) {
                Ok(job) => self.job = Some(job),
                Err(e) => eprintln!("[skill_layer] cannot contain process {}: {}", pid, e),
            }
        }

        #[cfg(not(windows))]
        {
            self.leader = Some(pid);
        }
    }

    pub fn kill_all(&mut self) {
        #[cfg(windows)]
        {
            if let Some(job) = self.job.take() {
                job.terminate();
            }
        }

        #[cfg(unix)]
        {
            if let Some(pid) = self.leader.take() {
                for descendant in unix_descendants(pid) {
                    unsafe {
                        libc::kill(descendant as i32, libc::SIGKILL);
                    }
                }
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                    libc::kill(pid as i32, libc::SIGKILL);
                }
            }
        }
    }
}

#[cfg(unix)]
fn unix_descendants(root: u32) -> Vec<u32> {
    let parents = read_parent_table();

    let mut generation = vec![root];
    let mut found: Vec<u32> = Vec::new();

    while !generation.is_empty() {
        let next: Vec<u32> = parents
            .iter()
            .filter(|(pid, ppid)| generation.contains(ppid) && !found.contains(pid))
            .map(|(pid, _)| *pid)
            .collect();

        found.extend(next.iter().copied());
        generation = next;
    }

    found.reverse();
    found
}

#[cfg(target_os = "linux")]
fn read_parent_table() -> Vec<(u32, u32)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };

    let mut table = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };

        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };

        let Some(rest) = stat.rsplit_once(')').map(|(_, tail)| tail) else {
            continue;
        };

        if let Some(ppid) = rest.split_whitespace().nth(1).and_then(|f| f.parse().ok()) {
            table.push((pid, ppid));
        }
    }

    table
}

#[cfg(all(unix, not(target_os = "linux")))]
fn read_parent_table() -> Vec<(u32, u32)> {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-eo", "pid=,ppid="])
        .output()
    else {
        return Vec::new();
    };

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            Some((pid, ppid))
        })
        .collect()
}

impl Default for ProcessGroup {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill_all();
    }
}

#[cfg(windows)]
mod windows {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    pub struct Job(HANDLE);

    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn containing(pid: u32) -> io::Result<Self> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(io::Error::last_os_error());
                }

                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &limits as *const _ as *const _,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    let error = io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(error);
                }

                let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                if process.is_null() {
                    let error = io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(error);
                }

                let assigned = AssignProcessToJobObject(job, process);
                CloseHandle(process);

                if assigned == 0 {
                    let error = io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(error);
                }

                Ok(Job(job))
            }
        }

        pub fn terminate(&self) {
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}
