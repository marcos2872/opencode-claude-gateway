//! Daemon lifecycle: --start / --stop / --status.
//! (Auto-start on login is systemd's job: see `autostart`, --enable/--disable.)
//!
//! pidfile: ~/.local/share/opencode-claude-gateway/ocg.pid
//! portfile: ~/.local/share/opencode-claude-gateway/ocg.port
//! log: ~/.local/share/opencode-claude-gateway/ocg.log

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use sysinfo::{Pid, System};

fn dir() -> PathBuf {
    crate::config::AppConfig::data_dir()
}

pub fn pid_file() -> PathBuf {
    dir().join("ocg.pid")
}

pub fn port_file() -> PathBuf {
    dir().join("ocg.port")
}

pub fn log_file() -> PathBuf {
    dir().join("ocg.log")
}

fn read_pid() -> Option<u32> {
    fs::read_to_string(pid_file()).ok()?.trim().parse().ok()
}

fn pid_alive(pid: u32) -> bool {
    let mut sys = System::new();
    sys.refresh_all();
    sys.process(Pid::from_u32(pid)).is_some()
}

/// Single-connection health probe: TCP connect + `GET /health`, expect 200.
/// (Previously this was two separate connects under a confusing name.)
pub fn is_healthy(port: u16) -> bool {
    gateway_healthy(port)
}

fn gateway_healthy(port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;
    let addr: std::net::SocketAddr = match format!("127.0.0.1:{port}").parse() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
    let _ = s.write_all(b"GET /health HTTP/1.0\r\nHost: 127.0.0.1\r\n\r\n");
    let mut buf = [0u8; 1024];
    let Ok(n) = s.read(&mut buf) else {
        return false;
    };
    String::from_utf8_lossy(&buf[..n]).contains("200")
}

