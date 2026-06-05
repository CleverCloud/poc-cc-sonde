//! Local monitoring agent.
//!
//! Runs on a monitored VM, samples CPU and RAM usage at a fixed frequency and
//! POSTs them to the monitoring app's ingest endpoint, which keys samples by
//! app id (the URL suffix) and instance id (in the body).
//!
//! Example:
//! ```sh
//! agent \
//!   --endpoint http://monitor.example.com:8080/api/metrics \
//!   --app-id app_1234 \
//!   --instance-id $INSTANCE_ID \
//!   --interval 15
//! ```

use clap::Parser;
use serde::Serialize;
use std::collections::HashMap;
use std::time::Duration;
use sysinfo::System;

#[derive(Parser, Debug)]
#[command(author, version, about = "Local CPU/RAM reporting agent for poc-sonde", long_about = None)]
struct Args {
    /// Base ingest URL; the app id is appended as a path suffix.
    /// e.g. http://monitor:8080/api/metrics  ->  POST .../api/metrics/<app-id>
    #[arg(long, env = "SONDE_ENDPOINT")]
    endpoint: String,

    /// Application id (URL suffix).
    #[arg(long, env = "SONDE_APP_ID")]
    app_id: String,

    /// Instance id reported in the body (defaults to the hostname).
    #[arg(long, env = "SONDE_INSTANCE_ID")]
    instance_id: Option<String>,

    /// Reporting frequency in seconds.
    #[arg(long, env = "SONDE_INTERVAL", default_value_t = 15)]
    interval: u64,

    /// Optional shared secret sent as the `X-Agent-Token` header.
    #[arg(long, env = "SONDE_INGEST_TOKEN")]
    token: Option<String>,
}

#[derive(Serialize)]
struct MetricsPayload {
    instance_id: String,
    metrics: HashMap<String, f64>,
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let instance_id = args.instance_id.clone().unwrap_or_else(hostname);
    let url = format!("{}/{}", args.endpoint.trim_end_matches('/'), args.app_id);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("failed to build HTTP client");

    let mut sys = System::new_all();

    println!(
        "poc-sonde agent: posting CPU/RAM for app '{}' (instance '{}') to {} every {}s",
        args.app_id, instance_id, url, args.interval
    );

    loop {
        // CPU usage requires two refreshes spaced by a minimum interval.
        sys.refresh_cpu_usage();
        tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
        sys.refresh_cpu_usage();
        sys.refresh_memory();

        let cpu = sys.global_cpu_usage() as f64;
        let total = sys.total_memory();
        let memory = if total > 0 {
            sys.used_memory() as f64 / total as f64 * 100.0
        } else {
            0.0
        };

        let mut metrics = HashMap::new();
        metrics.insert("cpu".to_string(), round2(cpu));
        metrics.insert("memory".to_string(), round2(memory));

        let payload = MetricsPayload {
            instance_id: instance_id.clone(),
            metrics,
        };

        let mut req = client.post(&url).json(&payload);
        if let Some(ref token) = args.token {
            req = req.header("X-Agent-Token", token);
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                println!("posted cpu={:.2}% memory={:.2}%", cpu, memory);
            }
            Ok(resp) => {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                eprintln!("ingest rejected: HTTP {} {}", status, body);
            }
            Err(e) => {
                eprintln!("failed to post metrics: {}", e);
            }
        }

        // Account for the CPU sampling delay already slept above.
        let remaining = args
            .interval
            .saturating_mul(1000)
            .saturating_sub(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL.as_millis() as u64);
        tokio::time::sleep(Duration::from_millis(remaining)).await;
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
