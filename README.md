# cc-sonde — HTTP Monitoring & Auto-Scaling Application

A Rust-based monitoring application that periodically checks HTTP endpoints and executes shell commands on failure, and optionally drives level-based auto-scaling from Warp 10 metrics.

## Features

- **Periodic HTTP Monitoring**: Configure multiple probes with custom intervals
- **Flexible Checks**: Multiple verification methods per probe:
  - HTTP status code validation
  - Response body substring match
  - Response body regex pattern match (pre-compiled at startup)
  - HTTP header validation
- **WarpScript Probes**: Execute WarpScript queries and auto-scale based on numeric thresholds
  - Multi-level scaling (1, 2, 3, …N levels)
  - Levels must be contiguous integers (no gaps)
  - Automatic level transitions based on metric thresholds
  - `levels = [N, M, ...]` shorthand to share identical config across multiple levels
  - Execute scale-up/scale-down commands per level
  - Manage multiple applications with a single configuration block
  - Optional per-app Warp token
  - `${APP_ID}` and `${WARP_TOKEN}` substitution in WarpScript files and commands
  - WarpScript file read once at startup (restart required to pick up file changes)
  - Each app instance maintains independent state
  - Persisted level is validated on restart; resets to min level if the config changed
- **Failure Actions**: Execute shell commands when checks fail
- **Retry Threshold**: Require N consecutive failures before triggering the failure command (triggers on the Nth failure exactly)
- **Process Group Cleanup**: On command timeout, the entire process group is killed on Linux/macOS — pipelines and sub-shells included
- **Configurable Delays**: Different wait times after success, failure, command success, command failure
- **Concurrent Execution**: All probes run as independent async tasks
- **Graceful Shutdown**: Handles SIGTERM (containers, systemd) and SIGINT (Ctrl+C) for clean task teardown
- **Health Check Endpoint**: Optional HTTP server to expose the application's own liveness
- **State Persistence**: In-memory (default) or Redis-backed persistence across restarts
- **Structured Logging**: Configurable log levels via `RUST_LOG`
- **TOML Configuration**: Human-readable configuration format

## Installation

### Prerequisites

- Rust 1.70 or later
- Cargo (comes with Rust)

### Building from Source

```bash
cd cc-sonde

# Default build (in-memory persistence)
cargo build --release

# With Redis persistence support
cargo build --release --features redis-persistence

# The binary is at ./target/release/cc-sonde
```

## Usage

```bash
# Default: reads config.toml in the current directory
./target/release/cc-sonde

# Custom config file
./target/release/cc-sonde --config /path/to/config.toml

# With built-in liveness endpoint (default port 8080)
./target/release/cc-sonde --healthcheck

# Custom port
./target/release/cc-sonde --healthcheck --healthcheck-port 9090

# Full example
./target/release/cc-sonde --config myconfig.toml --healthcheck --healthcheck-port 3000
```

### Command-Line Options

```
Usage: cc-sonde [OPTIONS]

Options:
      --config <CONFIG>
          Configuration file path [default: config.toml]
      --healthcheck
          Enable health check HTTP server
      --healthcheck-port <HEALTHCHECK_PORT>
          Port for health check server (requires --healthcheck) [default: 8080]
  -h, --help
          Print help
  -V, --version
          Print version
```

### Health Check Endpoint

When `--healthcheck` is enabled, the application starts an HTTP server that answers all requests with:

- **Status**: 200 OK
- **Body**: `Probe is running`

Useful for meta-monitoring the monitoring application itself.

```bash
curl http://localhost:8080
# Probe is running
```

## Configuration

### Healthcheck Probes

