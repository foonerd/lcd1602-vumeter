//! Signals and the system's intent. SIGTERM and SIGINT ask the daemon to
//! leave; SIGHUP asks it to read its settings again. When asked to leave,
//! the daemon looks at systemd's job queue to tell a shutdown or a reboot
//! from a plain stop, so the glass can say which.
//!
//! The one program run goes through `posix_spawnp` and `waitpid` directly:
//! the standard library's spawn refers to glibc 2.39's `pidfd_spawnp`, and
//! a binary that refers to it, even weakly, is refused whole by the loader
//! of Volumio's glibc 2.36.

use std::ffi::CString;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::control::Farewell;

pub const TERM: u32 = 1;
pub const HUP: u32 = 2;

static PENDING: AtomicU32 = AtomicU32::new(0);

extern "C" fn on_signal(sig: libc::c_int) {
    let bit = match sig {
        libc::SIGTERM | libc::SIGINT => TERM,
        libc::SIGHUP => HUP,
        _ => return,
    };
    PENDING.fetch_or(bit, Ordering::SeqCst);
}

/// Catch the three signals; a store into a flag is all the handler does.
pub fn install() {
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut action.sa_mask);
            action.sa_flags = libc::SA_RESTART;
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
    // A plugin that went away mid-write costs a failed write, not the daemon.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

/// The signals since the last call, as bits, cleared.
pub fn take() -> u32 {
    PENDING.swap(0, Ordering::SeqCst)
}

/// Whether the system is on its way down, and how: a reboot, halt or
/// power-off job in systemd's queue. `None` when the stop is the daemon's
/// alone, or when systemd cannot be asked in time.
pub fn pending_shutdown() -> Option<Farewell> {
    let jobs = run_with_timeout(
        &["systemctl", "list-jobs", "--no-legend", "--no-pager"],
        Duration::from_millis(1500),
    )?;
    classify_jobs(&jobs)
}

fn classify_jobs(jobs: &str) -> Option<Farewell> {
    let mut found = None;
    for line in jobs.lines() {
        if line.contains("reboot.target") || line.contains("kexec.target") {
            return Some(Farewell::Reboot);
        }
        if line.contains("poweroff.target")
            || line.contains("halt.target")
            || line.contains("shutdown.target")
        {
            found = Some(Farewell::Shutdown);
        }
    }
    found
}

/// A program's standard output, or `None` when it cannot be started, fails,
/// or outlasts `timeout` (and is then killed). Found on a fixed PATH, with
/// nothing of this process's environment.
fn run_with_timeout(argv: &[&str], timeout: Duration) -> Option<String> {
    let args: Vec<CString> = argv
        .iter()
        .map(|a| CString::new(*a))
        .collect::<Result<_, _>>()
        .ok()?;
    let mut argv_ptrs: Vec<*mut libc::c_char> = args
        .iter()
        .map(|a| a.as_ptr() as *mut libc::c_char)
        .collect();
    argv_ptrs.push(std::ptr::null_mut());
    let path = CString::new("PATH=/usr/sbin:/usr/bin:/sbin:/bin").unwrap();
    let envp = [path.as_ptr() as *mut libc::c_char, std::ptr::null_mut()];
    let devnull = CString::new("/dev/null").unwrap();

    let mut fds = [0 as libc::c_int; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return None;
    }
    let (read_fd, write_fd) = (fds[0], fds[1]);
    let mut pid: libc::pid_t = 0;
    let spawned = unsafe {
        let mut actions: libc::posix_spawn_file_actions_t = std::mem::zeroed();
        libc::posix_spawn_file_actions_init(&mut actions);
        libc::posix_spawn_file_actions_adddup2(&mut actions, write_fd, 1);
        libc::posix_spawn_file_actions_addopen(
            &mut actions,
            0,
            devnull.as_ptr(),
            libc::O_RDONLY,
            0,
        );
        libc::posix_spawn_file_actions_addopen(
            &mut actions,
            2,
            devnull.as_ptr(),
            libc::O_WRONLY,
            0,
        );
        let rc = libc::posix_spawnp(
            &mut pid,
            args[0].as_ptr(),
            &actions,
            std::ptr::null(),
            argv_ptrs.as_ptr(),
            envp.as_ptr(),
        );
        libc::posix_spawn_file_actions_destroy(&mut actions);
        libc::close(write_fd);
        rc == 0
    };
    if !spawned {
        unsafe { libc::close(read_fd) };
        return None;
    }

    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let mut whole = false;
    while Instant::now() < deadline {
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as libc::c_int;
        let mut poll = libc::pollfd {
            fd: read_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let ready = unsafe { libc::poll(&mut poll, 1, left.max(1)) };
        if ready <= 0 {
            continue;
        }
        let n = unsafe { libc::read(read_fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            out.extend_from_slice(&buf[..n as usize]);
        } else if n == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            whole = n == 0;
            break;
        }
    }
    unsafe { libc::close(read_fd) };

    // The program closed its output; it exits right after, or is made to.
    let mut status: libc::c_int = 0;
    let exited = loop {
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if waited == pid {
            break true;
        }
        if waited < 0 || Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if !exited {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, &mut status, 0);
        }
        return None;
    }
    if !whole || !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
        return None;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_job_queue_tells_a_reboot_from_a_power_off() {
        assert_eq!(classify_jobs(""), None);
        assert_eq!(
            classify_jobs("12 reboot.target start waiting\n13 umount.target stop running\n"),
            Some(Farewell::Reboot)
        );
        assert_eq!(
            classify_jobs("12 poweroff.target start waiting\n"),
            Some(Farewell::Shutdown)
        );
        assert_eq!(
            classify_jobs("1 shutdown.target start waiting\n2 reboot.target start waiting\n"),
            Some(Farewell::Reboot),
            "a reboot carries shutdown.target too; the reboot wins"
        );
        assert_eq!(
            classify_jobs("5 lcd1602-vumeter.service stop running\n"),
            None
        );
    }

    #[test]
    fn a_command_that_outlasts_its_time_is_killed() {
        let started = Instant::now();
        let out = run_with_timeout(&["sleep", "5"], Duration::from_millis(100));
        assert_eq!(out, None);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(
            run_with_timeout(&["echo", "hi"], Duration::from_secs(2)),
            Some("hi\n".to_string())
        );
        assert_eq!(
            run_with_timeout(&["false"], Duration::from_secs(2)),
            None,
            "a failing program is no answer"
        );
        assert_eq!(
            run_with_timeout(&["no-such-program-here"], Duration::from_secs(2)),
            None
        );
    }

    #[test]
    fn signals_set_their_bits_once() {
        install();
        assert_eq!(take(), 0);
        unsafe { libc::raise(libc::SIGHUP) };
        assert_eq!(take(), HUP);
        assert_eq!(take(), 0);
    }
}
