//! Auto-start on login: systemd user service (`--enable` / `--disable`).
//!
//! Writes `~/.config/systemd/user/ocg.service` (a copy of the gateway's own
//! executable, run with `--serve`) and flips the `enabled` symlink in the
//! user's systemd units. Strictly separated from the daemon lifecycle:
//! `--enable`/`--disable` never start or stop a running gateway, and
//! `--start`/`--stop` never touch auto-start. Ports/auth come from the
//! config file (shell envs like `OCG_PORT` do not reach the service).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const UNIT_NAME: &str = "ocg.service";

fn unit_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("systemd")
        .join("user")
}

pub fn unit_path() -> PathBuf {
    unit_dir().join(UNIT_NAME)
}

/// True when the unit file is installed (enabled or not).
pub fn is_installed() -> bool {
    unit_path().exists()
}

/// Unit body for the current executable. `config_arg` (from `--config`) is
/// baked in so the service reads the same file the CLI did.
fn unit_body(exe: &Path, config_arg: Option<&Path>) -> String {
    let mut exec = format!("ExecStart=\"{}\" --serve", exe.display());
    if let Some(c) = config_arg {
        exec.push_str(&format!(" --config \"{}\"", c.display()));
    }
    format!(
        "[Unit]\n\
         Description=opencode-claude-gateway (Anthropic-compatible gateway for OpenCode)\n\
         After=default.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         {exec}\n\
         Restart=on-failure\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

fn systemctl(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| {
            anyhow::anyhow!("cannot run `systemctl --user` ({e}); auto-start needs systemd")
        })?;
    if !out.status.success() {
        anyhow::bail!(
            "`systemctl --user {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Enable auto-start: install the unit + `systemctl --user enable` (no start).
pub fn enable(config_arg: Option<PathBuf>) -> anyhow::Result<()> {
    let exe = std::env::current_exe()?;
    fs::create_dir_all(unit_dir())?;
    fs::write(unit_path(), unit_body(&exe, config_arg.as_deref()))?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT_NAME])?;
    println!("auto-start enabled ({})", unit_path().display());
    println!("starts on login; run `ocg --start` to bring it up right now");
    Ok(())
}

/// Disable auto-start: `systemctl --user disable` + remove the unit (no stop).
pub fn disable() -> anyhow::Result<()> {
    if !is_installed() {
        println!("auto-start not enabled (no {})", UNIT_NAME);
        return Ok(());
    }
    // disable first (ignoring failure: the unit may be present but never
    // enabled), then drop the file so nothing tries to load it anymore.
    let _ = systemctl(&["disable", UNIT_NAME]);
    fs::remove_file(unit_path())?;
    systemctl(&["daemon-reload"])?;
    println!("auto-start disabled (removed {})", UNIT_NAME);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_path_lives_under_systemd_user() {
        let p = unit_path();
        assert!(p.ends_with(Path::new("systemd/user").join(UNIT_NAME)));
    }

    #[test]
    fn unit_body_runs_serve_and_wants_login_target() {
        let body = unit_body(Path::new("/home/u/.local/bin/ocg"), None);
        assert!(body.contains("ExecStart=\"/home/u/.local/bin/ocg\" --serve"));
        assert!(body.contains("WantedBy=default.target"));
        assert!(body.contains("Restart=on-failure"));
        assert!(!body.contains("--config"));
    }

    #[test]
    fn unit_body_bakes_config_arg() {
        let body = unit_body(
            Path::new("/bin/ocg"),
            Some(Path::new("/home/u/my-config.toml")),
        );
        assert!(body.contains("--config \"/home/u/my-config.toml\""));
    }
}
