use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{info, warn, error};

use super::cli;

static ACTIVE_429_COUNT: AtomicU32 = AtomicU32::new(0);
static RESET_IN_PROGRESS: Mutex<()> = Mutex::const_new(());

pub struct WarpResolver {
    pub max_retries: u32,
    pub reset_delay_ms: u64,
}

impl WarpResolver {
    pub fn new(max_retries: u32, reset_delay_ms: u64) -> Self {
        Self {
            max_retries,
            reset_delay_ms,
        }
    }

    pub async fn handle_429(&self, retry_count: u32) -> bool {
        if retry_count >= self.max_retries {
            warn!(
                "Max retry attempts ({}) reached, giving up",
                self.max_retries
            );
            return false;
        }

        let _guard = match RESET_IN_PROGRESS.try_lock() {
            Ok(g) => g,
            Err(_) => {
                info!("WARP reset already in progress, waiting...");
                tokio::time::sleep(Duration::from_millis(self.reset_delay_ms)).await;
                return true;
            }
        };

        let count = ACTIVE_429_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
        info!(
            "HTTP 429 detected (count: {}). Initiating WARP reset...",
            count
        );

        if let Err(e) = self.reset_warp().await {
            error!("WARP reset failed: {}", e);
            return false;
        }

        info!("WARP reset complete. Waiting for connection stabilization...");
        tokio::time::sleep(Duration::from_millis(self.reset_delay_ms)).await;

        ACTIVE_429_COUNT.fetch_sub(1, Ordering::SeqCst);
        true
    }

    async fn reset_warp(&self) -> Result<(), String> {
        if cfg!(target_os = "macos") {
            self.reset_warp_macos().await
        } else {
            self.reset_warp_linux().await
        }
    }

    /// macOS runs the WARP client as a launchd daemon inside the app bundle.
    ///
    /// There is no `warp-svc` systemd unit and no `/var/lib/cloudflare-warp`
    /// cache to wipe, so the tunnel is rotated purely through the CLI:
    /// disconnect, re-register to obtain a fresh identity, then reconnect.
    async fn reset_warp_macos(&self) -> Result<(), String> {
        info!("Step 1: warp-cli disconnect");
        run_warp_command(&["disconnect"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 2: warp-cli registration new");
        run_warp_command(&["registration", "new"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 3: warp-cli connect");
        run_warp_command(&["connect"]).await?;

        info!("WARP tunnel reset complete");
        Ok(())
    }

    /// Linux runs the WARP client as the `warp-svc` systemd unit.
    ///
    /// The unit is stopped and its on-disk state wiped before a new
    /// registration is created, which is what forces a fresh tunnel.
    async fn reset_warp_linux(&self) -> Result<(), String> {
        info!("Step 1: warp-cli disconnect");
        run_warp_command(&["disconnect"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 2: systemctl stop warp-svc");
        let stopped = run_sudo_command("systemctl stop warp-svc").await.is_ok();
        if !stopped {
            warn!("Stopping WARP service failed or skipped");
        }

        tokio::time::sleep(Duration::from_millis(500)).await;

        info!("Step 3: Clearing WARP cache");
        if stopped {
            if let Err(e) = run_sudo_command("rm -rf /var/lib/cloudflare-warp/*").await {
                warn!("Clearing WARP cache failed or skipped: {}", e);
            }
        } else {
            warn!("Skipping cache clear: service may still be running");
        }

        tokio::time::sleep(Duration::from_millis(500)).await;

        info!("Step 4: systemctl start warp-svc");
        if let Err(e) = run_sudo_command("systemctl start warp-svc").await {
            warn!("Starting WARP service failed or skipped: {}", e);
        }

        tokio::time::sleep(Duration::from_millis(2000)).await;

        info!("Step 5: warp-cli registration new");
        run_warp_command(&["registration", "new"]).await?;

        tokio::time::sleep(Duration::from_millis(1000)).await;

        info!("Step 6: warp-cli connect");
        run_warp_command(&["connect"]).await?;

        info!("WARP tunnel reset complete");
        Ok(())
    }
}

async fn run_warp_command(args: &[&str]) -> Result<(), String> {
    // `spawn_blocking` requires a `'static` closure, so the arguments are
    // copied into owned strings first.
    let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();

    let output = tokio::task::spawn_blocking(move || cli::run_warp_cli(&args))
        .await
        .map_err(|e| format!("warp-cli task failed: {}", e))?;

    output.map(|_| ())
}

async fn run_sudo_command(cmd: &str) -> Result<(), String> {
    let output = tokio::process::Command::new("sudo")
        .arg("-n")
        .arg("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await
        .map_err(|e| format!("Failed to execute sudo {}: {}", cmd, e))?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("sudo {} failed: {}", cmd, stderr.trim()))
    }
}
