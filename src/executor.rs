use std::process::Output;
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, error, info, warn};
#[cfg(unix)]
extern crate libc;

pub async fn execute_command(
    command: &str,
    timeout_seconds: u64,
) -> Result<Output, Box<dyn std::error::Error>> {
    // Log at debug only: the command string may contain tokens or passwords
    debug!(
        command = %command,
        timeout_seconds = timeout_seconds,
        "Executing command"
    );

    if command.trim().is_empty() {
        return Err("Empty command".into());
    }

    // Spawn through shell to support &&, ||, ;, pipes, etc.
    // kill_on_drop(true) ensures the shell is killed when the Child handle is dropped.
    // process_group(0) places the child in its own process group so that SIGKILL on the
    // group also reaches grandchildren (pipelines, sub-shells) on timeout.
    let mut cmd = Command::new("sh");
    cmd.args(["-c", command]).kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn().map_err(|e| {
        error!(error = %e, "Failed to spawn command");
        e
    })?;

    // Capture PGID before moving `child` into wait_with_output.
    // On Unix with process_group(0), PGID == child PID.
    #[cfg(unix)]
    let pgid = child.id();

    let output = match tokio::time::timeout(
        Duration::from_secs(timeout_seconds),
        child.wait_with_output(),
    )
    .await
    {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => {
            error!(error = %e, "Failed to wait for command");
            return Err(e.into());
        }
        Err(_) => {
            error!(
                timeout_seconds = timeout_seconds,
                "Command execution timed out"
            );
            // kill_on_drop kills `sh`; also kill the entire process group to
            // reach grandchildren (pipelines, sub-shells).
            #[cfg(unix)]
            if let Some(pid) = pgid {
                // SAFETY: kill(2) is always safe to call; we ignore ESRCH if
                // the group already exited.
                unsafe { libc::kill(-(pid as libc::pid_t), libc::SIGKILL) };
            }
            return Err("Command execution timed out".into());
        }
    };

    let exit_code = output.status.code().unwrap_or(-1);

    if output.status.success() {
        info!(exit_code = exit_code, "Command executed successfully");
    } else {
        // stderr only — stdout may contain sensitive data
        let stderr = String::from_utf8_lossy(&output.stderr);
        warn!(
            exit_code = exit_code,
            stderr = %stderr.trim(),
            "Command executed with non-zero exit code"
        );
    }

    Ok(output)
}