/// Write a small state file with owner-only permissions (0600 on unix).
/// The README asks for 0600; `fs::write` alone would follow the umask (0644),
/// and `mode(0o600)` only applies at creation, so enforce afterwards too in
/// case the file pre-existed with wider permissions.
pub(crate) fn write_private(path: PathBuf, content: &str) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    let mut opts = OpenOptions::new();
    opts.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(&path).and_then(|mut f| {
        use std::io::Write;
        f.write_all(content.as_bytes())
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn stored_port() -> Option<u16> {
    fs::read_to_string(port_file()).ok()?.trim().parse().ok()
}

/// Start: spawn detached child running `--serve`, wait for /health.
pub fn start(port: u16, config_arg: Option<PathBuf>) -> anyhow::Result<()> {
    if let Some(pid) = read_pid() {
        if pid_alive(pid) {
            let running = stored_port().unwrap_or(crate::config::DEFAULT_PORT);
            if running != port {
                anyhow::bail!(
                    "ocg already running on :{running} (pid {pid}); \
                     run `ocg --stop` first to move it to :{port}"
                );
            }
            println!("ocg already running (pid {pid}, port {running})");
            print_next_steps(running);
            return Ok(());
        }
        let _ = fs::remove_file(pid_file());
    }
    fs::create_dir_all(dir())?;
    let exe = std::env::current_exe()?;
    let mut log_opts = fs::OpenOptions::new();
    log_opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        log_opts.mode(0o600);
    }
    let log = log_opts.open(log_file())?;
    #[cfg(unix)]
    {
        // `mode` only applies at creation; enforce for pre-existing files too.
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(log_file(), std::fs::Permissions::from_mode(0o600));
    }
    let log_err = log.try_clone()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--serve")
        .arg("--port")
        .arg(port.to_string())
        .arg("--daemon-child");
    if let Some(c) = config_arg {
        cmd.arg("--config").arg(c);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    // Detach from the terminal session (no process-group dependency for MVP).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let child = cmd.spawn()?;
    write_private(pid_file(), &child.id().to_string())?;
    write_private(port_file(), &port.to_string())?;

    // Wait for health (catalog now loads in background, so this is fast).
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if gateway_healthy(port) {
            println!(
                "ocg started on http://127.0.0.1:{port} (pid {})",
                child.id()
            );
            print_next_steps(port);
            return Ok(());
        }
        if let Some(pid) = read_pid() {
            if !pid_alive(pid) {
                break;
            }
        }
    }
    anyhow::bail!(
        "daemon did not become healthy; see {}",
        log_file().display()
    )
}

/// Stop: SIGTERM the pidfile process, clean up.
pub fn stop() -> anyhow::Result<()> {
    let Some(pid) = read_pid() else {
        println!("ocg is not running (no pidfile)");
        return Ok(());
    };
    if !pid_alive(pid) {
        let _ = fs::remove_file(pid_file());
        let _ = fs::remove_file(port_file());
        println!("ocg was not running (stale pidfile removed)");
        return Ok(());
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
    // Give it a moment, then confirm.
    for _ in 0..25 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        if !pid_alive(pid) {
            break;
        }
    }
    if pid_alive(pid) {
        anyhow::bail!("could not stop pid {pid}; kill it manually");
    }
    let _ = fs::remove_file(pid_file());
    let _ = fs::remove_file(port_file());
    println!("ocg stopped (pid {pid} stopped)");
    Ok(())
}

pub fn status() -> anyhow::Result<()> {
    match read_pid() {
        Some(pid) if pid_alive(pid) => {
            let port = stored_port().unwrap_or(crate::config::DEFAULT_PORT);
            let h = if gateway_healthy(port) {
                "healthy"
            } else {
                "unreachable"
            };
            println!(
                "ocg {} · running (pid {pid}, port {port}, {h}) · up {}",
                env!("CARGO_PKG_VERSION"),
                uptime(pid)
            );
        }
        Some(pid) => println!("ocg pidfile exists but pid {pid} is dead"),
        None => println!("ocg is not running"),
    }
    let auto = if crate::autostart::is_installed() {
        "enabled"
    } else {
        "disabled"
    };
    println!("auto-start: {auto}");
    print_usage();
    Ok(())
}

/// Per-model usage block: from the running daemon's memory when it is up (the
/// freshest copy), else from the persisted `ocg.stats.json` (up to one flush
/// interval old), else "nothing yet".
fn print_usage() {
    use crate::stats::{self, UsageStats};
    println!();
    let live = if let Some(pid) = read_pid() {
        if pid_alive(pid) {
            snapshot_from_daemon(stored_port().unwrap_or(crate::config::DEFAULT_PORT))
        } else {
            None
        }
    } else {
        None
    };
    if let Some(snapshot) = live {
        println!("{}", stats::render_report(&snapshot, stats::WINDOW_DAYS));
        return;
    }
    let path = stats::default_path();
    if !path.exists() {
        println!(
            "{}",
            stats::render_report(&stats::Snapshot::default(), stats::WINDOW_DAYS)
        );
        return;
    }
    let stored = UsageStats::open(path);
    println!(
        "{}",
        stats::render_report(&stored.snapshot(stats::WINDOW_DAYS), stats::WINDOW_DAYS)
    );
}

/// One authenticated `GET /status` against the running gateway (it holds the
/// in-memory counters). `None` on any failure: the caller falls back to disk.
fn snapshot_from_daemon(port: u16) -> Option<crate::stats::Snapshot> {
    use crate::stats::Snapshot;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let cfg = crate::config::AppConfig::load(None).ok()?;
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().ok()?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(500)).ok()?;
    let _ = s.set_read_timeout(Some(Duration::from_millis(1500)));
    let auth = if cfg.auth_token.is_empty() {
        String::new()
    } else {
        format!("Authorization: Bearer {}\r\n", cfg.auth_token)
    };
    s.write_all(format!("GET /status HTTP/1.0\r\nHost: 127.0.0.1\r\n{auth}\r\n").as_bytes())
        .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let (head, body) = raw.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.") || !head.contains(" 200 ") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let rows = v.get("rows")?.as_array()?;
    let mut snapshot = Snapshot::default();
    for row in rows {
        let requests = row.get("requests")?.as_u64()?;
        let input = row.get("input")?.as_u64()?;
        let output = row.get("output")?.as_u64()?;
        let cached = row
            .get("cached")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        snapshot.requests = snapshot.requests.saturating_add(requests);
        snapshot.input = snapshot.input.saturating_add(input);
        snapshot.output = snapshot.output.saturating_add(output);
        snapshot.cached = snapshot.cached.saturating_add(cached);
        snapshot.rows.push(crate::stats::Row {
            gateway_model: row.get("gateway")?.as_str()?.to_string(),
            opencode_ref: row.get("ref")?.as_str()?.to_string(),
            totals: crate::stats::Totals {
                requests,
                input,
                output,
                cached,
            },
        });
    }
    Some(snapshot)
}

/// Human-readable age of the daemon, from the pidfile's mtime (the moment the
/// process was recorded as started).
fn uptime(pid: u32) -> String {
    let _ = pid;
    let Ok(meta) = fs::metadata(pid_file()) else {
        return "?".to_string();
    };
    let Ok(modified) = meta.modified() else {
        return "?".to_string();
    };
    let Ok(secs) = std::time::SystemTime::now().duration_since(modified) else {
        return "?".to_string();
    };
    let total = secs.as_secs();
    let (h, m) = (total / 3600, (total % 3600) / 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{total}s")
    }
}

fn print_next_steps(port: u16) {
    println!();
    println!("Claude Code:");
    println!("  export ANTHROPIC_BASE_URL=http://127.0.0.1:{port}");
    println!("  export ANTHROPIC_AUTH_TOKEN=dummy");
    println!("  export CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY=1");
    println!("  claude");
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode_of(path: &PathBuf) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn write_private_creates_0600() {
        let dir = std::env::temp_dir().join("ocg-perm-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("new-{}.txt", std::process::id()));
        let _ = std::fs::remove_file(&path);
        write_private(path.clone(), "secret").unwrap();
        assert_eq!(mode_of(&path), 0o600, "{}", path.display());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "secret");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_private_tightens_preexisting_file() {
        let dir = std::env::temp_dir().join("ocg-perm-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("pre-{}.txt", std::process::id()));
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(mode_of(&path), 0o644);
        write_private(path.clone(), "new").unwrap();
        assert_eq!(mode_of(&path), 0o600, "{}", path.display());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let _ = std::fs::remove_file(&path);
    }
}
