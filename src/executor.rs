use std::process::Output;
use std::time::Duration;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

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
    // kill_on_drop(true) ensures the child process is killed when the Child handle is dropped,
    // which happens on timeout (the future is cancelled and the local is dropped).
    let child = Command::new("sh")
        .args(["-c", command])
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            error!(error = %e, "Failed to spawn command");
            e
        })?;

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
            // `child` is dropped here; kill_on_drop(true) sends SIGKILL to the process group
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