```toml
[[healthcheck_probes]]
name = "API Health Check"
url = "https://api.example.com/health"
interval_seconds = 60
on_failure_command = "systemctl restart my-service"
command_timeout_seconds = 30          # Optional, default: 30
delay_after_success_seconds = 300     # Optional, default: interval_seconds
delay_after_failure_seconds = 30      # Optional, default: interval_seconds
delay_after_command_success_seconds = 120  # Optional, default: delay_after_failure_seconds
delay_after_command_failure_seconds = 30   # Optional, default: delay_after_failure_seconds
failure_retries_before_command = 3    # Optional, default: 0

[healthcheck_probes.checks]
expected_status = 200
expected_body_contains = "\"status\":\"ok\""
```

#### Parameters

| Key | Required | Default | Description |
|-----|----------|---------|-------------|
| `name` | yes | — | Descriptive name for the probe |
| `url` | yes* | — | HTTP endpoint to monitor (*required if `apps` not set) |
| `apps` | yes* | — | List of apps to monitor (*required if `url` not set) |
| `interval_seconds` | yes | — | Default interval between runs |
| `on_failure_command` | no | — | Shell command executed when checks fail and the retry threshold is reached |
| `command_timeout_seconds` | no | `30` | Max execution time for the failure command (seconds) |
| `delay_after_success_seconds` | no | `interval_seconds` | Wait time after a successful check |
| `delay_after_failure_seconds` | no | `interval_seconds` | Wait time after a failed check |
| `delay_after_command_success_seconds` | no | `delay_after_failure_seconds` | Wait time after the failure command succeeds |
| `delay_after_command_failure_seconds` | no | `delay_after_failure_seconds` | Wait time after the failure command fails |
| `failure_retries_before_command` | no | `0` | Number of consecutive failures tolerated before executing the command |

**Note:** `url` and `apps` are mutually exclusive.

#### `failure_retries_before_command` semantics

This parameter sets the number of consecutive failures that must accumulate **before** the command is triggered:

| Value | Behaviour |
|-------|-----------|
| `0` (default) | Command executes on the **1st** consecutive failure |
| `1` | Command executes on the **2nd** consecutive failure (1 failure tolerated silently) |
| `N` | Command executes on the **(N+1)th** consecutive failure |

The counter resets to zero on any successful check.

#### Check Types

At least one check must be configured per probe. All configured checks must pass.

| Key | Description |
|-----|-------------|
| `expected_status` | Expected HTTP status code |
| `expected_body_contains` | Substring that must appear in the response body |
| `expected_body_regex` | Regex pattern that must match the response body (compiled once at startup) |
| `expected_header` | Key-value map of HTTP headers that must be present with the given values |

#### Multiple Apps (Healthcheck)

Use `apps` to apply the same probe configuration to multiple endpoints. Each app creates an independent probe instance named `"<probe name> - <app id>"`.

```toml
[[healthcheck_probes]]
name = "App Monitor"
interval_seconds = 60
on_failure_command = "clever restart --app ${APP_ID}"
failure_retries_before_command = 1

[healthcheck_probes.checks]
expected_status = 200

[[healthcheck_probes.apps]]
id = "app_frontend"
url = "https://frontend.example.com/health"

[[healthcheck_probes.apps]]
id = "app_backend"
url = "https://backend.example.com/health"
```

App fields:

| Key | Required | Description |
|-----|----------|-------------|
| `id` | yes | Identifier substituted as `${APP_ID}` in `on_failure_command`. Only alphanumeric, `-`, `_`, `.` allowed. |
| `url` | yes | Health check URL for this app |

### WarpScript Probes (Auto-Scaling)

Monitor Warp 10 metrics and automatically scale applications based on numeric thresholds.

#### Environment Variables

```bash
# Required when WarpScript probes are configured
export WARP_ENDPOINT="https://warp.example.com/api/v0/exec"

# Optional: fallback token for apps without warp_token
export WARP_TOKEN="YOUR_READ_TOKEN"
```

`WARP_ENDPOINT` is validated at startup but logged only at `debug` level to avoid exposing credentials or query parameters that may be embedded in the URL.

#### Configuration Example

