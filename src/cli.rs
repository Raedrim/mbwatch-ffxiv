//! Command-line interface wiring.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};

use crate::cache::{Cache, CacheError};
use crate::config::Config;
use crate::notify::Notifier;
use crate::runner;
use crate::state::State;
use crate::universalis::Client;

pub const DEFAULT_UNIVERSALIS_URL: &str = "https://universalis.app";
pub const DEFAULT_ITEMS_URL: &str = "https://raw.githubusercontent.com/ffxiv-teamcraft/ffxiv-teamcraft/master/libs/data/src/lib/json/items.json";

#[derive(Debug, Parser)]
#[command(
    name = "mbwatch",
    version,
    about = "FFXIV Market Board CLI & alert daemon (Universalis)"
)]
pub struct Cli {
    /// Directory for the metadata cache [default: $XDG_CACHE_HOME/mbwatch]
    #[arg(long, global = true, env = "MBWATCH_CACHE_DIR")]
    pub cache_dir: Option<PathBuf>,

    /// Alert de-duplication state file [default: <cache-dir>/state.json]
    #[arg(long, global = true, env = "MBWATCH_STATE_FILE")]
    pub state_file: Option<PathBuf>,

    /// Universalis base URL
    #[arg(long, global = true, env = "MBWATCH_UNIVERSALIS_URL", default_value = DEFAULT_UNIVERSALIS_URL)]
    pub universalis_url: String,

    /// URL of the Teamcraft-style items.json used to build the item dictionary
    #[arg(long, global = true, env = "MBWATCH_ITEMS_URL", default_value = DEFAULT_ITEMS_URL)]
    pub items_url: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Download and cache worlds, data centers and the marketable item dictionary
    UpdateDb,
    /// One-shot price comparison table for the watchlist
    Check {
        #[arg(short, long)]
        config: PathBuf,
    },
    /// Poll continuously and send de-duplicated webhook alerts
    Watch {
        #[arg(short, long)]
        config: PathBuf,
        /// Run a single poll cycle and exit
        #[arg(long)]
        once: bool,
    },
}

/// Resolve the default cache directory from environment values.
pub fn default_cache_dir(xdg_cache_home: Option<&str>, home: Option<&str>) -> PathBuf {
    match (
        xdg_cache_home.filter(|s| !s.is_empty()),
        home.filter(|s| !s.is_empty()),
    ) {
        (Some(x), _) => PathBuf::from(x).join("mbwatch"),
        (None, Some(h)) => PathBuf::from(h).join(".cache").join("mbwatch"),
        _ => PathBuf::from(".mbwatch-cache"),
    }
}

fn load_cache(dir: &Path) -> Result<Cache> {
    Cache::load(dir).map_err(|e| match e {
        CacheError::Missing(_) => anyhow::Error::new(e),
        other => {
            anyhow::Error::new(other).context("failed to load cache (try `mbwatch update-db`)")
        }
    })
}

pub async fn run(cli: Cli, out: &mut dyn Write) -> Result<()> {
    let cache_dir = cli.cache_dir.clone().unwrap_or_else(|| {
        default_cache_dir(
            std::env::var("XDG_CACHE_HOME").ok().as_deref(),
            std::env::var("HOME").ok().as_deref(),
        )
    });
    let state_path = cli
        .state_file
        .clone()
        .unwrap_or_else(|| cache_dir.join("state.json"));
    let client = Client::new(&cli.universalis_url)?;

    match cli.command {
        Command::UpdateDb => {
            let cache = runner::update_db(&client, &cli.items_url, &cache_dir).await?;
            writeln!(
                out,
                "Cached {} items, {} worlds, {} data centers in {}",
                cache.items.len(),
                cache.worlds.len(),
                cache.data_centers.len(),
                cache_dir.display()
            )?;
        }
        Command::Check { config } => {
            let cfg = Config::load(&config)?;
            let cache = load_cache(&cache_dir)?;
            let rows = runner::check(&cfg, &cache, &client).await?;
            let color = std::io::stdout().is_terminal();
            writeln!(out, "{}", runner::render_table(&cfg, &rows, color))?;
        }
        Command::Watch { config, once } => {
            let cfg = Config::load(&config)?;
            let notifier =
                Notifier::new(cfg.webhooks.discord.clone(), cfg.webhooks.telegram.clone());
            if !notifier.has_targets() {
                bail!(
                    "no webhook configured: set `webhooks.discord` or `webhooks.telegram` in {}",
                    config.display()
                );
            }
            let cache = load_cache(&cache_dir)?;
            let mut state = State::load(&state_path)?;
            watch_loop(
                &cfg,
                &cache,
                &client,
                &notifier,
                &mut state,
                &state_path,
                once,
                out,
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn watch_loop(
    cfg: &Config,
    cache: &Cache,
    client: &Client,
    notifier: &Notifier,
    state: &mut State,
    state_path: &Path,
    once: bool,
    out: &mut dyn Write,
) -> Result<()> {
    loop {
        let now = Utc::now();
        match runner::poll_once(cfg, cache, client, notifier, state, now).await {
            Ok(report) => {
                for a in &report.sent {
                    writeln!(
                        out,
                        "[{}] alert: {}{} at {} gil on {}",
                        now.format("%Y-%m-%d %H:%M:%S"),
                        a.item_name,
                        if a.hq { " (HQ)" } else { "" },
                        a.price,
                        a.world_name.as_deref().unwrap_or("?")
                    )?;
                }
                for w in &report.warnings {
                    eprintln!("warning: {w}");
                }
                if !report.failed_items.is_empty() {
                    eprintln!(
                        "warning: Universalis failed items: {:?}",
                        report.failed_items
                    );
                }
                if !report.sent.is_empty() {
                    state.save(state_path).context("failed to save state")?;
                }
            }
            Err(e) if once => return Err(e.into()),
            Err(e) => eprintln!("error: poll cycle failed: {e}"),
        }
        if once {
            return Ok(());
        }
        tokio::select! {
            _ = tokio::time::sleep(cfg.poll_interval) => {}
            _ = tokio::signal::ctrl_c() => {
                writeln!(out, "shutting down")?;
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cache_dir_precedence() {
        assert_eq!(
            default_cache_dir(Some("/x"), Some("/h")),
            PathBuf::from("/x/mbwatch")
        );
        assert_eq!(
            default_cache_dir(None, Some("/h")),
            PathBuf::from("/h/.cache/mbwatch")
        );
        assert_eq!(
            default_cache_dir(Some(""), Some("/h")),
            PathBuf::from("/h/.cache/mbwatch")
        );
        assert_eq!(
            default_cache_dir(None, None),
            PathBuf::from(".mbwatch-cache")
        );
    }

    #[test]
    fn parses_args() {
        let c = Cli::try_parse_from(["mbwatch", "watch", "--once", "-c", "w.yaml"]).unwrap();
        assert!(matches!(c.command, Command::Watch { once: true, .. }));
        assert!(Cli::try_parse_from(["mbwatch", "check"]).is_err());
    }
}
