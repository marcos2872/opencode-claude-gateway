//! CLI definition (clap). Side-effect free.

use clap::Parser;
use std::path::PathBuf;

/// ocg: Anthropic-compatible gateway for OpenCode models.
#[derive(Debug, Parser)]
#[command(name = "ocg", version, about)]
pub struct Cli {
    /// Start the background daemon (http://127.0.0.1:PORT).
    #[arg(long, conflicts_with_all = ["disable", "status", "serve", "refresh"])]
    pub enable: bool,

    /// Stop the background daemon.
    #[arg(long, conflicts_with_all = ["enable", "status", "serve", "refresh"])]
    pub disable: bool,

    /// Show daemon status.
    #[arg(long, conflicts_with_all = ["enable", "disable", "serve", "refresh"])]
    pub status: bool,

    /// Run the HTTP server in the foreground (dev mode + daemon child).
    #[arg(long, conflicts_with_all = ["enable", "disable", "status", "refresh"])]
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

    /// Hidden: used internally by --enable to spawn the daemon child.
    #[arg(long, hide = true)]
    pub daemon_child: bool,
}
