//! Detached tunnel ownership, including PID reuse and reboot protection.
use super::*;
#[cfg(target_os = "linux")]
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::{fs::OpenOptionsExt, process::CommandExt};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct ProcessIdentity {
    pub pid: u32,
    pub start_ticks: u64,
    pub boot_id: String,
}

impl ProcessIdentity {
    #[cfg(target_os = "linux")]
    pub fn capture(pid: u32) -> Result<Self> {
        anyhow::ensure!(pid > 1, "invalid tunnel PID");
        let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .context("invalid process stat")?
            .1
            .split_whitespace()
            .collect();
        anyhow::ensure!(fields.len() > 19, "incomplete process stat");
        anyhow::ensure!(fields[0] != "Z", "tunnel process exited");
        anyhow::ensure!(
            fields[2].parse::<u32>()? == pid,
            "tunnel is not its own process group"
        );
        Ok(Self {
            pid,
            start_ticks: fields[19].parse()?,
            boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")?
                .trim()
                .into(),
        })
    }

    #[cfg(target_os = "macos")]
    pub fn capture(pid: u32) -> Result<Self> {
        anyhow::ensure!(pid > 1, "invalid tunnel PID");
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
        let result = unsafe {
            libc::proc_pidinfo(
                pid as libc::pid_t,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size,
            )
        };
        anyhow::ensure!(result == size, "cannot read tunnel process information");
        let info = unsafe { info.assume_init() };
        anyhow::ensure!(info.pbi_status != libc::SZOMB, "tunnel process exited");
        anyhow::ensure!(info.pbi_pgid == pid, "tunnel is not its own process group");

        let mut boot = std::mem::MaybeUninit::<libc::timeval>::uninit();
        let mut boot_size = std::mem::size_of::<libc::timeval>();
        let result = unsafe {
            libc::sysctlbyname(
                b"kern.boottime\0".as_ptr().cast(),
                boot.as_mut_ptr().cast(),
                &mut boot_size,
                std::ptr::null_mut(),
                0,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("cannot read system boot time");
        }
        let boot = unsafe { boot.assume_init() };
        Ok(Self {
            pid,
            start_ticks: info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
            boot_id: format!("{}:{}", boot.tv_sec, boot.tv_usec),
        })
    }

    pub fn running(&self) -> bool {
        Self::capture(self.pid).is_ok_and(|actual| actual == *self)
    }

    #[cfg(target_os = "linux")]
    fn signal(&self, signal: i32) -> Result<()> {
        if !self.running() {
            return Ok(());
        }
        // Bind the signal to the process, not a PID that could be reused between
        // checking /proc and sending the signal. Supported by AliLinux 3 kernels.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, self.pid, 0) };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(());
            }
            return Err(error).context("cannot open tunnel process handle");
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
        if !self.running() {
            return Ok(());
        }
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error).context("cannot signal tunnel process");
            }
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn signal(&self, signal: i32) -> Result<()> {
        if self.running() && unsafe { libc::kill(self.pid as libc::pid_t, signal) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error).context("cannot signal tunnel process");
            }
        }
        Ok(())
    }

    pub fn stop(&self) -> Result<()> {
        self.signal(libc::SIGTERM)?;
        for _ in 0..30 {
            if !self.running() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        self.signal(libc::SIGKILL)?;
        for _ in 0..20 {
            if !self.running() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        anyhow::ensure!(!self.running(), "tunnel process did not stop");
        Ok(())
    }
}

pub(super) struct PendingTunnel {
    child: std::process::Child,
    pub identity: ProcessIdentity,
    detached: bool,
}

impl PendingTunnel {
    pub fn spawn(mut command: Command, log_file: &Path) -> Result<Self> {
        if let Some(parent) = log_file.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let log = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(log_file)?;
        log.set_permissions(fs::Permissions::from_mode(0o600))?;
        command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .context("failed to start native TNG tunnel")?;
        match ProcessIdentity::capture(child.id()) {
            Ok(identity) => Ok(Self {
                child,
                identity,
                detached: false,
            }),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                Err(error).context("TNG exited before its process identity could be recorded")
            }
        }
    }

    pub fn detach(mut self) {
        self.detached = true;
    }
}

impl Drop for PendingTunnel {
    fn drop(&mut self) {
        if !self.detached {
            let _ = self.identity.stop();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sleeper(log: &Path) -> PendingTunnel {
        let mut command = Command::new("sleep");
        command.arg("60");
        PendingTunnel::spawn(command, log).unwrap()
    }

    #[test]
    fn stale_identity_cannot_stop_a_live_process() {
        let dir = tempfile::tempdir().unwrap();
        let tunnel = sleeper(&dir.path().join("tng.log"));
        let mut stale = tunnel.identity.clone();
        stale.start_ticks += 1;
        stale.stop().unwrap();
        assert!(tunnel.identity.running());
        stale = tunnel.identity.clone();
        stale.boot_id = "previous-boot".into();
        stale.stop().unwrap();
        assert!(tunnel.identity.running());
    }

    #[test]
    fn failed_start_scope_reaps_process_and_logs_are_private() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("tng.log");
        let tunnel = sleeper(&log);
        let identity = tunnel.identity.clone();
        assert_eq!(
            fs::metadata(log).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(tunnel);
        assert!(!identity.running());
        identity.stop().unwrap();
    }

    #[test]
    fn detached_process_survives_launcher_and_stops_idempotently() {
        let dir = tempfile::tempdir().unwrap();
        let tunnel = sleeper(&dir.path().join("tng.log"));
        let identity = tunnel.identity.clone();
        tunnel.detach();
        assert!(identity.running());
        identity.stop().unwrap();
        identity.stop().unwrap();
        unsafe {
            libc::waitpid(identity.pid as i32, std::ptr::null_mut(), 0);
        }
    }
}
