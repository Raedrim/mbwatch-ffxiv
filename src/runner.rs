//! Orchestration: `update-db`, `check` and one `watch` poll cycle.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, Utc};
use comfy_table::presets::UTF8_FULL_CONDENSED;
use comfy_table::{Cell, Color, Table};
use owo_colors::OwoColorize;

use crate::cache::{Cache, CacheError};
use crate::config::Config;
use crate::evaluator::{evaluate, Alert};
use crate::notify::{format_gil, Notifier};
use crate::state::State;
use crate::universalis::{Client, ItemAggregate, UniversalisError};

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error(transparent)]
    Universalis(#[from] UniversalisError),
    #[error("failed to download item dictionary from {url}: {message}")]
    ItemDump { url: String, message: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct CheckRow {
    pub item: String,
    pub hq: bool,
    pub home_min: Option<f64>,
    pub dc_min: Option<f64>,
    pub dc_world: Option<String>,
    pub avg_sale: Option<f64>,
    pub velocity: Option<f64>,
}

#[derive(Debug, Default)]
pub struct PollReport {
    /// Alerts that were delivered and recorded in state.
    pub sent: Vec<Alert>,
    /// Item IDs Universalis reported in `failedItems`.
    pub failed_items: Vec<u32>,
    /// Non-fatal problems (webhook failures, Stage 2 lookup failures).
    pub warnings: Vec<String>,
}

/// Download worlds, data centers and the marketable item dictionary, then
/// write the cache atomically into `cache_dir`. Nothing is written on failure.
pub async fn update_db(
    client: &Client,
    items_url: &str,
    cache_dir: &Path,
) -> Result<Cache, RunError> {
    let worlds = client.worlds().await?;
    let data_centers = client.data_centers().await?;
    let marketable: HashSet<u32> = client.marketable().await?.into_iter().collect();
    let names = fetch_item_names(items_url).await?;

    let items: HashMap<u32, String> =
        names.into_iter().filter(|(id, _)| marketable.contains(id)).collect();
    let worlds = worlds.into_iter().map(|w| (w.id, w.name)).collect();
    let cache = Cache::new(items, worlds, data_centers);
    cache.save(cache_dir)?;
    Ok(cache)
}

/// Fetch a Teamcraft-style `items.json` (`{"<id>": {"en": "<name>", ...}}`).
pub async fn fetch_item_names(url: &str) -> Result<HashMap<u32, String>, RunError> {
    let err = |message: String| RunError::ItemDump { url: url.to_string(), message };
    let http = reqwest::Client::builder()
        .user_agent(crate::universalis::USER_AGENT)
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| err(e.to_string()))?;
    let resp = http.get(url).send().await.map_err(|e| err(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(err(format!("HTTP {}", resp.status().as_u16())));
    }
    let raw: HashMap<u32, HashMap<String, serde_json::Value>> =
        resp.json().await.map_err(|e| err(e.to_string()))?;
    Ok(raw
        .into_iter()
        .filter_map(|(id, langs)| {
            let en = langs.get("en")?.as_str()?.trim().to_string();
            (!en.is_empty()).then_some((id, en))
        })
        .collect())
}

struct Resolved {
    home_world_id: u32,
    /// Item ID per watch entry, in config order.
    ids: Vec<u32>,
    /// De-duplicated IDs for the API request, in first-seen order.
    unique_ids: Vec<u32>,
}

fn resolve(cfg: &Config, cache: &Cache) -> Result<Resolved, RunError> {
    let home_world_id = cache.world_id(&cfg.home_world)?;
    if cache.data_center(&cfg.data_center).is_none() {
        return Err(CacheError::UnknownWorld(cfg.data_center.clone()).into());
    }
    let ids = cfg
        .watch
        .iter()
        .map(|w| cache.item_id(&w.item))
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen = HashSet::new();
    let unique_ids = ids.iter().copied().filter(|id| seen.insert(*id)).collect();
    Ok(Resolved { home_world_id, ids, unique_ids })
}

pub async fn check(cfg: &Config, cache: &Cache, client: &Client) -> Result<Vec<CheckRow>, RunError> {
    let r = resolve(cfg, cache)?;
    let batch = client.aggregated(&cfg.home_world, &r.unique_ids).await?;
    let by_id: HashMap<u32, &ItemAggregate> = batch.results.iter().map(|a| (a.item_id, a)).collect();

    Ok(cfg
        .watch
        .iter()
        .zip(&r.ids)
        .map(|(w, id)| {
            let q = by_id.get(id).and_then(|a| a.quality(w.hq));
            let dc_min = q.and_then(|q| q.min_listing.dc);
            CheckRow {
                item: w.item.clone(),
                hq: w.hq,
                home_min: q.and_then(|q| q.min_listing.world).map(|p| p.price),
                dc_min: dc_min.map(|p| p.price),
                dc_world: dc_min
                    .and_then(|p| p.world_id)
                    .and_then(|id| cache.world_name(id))
                    .map(str::to_string),
                avg_sale: q.and_then(|q| q.average_sale_price.world).map(|p| p.price),
                velocity: q.and_then(|q| q.daily_sale_velocity.world).map(|v| v.quantity),
            }
        })
        .collect())
}

fn gil(v: Option<f64>) -> String {
    v.map_or("—".into(), |p| format_gil(p.round() as u64))
}

pub fn render_table(cfg: &Config, rows: &[CheckRow], color: bool) -> String {
    let mut table = Table::new();
    table.load_preset(UTF8_FULL_CONDENSED);
    table.set_header(vec![
        "Item".to_string(),
        "Qual".to_string(),
        format!("{} min", cfg.home_world),
        format!("{} min", cfg.data_center),
        "Cheapest world".to_string(),
        "Avg sale (4d)".to_string(),
        "Sales/day".to_string(),
    ]);
    for r in rows {
        let cheaper = matches!((r.home_min, r.dc_min), (Some(h), Some(d)) if d < h);
        let mut dc_cell = Cell::new(gil(r.dc_min));
        if color && cheaper {
            dc_cell = dc_cell.fg(Color::Green);
        }
        table.add_row(vec![
            Cell::new(&r.item),
            Cell::new(if r.hq { "HQ" } else { "NQ" }),
            Cell::new(gil(r.home_min)),
            dc_cell,
            Cell::new(r.dc_world.as_deref().unwrap_or("—")),
            Cell::new(gil(r.avg_sale)),
            Cell::new(r.velocity.map_or("—".into(), |v| format!("{v:.1}"))),
        ]);
    }
    if color {
        table.enforce_styling();
    } else {
        table.force_no_tty();
    }
    let title = format!("{} → {}", cfg.home_world, cfg.data_center);
    let title = if color { title.bold().to_string() } else { title };
    format!("{title}\n{table}")
}

pub async fn poll_once(
    cfg: &Config,
    cache: &Cache,
    client: &Client,
    notifier: &Notifier,
    state: &mut State,
    now: DateTime<Utc>,
) -> Result<PollReport, RunError> {
    let r = resolve(cfg, cache)?;
    let batch = client.aggregated(&cfg.home_world, &r.unique_ids).await?;
    let by_id: HashMap<u32, &ItemAggregate> = batch.results.iter().map(|a| (a.item_id, a)).collect();
    let mut report = PollReport { failed_items: batch.failed_items.clone(), ..Default::default() };

    for (w, &item_id) in cfg.watch.iter().zip(&r.ids) {
        let Some(stats) = by_id.get(&item_id) else { continue };
        let Some(mut alert) = evaluate(w, item_id, stats, r.home_world_id) else { continue };
        let key = State::key(item_id, alert.hq, alert.world_id);
        if !state.should_alert(&key, alert.price, now, cfg.cooldown) {
            continue;
        }
        alert.world_name = cache.world_name(alert.world_id).map(str::to_string);

        // Stage 2: only for alerts that will actually be sent.
        let listings = match client.listings(&cfg.data_center, item_id, alert.hq).await {
            Ok(l) => l,
            Err(e) => {
                report.warnings.push(format!("listing lookup for {} failed: {e}", w.item));
                Vec::new()
            }
        };
        match notifier.send(&alert, &listings).await {
            Ok(n) if n > 0 => {
                state.record(key, alert.price, now);
                report.sent.push(alert);
            }
            Ok(_) => report.warnings.push("no webhook targets configured".into()),
            Err(e) => report.warnings.push(format!("notification for {} failed: {e}", w.item)),
        }
    }
    Ok(report)
}
