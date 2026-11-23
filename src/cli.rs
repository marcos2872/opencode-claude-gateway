//! CLI definition (clap). Side-effect free.

use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

/// ocg: Anthropic-compatible gateway for OpenCode models.
#[derive(Debug, Parser)]
#[command(name = "ocg", version, about)]
// Subcommands coexist with the historical `--flag` interface; a bare
// `--start` must not be mistaken for a stray subcommand argument.
#[command(args_conflicts_with_subcommands = true)]
pub struct Cli {
    /// Subcommand: `install` (config + plugins) or `claude` (launched CLI).
    #[command(subcommand)]
    pub command: Option<Command>,

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

    /// Show gateway status (running/autostart) and the week's model usage.
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

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a ready-to-run config (with a generated auth_token) and,
    /// optionally, install the Claude Code plugins.
    ///
    /// Never edits ~/.claude/settings.json: `ocg claude` is the way in.
    Install(InstallArgs),

    /// Open Claude Code pointed at the gateway, with a session-only settings
    /// file (the global ~/.claude config stays untouched).
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Claude(ClaudeArgs),
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Install the agent-usage mod (OpenCode Go quota in the status line).
    #[arg(long, visible_alias = "agent-use")]
    pub agent_usage: bool,

    /// Install the browser-relay plugin (Chrome tabs via MCP; needs `bun`).
    #[arg(long)]
    pub browser: bool,

    /// Overwrite an existing config (a dated .bak is kept).
    #[arg(long)]
    pub force: bool,

    /// Install plugins from this repository folder instead of the GitHub
    /// marketplace (development).
    #[arg(long, value_name = "DIR")]
    pub local: Option<PathBuf>,

    /// Plugin install scope: user | project | local.
    #[arg(long, default_value = "user")]
    pub scope: String,

    /// Add the `ctrl+x o` shortcut that hides/shows the quota band in
    /// ~/.claude/keybindings.json. On by default with --agent-usage.
    #[arg(long)]
    pub no_keybinding: bool,

    /// Also unbind the engine's own `ctrl+x ctrl+a` for that toggle.
    #[arg(long)]
    pub move_keybinding: bool,
}

#[derive(Debug, Args)]
pub struct ClaudeArgs {
    /// Args forwarded to `claude` (e.g. `-p`, `--resume`, `--name`).
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}