```toml
[[warpscript_probes]]
name = "CPU Auto-Scaler"
warpscript_file = "warpscript/cpu_usage.mc2"
interval_seconds = 60
command_timeout_seconds = 45          # Optional, default: 30
delay_after_scale_seconds = 120       # Optional, default: interval_seconds

[[warpscript_probes.apps]]
id = "app_frontend"
warp_token = "READ_TOKEN_FRONTEND"    # Optional: overrides WARP_TOKEN env var

[[warpscript_probes.apps]]
id = "app_backend"
# No warp_token: uses WARP_TOKEN env var

# Level 1: minimum scale
[[warpscript_probes.levels]]
level = 1
scale_up_threshold = 70.0
upscale_command = "clever scale --app ${APP_ID} --min-instances 2"

# Level 2: medium scale
[[warpscript_probes.levels]]
level = 2
scale_up_threshold = 85.0
scale_down_threshold = 50.0
upscale_command = "clever scale --app ${APP_ID} --min-instances 3"
downscale_command = "clever scale --app ${APP_ID} --min-instances 1"

# Level 3: maximum scale
[[warpscript_probes.levels]]
level = 3
scale_down_threshold = 60.0
downscale_command = "clever scale --app ${APP_ID} --min-instances 2"
```

#### Parameters

**Probe:**

| Key | Required | Default | Description |
|-----|----------|---------|-------------|
| `name` | yes | — | Descriptive name |
| `warpscript_file` | yes | — | Path to the `.mc2` file. Read once at startup; restart required to pick up changes. |
| `interval_seconds` | yes | — | Interval between executions |
| `command_timeout_seconds` | no | `30` | Max execution time for scaling commands (seconds) |
| `delay_after_scale_seconds` | no | `interval_seconds` | Wait time after any scaling action |
| `apps` | no | `[]` | List of apps to manage |

**App:**

| Key | Required | Description |
|-----|----------|-------------|
| `id` | yes | Identifier substituted as `${APP_ID}` in the script and commands. Only alphanumeric, `-`, `_`, `.` allowed. |
| `warp_token` | no | Per-app read token (falls back to `WARP_TOKEN` env var) |

**Level:**

At least one level must be defined per probe. Level numbers must be unique **and contiguous** (no gaps allowed — e.g., `1, 2, 3` is valid; `1, 3` is not).

| Key | Required | Description |
|-----|----------|-------------|
| `level` | yes* | Single level number — use `level = N` |
| `levels` | yes* | Multiple level numbers — use `levels = [N, M, ...]` for levels sharing the same config |
| `scale_up_threshold` | no | If value exceeds this, scale up (ignored at max level) |
| `scale_down_threshold` | no | If value drops below this, scale down (ignored at min level) |
| `upscale_command` | no | Command executed when scaling up from this level |
| `downscale_command` | no | Command executed when scaling down from this level |

*`level` and `levels` are mutually exclusive; exactly one must be specified per entry.

#### Sharing Config Across Multiple Levels

When consecutive levels share identical thresholds and commands, use `levels = [N, M, ...]` instead of repeating the block:

```toml
# Before (verbose)
[[warpscript_probes.levels]]
level = 2
scale_down_threshold = 45.0
downscale_command = "clever scale --app ${APP_ID} --flavor XS"

[[warpscript_probes.levels]]
level = 3
scale_down_threshold = 45.0
downscale_command = "clever scale --app ${APP_ID} --flavor XS"

# After (compact)
[[warpscript_probes.levels]]
levels = [2, 3]
scale_down_threshold = 45.0
downscale_command = "clever scale --app ${APP_ID} --flavor XS"
```

Entries are automatically sorted by level number after deserialization regardless of declaration order.

#### How Scaling Works

1. The WarpScript file is read once at startup and cached in memory
2. At each interval, the cached script is sent via HTTP POST to `WARP_ENDPOINT`
3. `${WARP_TOKEN}` in the script is replaced with the app's `warp_token` or the `WARP_TOKEN` env var
4. `${APP_ID}` in the script and in commands is replaced with the app's `id`
5. The last element of the JSON response array is used as the metric value
6. The value is compared against the **current level's** thresholds:
   - `value > scale_up_threshold` → execute `upscale_command`, move to level + 1
   - `value < scale_down_threshold` → execute `downscale_command`, move to level − 1
