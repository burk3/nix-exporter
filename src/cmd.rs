//! Running subprocesses with a timeout.

use anyhow::{bail, Context, Result};
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Kills the child's whole process group (children are spawned as group
/// leaders), so grandchildren such as `ssh` die with it.
fn kill_group(child: &mut Child) {
    // SAFETY: kill(2) has no memory-safety requirements. The child has not
    // been reaped yet, so its pid (and so its process group id) cannot have
    // been reused.
    let r = unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    if r != 0 {
        let _ = child.kill();
    }
}

fn wait(child: &mut Child, desc: &str, timeout: Duration) -> Result<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            kill_group(child);
            let _ = child.wait();
            bail!("{desc} timed out after {timeout:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Runs `cmd` and returns its stdout. Fails on timeout (the child is killed)
/// or a non-zero exit, with stderr in the error.
pub fn output(cmd: &mut Command, timeout: Duration) -> Result<String> {
    let desc = format!("{cmd:?}");
    let mut child = cmd
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawning {desc}"))?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out = thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    // On timeout, return without joining the readers: they finish once the
    // killed group's pipe ends close, but there is no reason to wait for that.
    let status = wait(&mut child, &desc, timeout)?;
    let out = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if !status.success() {
        bail!("{desc} failed with {status}: {}", err.trim());
    }
    Ok(out)
}

/// Runs `cmd` with its output discarded; succeeds only on a zero exit within `timeout`.
/// Used where the child may leave grandchildren (ssh) holding pipes open.
pub fn status(cmd: &mut Command, timeout: Duration) -> Result<()> {
    let desc = format!("{cmd:?}");
    let mut child = cmd
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawning {desc}"))?;
    let status = wait(&mut child, &desc, timeout)?;
    if !status.success() {
        bail!("{desc} failed with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn returns_stdout() {
        assert_eq!(
            output(&mut sh("echo hi"), Duration::from_secs(5)).unwrap(),
            "hi\n"
        );
    }

    #[test]
    fn fails_with_stderr_on_nonzero_exit() {
        let e = output(&mut sh("echo oops >&2; exit 3"), Duration::from_secs(5)).unwrap_err();
        assert!(format!("{e:#}").contains("oops"));
    }

    #[test]
    fn kills_on_timeout() {
        let start = Instant::now();
        let e = status(&mut sh("sleep 5"), Duration::from_millis(200)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(format!("{e}").contains("timed out"));
    }

    #[test]
    fn timeout_kills_grandchildren() {
        let d = tempfile::tempdir().unwrap();
        let pidfile = d.path().join("pid");
        let script = format!("sleep 30 & echo $! > '{}'; sleep 30", pidfile.display());
        let start = Instant::now();
        let e = status(&mut sh(&script), Duration::from_millis(200)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(format!("{e}").contains("timed out"));
        let pid = std::fs::read_to_string(&pidfile).unwrap();
        let stat = format!("/proc/{}/stat", pid.trim());
        // Alive means present and not a zombie: the orphan is reaped by
        // whatever it was reparented to (init, a subreaper, or a sandbox's
        // pid 1), which may take a moment or not happen at all.
        let alive = || {
            std::fs::read_to_string(&stat)
                .map(|s| {
                    let state = s.rsplit_once(')').map(|(_, r)| r.trim_start());
                    !matches!(state, Some(r) if r.starts_with('Z') || r.starts_with('X'))
                })
                .unwrap_or(false)
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(!alive(), "grandchild {} survived", pid.trim());
    }

    #[test]
    fn output_timeout_kills_grandchildren() {
        let start = Instant::now();
        let e = output(&mut sh("sleep 30 & sleep 30"), Duration::from_millis(200)).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(format!("{e}").contains("timed out"));
    }

    #[test]
    fn status_reports_failure() {
        assert!(status(&mut sh("exit 0"), Duration::from_secs(5)).is_ok());
        assert!(status(&mut sh("exit 1"), Duration::from_secs(5)).is_err());
    }
}
