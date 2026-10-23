//! Locating and driving the Cloudflare `warp-cli` binary.
//!
//! On Linux the WARP client installs `warp-cli` onto `PATH`. The macOS
//! client is a GUI application bundle and keeps `warp-cli` inside the
//! bundle instead, so it has to be resolved explicitly. The macOS client
//! also refuses to run anything until its Terms of Service have been
//! accepted, which requires the global `--accept-tos` flag.

use std::path::PathBuf;
use std::process::Command;

/// Locations of `warp-cli` inside the macOS application bundle.
///
/// Newer builds ship it under `SharedSupport`, older ones under `Resources`.
const MACOS_BUNDLE_LOCATIONS: &[&str] = &[
    "/Applications/Cloudflare WARP.app/Contents/SharedSupport/warp-cli",
    "/Applications/Cloudflare WARP.app/Contents/Resources/warp-cli",
    "/Applications/Cloudflare WARP.app/Contents/MacOS/warp-cli",
];

/// Sub-directories of the bundle that may contain `warp-cli`.
const MACOS_BUNDLE_SUBDIRS: &[&str] = &["SharedSupport", "Resources", "MacOS"];

/// Search `PATH` for an executable named `cmd`.
fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let mut suffixes: Vec<String> = vec![String::new()];
    if cfg!(target_os = "windows") {
        suffixes.push(".exe".to_string());
    }

    for dir in std::env::split_paths(&path) {
        for suffix in &suffixes {
            let candidate = dir.join(format!("{}{}", cmd, suffix));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Locate the `warp-cli` executable for the current platform.
///
/// Returns the absolute path to the binary, or `None` when the WARP client
/// is not installed.
pub fn warp_cli_path() -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        for location in MACOS_BUNDLE_LOCATIONS {
            let path = PathBuf::from(location);
            if path.is_file() {
                return Some(path);
            }
        }

        // The bundle may also live in the user's own Applications directory.
        if let Some(home) = std::env::var_os("HOME") {
            let bundle = PathBuf::from(home)
                .join("Applications")
                .join("Cloudflare WARP.app");
            for subdir in MACOS_BUNDLE_SUBDIRS {
                let path = bundle.join("Contents").join(subdir).join("warp-cli");
                if path.is_file() {
                    return Some(path);
                }
            }
        }
    }

    which("warp-cli")
}

/// Whether the Cloudflare WARP client is installed and usable.
pub fn warp_installed() -> bool {
    warp_cli_path().is_some()
}

/// A human readable representation of the `warp-cli` command.
///
/// On macOS the binary is not on `PATH`, so the resolved absolute path is
/// shown instead of a bare command name.
pub fn warp_cli_display() -> String {
    match warp_cli_path() {
        Some(path) => path.display().to_string(),
        None => "warp-cli".to_string(),
    }
}

/// Build the argument list for a `warp-cli` invocation.
///
/// The macOS client refuses to run any command until the Terms of Service
/// have been accepted, which requires the global `--accept-tos` flag. The
/// flag is only added on macOS so the Linux behaviour stays unchanged.
pub fn warp_cli_args<S: AsRef<str>>(args: &[S]) -> Vec<String> {
    let mut full = Vec::with_capacity(args.len() + 1);
    if cfg!(target_os = "macos") {
        full.push("--accept-tos".to_string());
    }
    full.extend(args.iter().map(|arg| arg.as_ref().to_string()));
    full
}

/// Run `warp-cli` with the platform-appropriate arguments.
pub fn run_warp_cli<S: AsRef<str>>(args: &[S]) -> Result<String, String> {
    let path = warp_cli_path().ok_or_else(|| "warp-cli not found".to_string())?;

    let rendered = warp_cli_args(args).join(" ");

    let output = Command::new(&path)
        .args(warp_cli_args(args))
        .output()
        .map_err(|e| format!("failed to execute {}: {}", path.display(), e))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.is_empty() {
            Err(format!("warp-cli {} failed", rendered))
        } else {
            Err(stderr)
        }
    }
}

/// Parse the tunnel state out of `warp-cli status` output.
///
/// The client reports `Status update: Connected` or `Status update:
/// Disconnected`. A naive "does the output contain `connected`" test also
/// matches `Disconnected`, so the negative case is checked first.
pub fn parse_connected(output: &str) -> bool {
    let lower = output.to_lowercase();
    if lower.contains("disconnected") {
        return false;
    }
    lower.contains("connected")
}

/// Query the current tunnel state.
///
/// Newer macOS builds dropped `warp-cli status` in favour of
/// `warp-cli settings`, so both are tried before giving up.
pub fn warp_status() -> Result<String, String> {
    match run_warp_cli(&["status"]) {
        Ok(output) => Ok(output),
        Err(status_err) => run_warp_cli(&["settings"]).map_err(|_| status_err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_connected_detects_connected_state() {
        assert!(parse_connected("Status update: Connected"));
        assert!(parse_connected("Connected"));
        assert!(parse_connected("Status update: connected"));
    }

    #[test]
    fn parse_connected_does_not_match_disconnected() {
        // A naive "contains connected" check would wrongly report this as
        // connected, which is exactly what the macOS client prints.
        assert!(!parse_connected("Status update: Disconnected"));
        assert!(!parse_connected("Disconnected"));
        assert!(!parse_connected("status update: disconnected"));
    }

    #[test]
    fn parse_connected_handles_unknown_state() {
        assert!(!parse_connected(""));
        assert!(!parse_connected("WARP is not installed"));
    }

    #[test]
    fn warp_cli_args_keeps_the_subcommand() {
        let args = warp_cli_args(&["registration", "new"]);
        assert!(args.ends_with(&["registration".to_string(), "new".to_string()]));
    }

    #[test]
    fn warp_cli_args_adds_accept_tos_on_macos() {
        let args = warp_cli_args(&["connect"]);
        if cfg!(target_os = "macos") {
            assert_eq!(args[0], "--accept-tos");
            assert_eq!(args[1], "connect");
        } else {
            assert_eq!(args, vec!["connect".to_string()]);
        }
    }
}
