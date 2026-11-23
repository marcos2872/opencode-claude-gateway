use clap::Parser;
use opencode_claude_gateway::{api, autostart, cli, config, daemon, infra, install, stats};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .compact()
        .init();

    let cli = cli::Cli::parse();
    let mut cfg = config::AppConfig::load(cli.config.clone()).map_err(|e| anyhow::anyhow!(e))?;
    if let Some(p) = cli.port {
        cfg.port = p;
    }

    if let Some(cmd) = cli.command {
        return match cmd {
            cli::Command::Install(args) => run_install(cfg, args, cli.config.clone()),
            cli::Command::Claude(args) => run_claude(cfg, args),
        };
    }

    if cli.start {
        return daemon::start(cfg.port, cli.config);
    }
    if cli.stop {
        return daemon::stop();
    }
    if cli.enable {
        return autostart::enable(cli.config);
    }
    if cli.disable {
        return autostart::disable();
    }
    if cli.status {
        return daemon::status();
    }
    if cli.refresh {
        let db = infra::opencode::resolve_db_path(&cfg.opencode_bin);
        let state = api::server::AppState::new(cfg, db);
        let n = state.refresh().await.map_err(|e| anyhow::anyhow!(e))?;
        let aliases = state.aliases.read().await;
        println!(
            "catalog: {n} enabled models, {} gateway aliases",
            aliases.len()
        );
        for a in aliases.iter() {
            println!("  {} -> {}", a.gateway_id, a.opencode_ref);
        }
        return Ok(());
    }
    if cli.serve || cli.daemon_child {
        return serve(cfg).await;
    }

    // No flag: show status + hint.
    daemon::status()?;
    println!();
    println!("usage: ocg --start | --stop | --enable | --disable | --status | --serve");
    println!("       ocg install [--agent-usage] [--browser] | ocg claude [-- <args>]");
    Ok(())
}

/// `ocg install`: seed the config (generated token, aliases commented) and,
/// when asked, install the Claude Code plugins. Never edits `~/.claude`.
fn run_install(
    mut cfg: config::AppConfig,
    args: cli::InstallArgs,
    config_arg: Option<std::path::PathBuf>,
) -> anyhow::Result<()> {
    // Same precedence the loader uses: `--config`, then `OCG_CONFIG`, then the
    // XDG default. `ocg install --config X` must seed X, not the global file.
    let path = config::AppConfig::config_path(config_arg);
    let write = install::install_config(&path, args.force).map_err(|e| anyhow::anyhow!(e))?;
    match &write {
        install::ConfigWrite::Created { .. } => {
            println!("created {}", path.display());
            println!(
                "  auth_token: {} (copy it if you wire a client by hand)",
                write.token()
            );
        }
        install::ConfigWrite::Replaced { backup, .. } => {
            println!("rewrote {} (backup: {})", path.display(), backup.display());
            println!("  auth_token: {}", write.token());
        }
        install::ConfigWrite::Kept { .. } => {
            println!(
                "kept existing {} (use --force to regenerate)",
                path.display()
            );
        }
    }
    // The freshly written value is what a client must send; the in-memory
    // config was loaded before the file existed.
    if !matches!(write, install::ConfigWrite::Kept { .. }) {
        cfg.auth_token = write.token().to_string();
    }
    cfg.port = install::port_of(&cfg);

    let mut plugins = vec![];
    if args.agent_usage {
        plugins.push(install::AGENT_USAGE_PLUGIN.to_string());
    }
    if args.browser {
        plugins.push(install::BROWSER_PLUGIN.to_string());
    }
    if !plugins.is_empty() {
        if !matches!(args.scope.as_str(), "user" | "project" | "local") {
            anyhow::bail!(
                "invalid --scope `{}` (use user, project or local)",
                args.scope
            );
        }
        let plan = install::PluginInstall {
            plugins,
            local: args.local.clone(),
            scope: args.scope.clone(),
            port: cfg.port,
            token: cfg.auth_token.clone(),
        };
        println!("installing plugins from {}", plan.marketplace());
        let installed = install::install_plugins(&plan).map_err(|e| anyhow::anyhow!(e))?;
        for name in &installed {
            println!("  installed {name} (scope {})", plan.scope);
        }
        if args.browser && !matches!(args.scope.as_str(), "user") {
            println!("  note: browser-relay needs `bun` on PATH to start");
        }
    }

    println!();
    println!("next:");
    println!("  ocg --start        # or `ocg --enable` for start-on-login");
    println!("  ocg claude         # opens Claude Code against this gateway");
    println!("  ocg --status       # health + the week's model usage");
    Ok(())
}