7. Boundaries: upscale is ignored at max level, downscale is ignored at min level
8. After any scaling action, wait `delay_after_scale_seconds` before the next check
9. Current level is persisted (Redis or in-memory) and restored on restart
10. On restart, if the persisted level no longer exists in the config (e.g., max level reduced), it is automatically reset to the minimum level and a warning is logged

#### Scaling Strategy Tips

- **Hysteresis**: Keep `scale_down_threshold` meaningfully below `scale_up_threshold` to avoid flapping (e.g., up at 70 %, down at 50 %)
- **Cooldown**: Use `delay_after_scale_seconds` to let the system stabilize before re-evaluating
- **Progressive thresholds**: Set higher up-thresholds for higher levels (e.g., 70 % → level 2, 85 % → level 3)
- **Script changes**: The WarpScript file is read once at startup. To apply edits, restart the application.

#### WarpScript File Example

```warpscript
// warpscript/cpu_usage.mc2
// ${WARP_TOKEN} → replaced with the effective read token
// ${APP_ID}     → replaced with the specific app id

'${WARP_TOKEN}' 'token' STORE
'${APP_ID}' 'app' STORE

[
  $token
  'os.cpu'
  { 'app_id' $app }
  NOW 5 m -
  NOW
]
FETCH

[ SWAP bucketizer.mean 0 1 0 ] BUCKETIZE

// Return a single numeric value (e.g., 75.5)
0 GET VALUES 0 GET 0 GET
```

See `config-warpscript-example.toml` for complete multi-level examples.

### Retry Strategies

```toml
[[healthcheck_probes]]
name = "Critical API"
url = "https://api.example.com/health"
interval_seconds = 300
delay_after_success_seconds = 300          # Check every 5 min when healthy
delay_after_failure_seconds = 30           # Fast retry when unhealthy
delay_after_command_success_seconds = 120  # Wait 2 min after restart
delay_after_command_failure_seconds = 30   # Wait 30 s if restart fails
failure_retries_before_command = 2         # Tolerate 2 transient failures; act on the 3rd
on_failure_command = "systemctl restart myservice"

[healthcheck_probes.checks]
expected_status = 200
```

**Delay resolution order:**

| Situation | Delay used |
|-----------|-----------|
| Check succeeded | `delay_after_success_seconds` → `interval_seconds` |
| Check failed (below threshold) | `delay_after_failure_seconds` → `interval_seconds` |
| Failure command succeeded | `delay_after_command_success_seconds` → `delay_after_failure_seconds` |
| Failure command failed | `delay_after_command_failure_seconds` → `delay_after_failure_seconds` |

### Redis Persistence (Optional)

Build with `--features redis-persistence` and provide connection details:

```bash
# Option 1: single URL (takes precedence)
export REDIS_URL="redis://:mypassword@localhost:6379"

# Option 2: individual variables
export REDIS_HOST="localhost"
export REDIS_PORT="6379"           # Optional, default: 6379
export REDIS_PASSWORD="mypassword" # Optional
```

The Redis URL (including any embedded password) is **never written to logs** — only a masked form is logged at startup.

Without Redis configuration, in-memory persistence is used (state lost on restart).

With Redis, each probe instance persists:
- Last execution timestamp
- Current scaling level (WarpScript probes)
- Consecutive failure counter (healthcheck probes)
- Next scheduled execution time

This prevents duplicate checks immediately after a restart and preserves scaling levels across deployments.

**Level validation on restart:** If the persisted scaling level is no longer valid in the current configuration (e.g., you reduced the number of levels), the level is silently clamped to the configured minimum and a `WARN` log entry is emitted. No manual state cleanup is needed.

## Command Execution

