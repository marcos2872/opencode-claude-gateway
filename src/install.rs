//! Install path: seed a ready-to-run config, install the Claude Code plugins
//! and launch the CLI against the gateway.
//!
//! Three surfaces, kept deliberately separate:
//!
//! - [`install_config`] writes `config.toml` as a **direct copy** of the
//!   shipped `config.example.toml`, substituting only a generated `auth_token`
//!   so the client has a credential from the first request. It never
//!   overwrites an existing config unless `--force` (which keeps a `.bak`).
//! - [`install_plugin`] shells out to `claude plugin install <name>
//!   --marketplace <repo>`: the repository already ships a marketplace, so a
//!   release binary (no `plugins/` beside it) can install the mods too.
//! - [`launch_claude`] writes a session-only settings file and execs
//!   `claude --settings <file>`, leaving `~/.claude/settings.json` untouched.
//!   That keeps the global Claude config intact; pointing `~/.claude` at the
//!   gateway stays a documented, explicit alternative.

use crate::config::{AppConfig, DEFAULT_PORT};
use std::io::Write;
use std::path::{Path, PathBuf};

/// GitHub marketplace the release binary installs plugins from.
pub const MARKETPLACE: &str = "marcos2872/opencode-claude-gateway";
pub const AGENT_USAGE_PLUGIN: &str = "agent-usage";
pub const BROWSER_PLUGIN: &str = "browser-relay";

/// Installable config: the shipped `config.example.toml` **verbatim**, with the
/// generated `auth_token` substituted (`INSTALL_CONFIG_ANCHOR` marks the spot).
/// A direct copy so the aliases the example pins are exactly what a fresh
/// install gets and there is a single source of truth to maintain.
pub const INSTALL_TEMPLATE: &str = include_str!("../config.example.toml");

/// The example's `auth_token` line, replaced on install. Matched with the
/// surrounding newline so a doc comment mentioning the key can't be hit.
const INSTALL_CONFIG_ANCHOR: &str = "\nauth_token = \"\"\n";

/// `INSTALL_TEMPLATE` with the generated token substituted.
pub fn install_config_text(token: &str) -> String {
    let anchor = format!("\nauth_token = \"{token}\"\n");
    // Fail loudly if the example ever renames the key: a silent no-substitution
    // would ship a config whose token no client knows.
    assert!(
        INSTALL_TEMPLATE.contains(INSTALL_CONFIG_ANCHOR),
        "config.example.toml must contain a top-level `auth_token = \"\"` line"
    );
    INSTALL_TEMPLATE.replacen(INSTALL_CONFIG_ANCHOR, &anchor, 1)
}

/// A gateway credential. 122 random bits, RFC 4122 v4 shape, hex-only so it
/// survives every client's env/file quoting unchanged.
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// What `install_config` did to the target path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigWrite {
    Created {
        path: PathBuf,
        token: String,
    },
    Replaced {
        path: PathBuf,
        backup: PathBuf,
        token: String,
    },
    Kept {
        path: PathBuf,
        token: String,
    },
}

impl ConfigWrite {
    pub fn path(&self) -> &Path {
        match self {
            ConfigWrite::Created { path, .. }
            | ConfigWrite::Replaced { path, .. }
            | ConfigWrite::Kept { path, .. } => path,
        }
    }

    pub fn token(&self) -> &str {
        match self {
            ConfigWrite::Created { token, .. }
            | ConfigWrite::Replaced { token, .. }
            | ConfigWrite::Kept { token, .. } => token,
        }
    }
}