/// `ocg claude`: make sure the gateway is up, write a session-only settings
/// file and exec the Claude Code CLI with it.
fn run_claude(cfg: config::AppConfig, args: cli::ClaudeArgs) -> anyhow::Result<()> {
    // Follow the daemon this install manages (pidfile + stored port) when it
    // is up — it may have been started with `--start --port N`; otherwise use
    // the configured port.
    let configured = install::port_of(&cfg);
    let port = daemon::stored_port()
        .filter(|p| daemon::is_healthy(*p))
        .unwrap_or(configured);
    if !daemon::is_healthy(port) {
        println!("gateway not ready on :{port}; starting it…");
        daemon::start(port, None)?;
    }
    let url = install::base_url(port);
    let settings = install::session_settings_path();
    install::write_session_settings(&settings, &url, &cfg.auth_token)
        .map_err(|e| anyhow::anyhow!(e))?;
    let claude_args = install::claude_args(&settings, &cfg.default_model, &args.args);
    eprintln!(
        "ocg claude · gateway {url} · settings {}",
        settings.display()
    );
    install::exec_claude(&claude_args).map_err(|e| anyhow::anyhow!(e))
}

async fn serve(cfg: config::AppConfig) -> anyhow::Result<()> {
    let port = cfg.port;
    let db = infra::opencode::resolve_db_path(&cfg.opencode_bin);
    tracing::info!(db = %db.display(), port, "starting ocg");
    // Usage counters: load the flushed snapshot, then persist on a timer so a
    // restart keeps the week (`ocg --status` reads the same file when down).
    let usage_stats = stats::UsageStats::open(stats::default_path());
    let state = api::server::AppState::new(cfg, db).with_stats(usage_stats.clone());

    // Flush dirty counters every minute; the graceful shutdown below writes
    // one last snapshot.
    let flusher = usage_stats.clone();
    let _flush_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tick.tick().await;
            if flusher.is_dirty() {
                if let Err(e) = flusher.flush() {
                    tracing::warn!(error = %e, "could not flush usage stats");
                }
            }
        }
    });

    // Bind first so a stuck catalog fetch can't block boot; the catalog
    // loads in the background and /health reports degraded until then.
    // No periodic refresh: the catalog is read once at startup (update it
    // by restarting the gateway, or preview with `--refresh`).
    let app = api::server::router(state.clone());
    let addr = format!("127.0.0.1:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    let loader = state.clone();
    tokio::spawn(async move {
        // Retry while the catalog comes back empty: `opencode api` can
        // (re)start the OpenCode service, and the first fetch often returns
        // `data: []` while it warms up. A single read would snapshot
        // `models: 0` until a manual restart (issue #1).
        let n = loader
            .refresh_with_retry(
                api::server::BOOT_CATALOG_ATTEMPTS,
                api::server::BOOT_CATALOG_BACKOFF,
            )
            .await;
        if n > 0 {
            tracing::info!(models = n, "catalog loaded");
        } else {
            tracing::warn!("catalog still empty after retries; /health reports degraded");
        }
    });
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    // Last snapshot for `ocg --status` while the next boot hasn't loaded yet.
    if let Err(e) = usage_stats.flush() {
        tracing::warn!(error = %e, "could not flush usage stats on shutdown");
    }
    Ok(())
}

/// SIGTERM (from `--stop`) and Ctrl-C drain in-flight SSE streams.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = term.recv() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