All commands (`on_failure_command`, `upscale_command`, `downscale_command`) are run via `sh -c`, so shell operators work:

```toml
# Shell operators
on_failure_command = "clever scale --app ${APP_ID} --flavor S && clever restart --app ${APP_ID}"

# Pipes
on_failure_command = "echo 'Alert' | mail -s 'App down' ops@example.com"
```

`${APP_ID}` is substituted with the app identifier before execution.

### Timeout and Process Cleanup

If a command exceeds `command_timeout_seconds`, **the entire process group is killed** (Linux/macOS). This means:

- Pipelines, sub-shells, and child processes spawned by the command are all terminated — not just the top-level `sh` process
- The timeout is enforced reliably regardless of command complexity

On non-Unix platforms, only the direct child process is killed via `kill_on_drop`.

## Logging

Control verbosity with `RUST_LOG`:

```bash
RUST_LOG=info ./target/release/cc-sonde        # Default
RUST_LOG=debug ./target/release/cc-sonde       # Detailed (includes WARP_ENDPOINT, per-execution details)
RUST_LOG=cc_sonde::config=debug ./target/release/cc-sonde  # Module-level filter
```

Log output format:

```
2024-01-15T10:30:45.123Z  INFO cc_sonde: Starting HTTP monitoring application
2024-01-15T10:30:45.234Z  INFO cc_sonde: Loading configuration config_path="config.toml"
2024-01-15T10:30:45.345Z  INFO cc_sonde::healthcheck_probe: All checks passed probe_name="API Health Check" duration_ms=111
```

**Log level notes:**

| Information | Level |
|-------------|-------|
| `WARP_ENDPOINT` value | `debug` — not logged at `info` to avoid exposing credentials |
| Redis URL | never logged in plain text; masked form logged at `info` at startup only |
| Command strings | `debug` — commands may contain tokens or passwords |
| Scaling decisions, probe results | `info` |
| Internal scheduling details | `debug` |

## Testing

```bash
cargo test

# With output
cargo test -- --nocapture

# Specific test
cargo test test_warpscript_levels_plural_expands
```

## Troubleshooting

| Symptom | Likely cause |
|---------|-------------|
| `Configuration must contain at least one probe` | `healthcheck_probes` array is empty or missing |
| `Probe has no checks configured` | No key defined under `[healthcheck_probes.checks]` |
| `a scaling level entry must specify either level = N or levels = [N, ...]` | WarpScript level block is missing both `level` and `levels` |
| `WarpScript probe '…' has duplicate level number N` | Same level defined twice (including via `levels = [N, N]`) |
| `WarpScript probe '…' levels must be contiguous` | Level numbers have gaps (e.g., `1` and `3` defined but not `2`) |
| `WARP_ENDPOINT environment variable not set` | Required env var missing when WarpScript probes are configured |
| Command times out but child processes keep running | Should not happen on Linux/macOS — the whole process group is killed. On other platforms, only `sh` is killed. |
| Command timeout reached too early | Increase `command_timeout_seconds`; ensure the command doesn't wait for interactive input |
| WarpScript changes not reflected | The script file is read once at startup; restart the application after editing the `.mc2` file |
| Scaling level reset to minimum after restart | The previously persisted level is not in the current config; a `WARN` log entry is emitted. This is expected after removing levels from the configuration. |
| Connection errors | Verify URL reachability, DNS, and firewall rules |

## Security Notes

- Commands execute with the same OS privileges as the application process; consider running as a dedicated low-privilege user
- `${APP_ID}` values are validated at startup: only alphanumeric characters, `-`, `_`, and `.` are allowed, preventing shell injection
- Redis passwords are masked before being logged; the raw URL is never written to any log output
- `WARP_ENDPOINT` is logged only at `debug` level; run with `RUST_LOG=info` (the default) to avoid exposing it in production logs
- Command strings are logged only at `debug` level, as they may contain tokens or passwords

## License

MIT — use at your own discretion.