/// Create the config at `path` with a fresh token. An existing file is left
/// alone unless `force`, in which case it is copied to a dated `.bak` first.
/// The returned token is always the one a client should send: the fresh one
/// for a new file, the file's own `auth_token` when kept.
pub fn install_config(path: &Path, force: bool) -> Result<ConfigWrite, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let token = generate_token();
    let body = install_config_text(&token);
    if path.exists() {
        if !force {
            let existing = read_existing_token(path);
            return Ok(ConfigWrite::Kept {
                path: path.to_path_buf(),
                token: existing.unwrap_or(token),
            });
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backup = path.with_extension(format!("toml.bak.{stamp}"));
        std::fs::copy(path, &backup)
            .map_err(|e| format!("cannot back up {}: {e}", path.display()))?;
        write_private(path, body.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        return Ok(ConfigWrite::Replaced {
            path: path.to_path_buf(),
            backup,
            token,
        });
    }
    write_private(path, body.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(ConfigWrite::Created {
        path: path.to_path_buf(),
        token,
    })
}

/// The `auth_token` already configured, so `ocg install` on an existing config
/// hands the plugins the credential that is actually in use.
fn read_existing_token(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let cfg: AppConfig = toml::from_str(&text).ok()?;
    if cfg.auth_token.is_empty() {
        None
    } else {
        Some(cfg.auth_token)
    }
}

fn write_private(path: &Path, body: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path)?;
    file.write_all(body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Everything a plugin install needs: which plugins, from where, under which
/// scope, and the gateway values the mod polls.
#[derive(Debug, Clone)]
pub struct PluginInstall {
    pub plugins: Vec<String>,
    /// `--local`: install from the repository folder instead of GitHub.
    pub local: Option<PathBuf>,
    pub scope: String,
    pub port: u16,
    pub token: String,
}

impl PluginInstall {
    /// Marketplace source for `claude plugin install --marketplace`.
    pub fn marketplace(&self) -> String {
        match &self.local {
            Some(dir) => dir.display().to_string(),
            None => MARKETPLACE.to_string(),
        }
    }

    /// The `claude plugin install` argv for one plugin.
    pub fn argv(&self, plugin: &str) -> Vec<String> {
        let mut args = vec![
            "plugin".to_string(),
            "install".to_string(),
            plugin.to_string(),
            "--marketplace".to_string(),
            self.marketplace(),
            "--scope".to_string(),
            self.scope.clone(),
            "-y".to_string(),
            // Pin the values this install actually uses, so `/plugin configure`
            // shows the truth instead of the manifest defaults.
            "--config".to_string(),
            format!("base_url=http://127.0.0.1:{}", self.port),
        ];
        if !self.token.is_empty() {
            args.push("--config".to_string());
            args.push(format!("auth_token={}", self.token));
        }
        args
    }
}

/// Run `claude plugin install` for each requested plugin, in order, stopping
/// at the first failure (a half-installed marketplace is worse than none).
pub fn install_plugins(install: &PluginInstall) -> Result<Vec<String>, String> {
    let mut installed = vec![];
    for plugin in &install.plugins {
        let args = install.argv(plugin);
        let status = std::process::Command::new("claude")
            .args(&args)
            .status()
            .map_err(|e| format!("cannot run `claude` (is it on PATH?): {e}"))?;
        if !status.success() {
            return Err(format!(
                "`claude plugin install {plugin}` failed ({status}); installed so far: {installed:?}"
            ));
        }
        installed.push(plugin.clone());
    }
    Ok(installed)
}

/// The session settings `ocg claude` hands to `claude --settings`. `--settings`
/// sits above `~/.claude/settings.json`, so this never edits the user's global
/// config; it only overrides the gateway endpoint for this invocation.
pub fn session_settings(base_url: &str, token: &str) -> String {
    let effective = if token.is_empty() { "dummy" } else { token };
    serde_json::json!({
        "env": {
            "ANTHROPIC_BASE_URL": base_url,
            "ANTHROPIC_AUTH_TOKEN": effective,
            "CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY": "1",
        }
    })
    .to_string()
}

/// Where `ocg claude` keeps its session settings (0600, data dir).
pub fn session_settings_path() -> PathBuf {
    AppConfig::data_dir().join("claude-settings.json")
}

/// Write the session settings and return their path.
pub fn write_session_settings(path: &Path, base_url: &str, token: &str) -> Result<PathBuf, String> {
    let body = session_settings(base_url, token);
    write_private(path, body.as_bytes())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path.to_path_buf())
}

/// Extra args for `claude`: the session settings, then `--model` when the
/// config pins a default and the caller did not choose one.
pub fn claude_args(settings: &Path, default_model: &str, passthrough: &[String]) -> Vec<String> {
    let mut args = vec!["--settings".to_string(), settings.display().to_string()];
    let has_model = passthrough
        .iter()
        .any(|a| a == "--model" || a.starts_with("--model="));
    if !has_model && !default_model.is_empty() {
        args.push("--model".to_string());
        args.push(default_model.to_string());
    }
    args.extend(passthrough.iter().cloned());
    args
}

/// Replace this process with `claude`, inheriting the terminal. On success it
/// never returns; a spawn error is surfaced as `Err`.
pub fn exec_claude(args: &[String]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new("claude").args(args).exec();
        Err(format!("cannot run `claude` (is it on PATH?): {err}"))
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new("claude")
            .args(args)
            .status()
            .map_err(|e| format!("cannot run `claude` (is it on PATH?): {e}"))?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

/// Base URL for the configured port.
pub fn base_url(port: u16) -> String {
    format!("http://127.0.0.1:{}", port.max(1))
}

/// Effective port for a config (already env-overridden by `AppConfig::load`).
pub fn port_of(cfg: &AppConfig) -> u16 {
    if cfg.port == 0 {
        DEFAULT_PORT
    } else {
        cfg.port
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ocg-install-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn install_text_is_the_shipped_example_with_a_token() {
        let text = install_config_text("abc123");
        let cfg: AppConfig = toml::from_str(&text).expect("install text must parse");
        assert_eq!(cfg.auth_token, "abc123");
        assert_eq!(cfg.port, 3737);
        // Whatever the example pins is what ships: the copy must not filter,
        // reorder or uncomment anything (aliases included).
        let example: AppConfig = toml::from_str(include_str!("../config.example.toml")).unwrap();
        assert_eq!(cfg.aliases, example.aliases);
        assert_eq!(cfg.tiers, example.tiers);
        assert_eq!(cfg.default_model, example.default_model);
        // Only the token differs from the file in the repository.
        assert_eq!(
            text,
            include_str!("../config.example.toml").replacen(
                "\nauth_token = \"\"\n",
                "\nauth_token = \"abc123\"\n",
                1
            ),
        );
    }

    #[test]
    fn every_example_alias_ref_is_qualified() {
        let cfg: AppConfig = toml::from_str(&install_config_text("t")).unwrap();
        for (id, alias) in &cfg.aliases {
            assert!(
                alias.opencode.contains('/'),
                "alias `{id}` must use a `provider/model` ref, got `{}`",
                alias.opencode
            );
        }
    }

    #[test]
    fn generate_token_is_long_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64, "{a}");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()), "{a}");
        assert_ne!(a, b);
    }

    #[test]
    fn install_creates_0600_config_and_keeps_existing() {
        let dir = tmpdir("create");
        let path = dir.join("config.toml");
        let first = install_config(&path, false).unwrap();
        assert!(matches!(first, ConfigWrite::Created { .. }));
        let cfg: AppConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.auth_token, first.token());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Without --force: the file and its token stay as they are.
        let kept = install_config(&path, false).unwrap();
        assert!(matches!(kept, ConfigWrite::Kept { .. }));
        assert_eq!(kept.token(), first.token());
        assert_eq!(
            toml::from_str::<AppConfig>(&std::fs::read_to_string(&path).unwrap())
                .unwrap()
                .auth_token,
            first.token()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn force_replaces_and_backs_up() {
        let dir = tmpdir("force");
        let path = dir.join("config.toml");
        let first = install_config(&path, false).unwrap().token().to_string();
        let replaced = install_config(&path, true).unwrap();
        let ConfigWrite::Replaced { backup, token, .. } = &replaced else {
            panic!("expected Replaced, got {replaced:?}");
        };
        assert_ne!(token, &first);
        assert!(backup.exists());
        assert!(std::fs::read_to_string(backup).unwrap().contains(&first));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plugin_argv_pins_gateway_values() {
        let install = PluginInstall {
            plugins: vec![AGENT_USAGE_PLUGIN.to_string()],
            local: None,
            scope: "user".to_string(),
            port: 4000,
            token: "tok".to_string(),
        };
        let args = install.argv(AGENT_USAGE_PLUGIN);
        assert_eq!(args[0..3], ["plugin", "install", "agent-usage"]);
        assert!(args.contains(&MARKETPLACE.to_string()));
        assert!(args.contains(&"base_url=http://127.0.0.1:4000".to_string()));
        assert!(args.contains(&"auth_token=tok".to_string()));
        assert!(args.contains(&"-y".to_string()));

        let local = PluginInstall {
            local: Some(PathBuf::from("/repo")),
            token: String::new(),
            ..install
        };
        let args = local.argv(BROWSER_PLUGIN);
        assert!(args.contains(&"/repo".to_string()));
        assert!(!args.iter().any(|a| a.starts_with("auth_token=")));
    }

    #[test]
    fn session_settings_carry_gateway_env() {
        let v: serde_json::Value =
            serde_json::from_str(&session_settings("http://127.0.0.1:3737", "tok")).unwrap();
        assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:3737");
        assert_eq!(v["env"]["ANTHROPIC_AUTH_TOKEN"], "tok");
        assert_eq!(v["env"]["CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY"], "1");
        let empty: serde_json::Value =
            serde_json::from_str(&session_settings("http://127.0.0.1:1", "")).unwrap();
        assert_eq!(empty["env"]["ANTHROPIC_AUTH_TOKEN"], "dummy");
    }

    #[test]
    fn claude_args_add_default_model_only_when_unset() {
        let settings = Path::new("/tmp/s.json");
        let args = claude_args(settings, "claude-opus-5", &[]);
        assert_eq!(
            args,
            ["--settings", "/tmp/s.json", "--model", "claude-opus-5"]
        );
        let user_choice = vec!["--model".to_string(), "mine".to_string()];
        let args = claude_args(settings, "claude-opus-5", &user_choice);
        assert_eq!(args, ["--settings", "/tmp/s.json", "--model", "mine"]);
        let args = claude_args(settings, "", &["-p".to_string(), "hi".to_string()]);
        assert_eq!(args, ["--settings", "/tmp/s.json", "-p", "hi"]);
    }
}
