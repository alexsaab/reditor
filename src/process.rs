use anyhow::{Result, bail};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// Drain both pipes concurrently to prevent deadlocks. Retain bounded output.
pub fn run(command: Command, input: String, timeout: Duration) -> Result<(bool, String, String)> {
    run_cancellable(command, input, timeout, Arc::new(AtomicBool::new(false)))
}

pub fn run_cancellable(
    mut command: Command,
    input: String,
    timeout: Duration,
    cancel: Arc<AtomicBool>,
) -> Result<(bool, String, String)> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().unwrap();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
    });
    fn drain(mut pipe: impl Read) -> String {
        let mut retained = Vec::new();
        let mut chunk = [0; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let count = n.min((2 * 1024 * 1024_usize).saturating_sub(retained.len()));
                    retained.extend_from_slice(&chunk[..count]);
                }
            }
        }
        String::from_utf8_lossy(&retained).into_owned()
    }
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out = thread::spawn(move || drain(stdout));
    let err = thread::spawn(move || drain(stderr));
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= timeout || cancel.load(Ordering::Relaxed) {
            #[cfg(unix)]
            // Give renderer processes a chance to close their headless browser.
            unsafe {
                // SAFETY: negative PID targets the group created above for this child.
                libc::kill(-(child.id() as i32), libc::SIGTERM);
            }
            let grace = Instant::now();
            while grace.elapsed() < Duration::from_millis(300) {
                if child.try_wait()?.is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            #[cfg(unix)]
            unsafe {
                // SAFETY: this is the same child's isolated process group.
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let _ = writer.join();
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    let Some(status) = status else {
        if cancel.load(Ordering::Relaxed) {
            bail!("Process cancelled");
        }
        bail!("Process timed out");
    };
    Ok((status.success(), stdout, stderr))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drains_stdout_and_stderr_without_deadlock() -> Result<()> {
        let mut command = Command::new("node");
        command.args([
            "-e",
            "process.stdout.write('x'.repeat(1000000)); process.stderr.write('y'.repeat(1000000));",
        ]);
        let (success, out, err) = run(command, String::new(), Duration::from_secs(5))?;
        assert!(success);
        assert_eq!(out.len(), 1000000);
        assert_eq!(err.len(), 1000000);
        Ok(())
    }
    #[test]
    fn cancellation_terminates_process() {
        let mut command = Command::new("node");
        command.args(["-e", "setInterval(() => {}, 1000)"]);
        let started = Instant::now();
        let result = run_cancellable(
            command,
            String::new(),
            Duration::from_secs(30),
            Arc::new(AtomicBool::new(true)),
        );
        assert!(result.unwrap_err().to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
