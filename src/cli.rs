//! CLI definition (clap). Side-effect free.

use clap::Parser;
use std::path::PathBuf;

/// ocg: Anthropic-compatible gateway for OpenCode models.
#[derive(Debug, Parser)]
#[command(name = "ocg", version, about)]
pub struct Cli {
    /// Start the gateway in the background (http://127.0.0.1:PORT).
    #[arg(long, conflicts_with_all = ["stop", "enable", "disable", "status", "serve", "refresh"])]
    pub start: bool,

    /// Stop the background gateway.
    #[arg(long, conflicts_with_all = ["start", "enable", "disable", "status", "serve", "refresh"])]
    pub stop: bool,

    /// Enable start-on-login (systemd user service). Does not start it now.
    #[arg(long, conflicts_with_all = ["start", "stop", "disable", "status", "serve", "refresh"])]
    pub enable: bool,

    /// Disable start-on-login. Does not stop a running gateway.
    #[arg(long, conflicts_with_all = ["start", "stop", "enable", "status", "serve", "refresh"])]
    pub disable: bool,

    /// Show gateway status (running/autostart).
    #[arg(long, conflicts_with_all = ["start", "stop", "enable", "disable", "serve", "refresh"])]
    pub status: bool,

    /// Run the HTTP server in the foreground (dev mode + service child).
    #[arg(long, conflicts_with_all = ["start", "stop", "enable", "disable", "status", "refresh"])]
    pub serve: bool,

    /// Refresh the model catalog cache and exit.
    #[arg(long)]
    pub refresh: bool,

    /// Port for the gateway. Overrides config file and OCG_PORT.
    #[arg(long, env = "OCG_PORT")]
    pub port: Option<u16>,

    /// Path to config.toml. Defaults to ~/.config/opencode-claude-gateway/config.toml.
    #[arg(long, env = "OCG_CONFIG")]
    pub config: Option<PathBuf>,

    /// Hidden: used internally by --start to spawn the daemon child.
    #[arg(long, hide = true)]
    pub daemon_child: bool,
}
