use crate::config::Probe;
use crate::executor;
use crate::probe;
use std::time::Duration;
use tokio::time;
use tracing::{error, info};

pub async fn schedule_probe(probe: Probe) {
    let interval = Duration::from_secs(probe.interval_seconds);
    let mut interval_timer = time::interval(interval);

    info!(
        probe_name = %probe.name,
        interval_seconds = probe.interval_seconds,
        "Starting probe scheduler"
    );

    loop {
        interval_timer.tick().await;

        info!(
            probe_name = %probe.name,
            "Executing scheduled probe"
        );

        match probe::execute_probe(&probe).await {
            Ok(_) => {
                info!(
                    probe_name = %probe.name,
                    "Probe succeeded"
                );
            }
            Err(failure) => {
                error!(
                    probe_name = %probe.name,
                    failure = %failure,
                    "Probe failed"
                );

                // Execute failure command if configured
                if let Some(ref command) = probe.on_failure_command {
                    info!(
                        probe_name = %probe.name,
                        command = %command,
                        "Executing failure command"
                    );

                    match executor::execute_command(command, probe.command_timeout_seconds).await {
                        Ok(output) => {
                            if output.status.success() {
                                info!(
                                    probe_name = %probe.name,
                                    "Failure command completed successfully"
                                );
                            } else {
                                error!(
                                    probe_name = %probe.name,
                                    exit_code = output.status.code().unwrap_or(-1),
                                    "Failure command completed with errors"
                                );
                            }
                        }
                        Err(e) => {
                            error!(
                                probe_name = %probe.name,
                                error = %e,
                                "Failed to execute failure command"
                            );
                        }
                    }
                }
            }
        }
    }
}
