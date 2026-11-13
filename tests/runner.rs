use std::collections::HashMap;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use mbwatch::cache::{Cache, CacheError, DataCenter};
use mbwatch::config::Config;
use mbwatch::notify::Notifier;
use mbwatch::runner::{self, RunError};
use mbwatch::state::State;
use mbwatch::universalis::Client;
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn cache() -> Cache {
    Cache::new(
        HashMap::from([(36115, "Moqueca".to_string()), (2, "Boring Item".to_string())]),
        HashMap::from([(33, "Phoenix".to_string()), (21, "Ravana".to_string())]),
        vec![DataCenter { name: "Light".into(), region: "Europe".into(), worlds: vec![33, 21] }],
    )
}

fn config(webhook: &str) -> Config {
    let y = format!(
        r#"
home_world: Phoenix
data_center: Light
webhooks:
  discord: "{webhook}"
watch:
  - item: moqueca
    hq: true
    alert_below: 1800
    arbitrage_margin: 0.25
  - item: Boring Item
    hq: true
    alert_below: 100
"#
    );
    Config::from_yaml(&y, &|_| None).unwrap()
}

fn aggregated_body() -> serde_json::Value {
    json!({"results": [
        {"itemId": 36115, "hq": {
            "minListing": {"world": {"price": 3000, "worldId": 33}, "dc": {"price": 1750, "worldId": 21}},
            "averageSalePrice": {"world": {"price": 3100.0}, "dc": {"price": 2000.0}},
            "dailySaleVelocity": {"world": {"quantity": 1.5}, "dc": {"quantity": 6.0}}}},
        {"itemId": 2, "hq": {
            "minListing": {"world": {"price": 5000, "worldId": 33}, "dc": {"price": 4000, "worldId": 21}}}}
    ], "failedItems": [777]})
}

async fn mock_aggregated(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/v2/aggregated/Phoenix/36115,2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(aggregated_body()))
        .mount(server)
        .await;
}

fn listings_body() -> serde_json::Value {
    json!({"listings": [
        {"pricePerUnit": 1750, "quantity": 12, "worldName": "Ravana", "hq": true}]})
}

fn now(h: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, h, 0, 0).unwrap()
}

struct Env {
    server: MockServer,
    client: Client,
    notifier: Notifier,
    cfg: Config,
}

async fn env() -> Env {
    let server = MockServer::start().await;
    let hook = format!("{}/hook", server.uri());
    Env {
        client: Client::new(&server.uri()).unwrap(),
        notifier: Notifier::new(Some(hook.clone()), None),
        cfg: config(&hook),
        server,
    }
}

#[tokio::test]
async fn stage2_only_called_for_triggering_items_and_webhook_includes_stacks() {
    let e = env().await;
    mock_aggregated(&e.server).await;
    Mock::given(method("GET")).and(path("/api/v2/Light/36115"))
        .respond_with(ResponseTemplate::new(200).set_body_json(listings_body()))
        .expect(1).mount(&e.server).await;
    // Boring Item (id 2) never breaches -> Stage 2 must not be called for it.
    Mock::given(method("GET")).and(path("/api/v2/Light/2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(listings_body()))
        .expect(0).mount(&e.server).await;
    Mock::given(method("POST")).and(path("/hook"))
        .and(body_partial_json(json!({"embeds": [{"title": "moqueca (HQ)"}]})))
        .respond_with(ResponseTemplate::new(204)).expect(1).mount(&e.server).await;

    let mut state = State::default();
    let r = runner::poll_once(&e.cfg, &cache(), &e.client, &e.notifier, &mut state, now(0)).await.unwrap();
    assert_eq!(r.sent.len(), 1);
    assert_eq!(r.sent[0].world_name.as_deref(), Some("Ravana"));
    assert_eq!(r.failed_items, vec![777]);
    assert!(state.get(&State::key(36115, true, 21)).is_some());
}

#[tokio::test]
async fn duplicate_suppressed_within_cooldown_then_refires_after() {
    let e = env().await;
    mock_aggregated(&e.server).await;
    Mock::given(method("GET")).and(path("/api/v2/Light/36115"))
        .respond_with(ResponseTemplate::new(200).set_body_json(listings_body()))
        .expect(2).mount(&e.server).await; // cycle 1 and cycle 3 only
    Mock::given(method("POST")).and(path("/hook"))
        .respond_with(ResponseTemplate::new(204)).expect(2).mount(&e.server).await;

    let mut state = State::default();
    let c = cache();
    let r1 = runner::poll_once(&e.cfg, &c, &e.client, &e.notifier, &mut state, now(0)).await.unwrap();
    let r2 = runner::poll_once(&e.cfg, &c, &e.client, &e.notifier, &mut state, now(1)).await.unwrap();
    let r3 = runner::poll_once(&e.cfg, &c, &e.client, &e.notifier, &mut state, now(7)).await.unwrap();
    assert_eq!((r1.sent.len(), r2.sent.len(), r3.sent.len()), (1, 0, 1));
}

#[tokio::test]
async fn failed_webhook_is_not_recorded_and_retried_next_cycle() {
    let e = env().await;
    mock_aggregated(&e.server).await;
    Mock::given(method("GET")).and(path("/api/v2/Light/36115"))
        .respond_with(ResponseTemplate::new(200).set_body_json(listings_body()))
        .mount(&e.server).await;
    Mock::given(method("POST")).and(path("/hook"))
        .respond_with(ResponseTemplate::new(500)).up_to_n_times(1).mount(&e.server).await;
    Mock::given(method("POST")).and(path("/hook"))
        .respond_with(ResponseTemplate::new(204)).mount(&e.server).await;

    let mut state = State::default();
    let c = cache();
    let r1 = runner::poll_once(&e.cfg, &c, &e.client, &e.notifier, &mut state, now(0)).await.unwrap();
    assert!(r1.sent.is_empty());
    assert_eq!(r1.warnings.len(), 1);
    assert!(state.get(&State::key(36115, true, 21)).is_none());
    let r2 = runner::poll_once(&e.cfg, &c, &e.client, &e.notifier, &mut state, now(0)).await.unwrap();
    assert_eq!(r2.sent.len(), 1);
}

#[tokio::test]
async fn stage2_failure_degrades_gracefully() {
    let e = env().await;
    mock_aggregated(&e.server).await;
    Mock::given(method("GET")).and(path("/api/v2/Light/36115"))
        .respond_with(ResponseTemplate::new(429)).mount(&e.server).await;
    Mock::given(method("POST")).and(path("/hook"))
        .respond_with(ResponseTemplate::new(204)).expect(1).mount(&e.server).await;
    let mut state = State::default();
    let r = runner::poll_once(&e.cfg, &cache(), &e.client, &e.notifier, &mut state, now(0)).await.unwrap();
    assert_eq!(r.sent.len(), 1);
    assert!(r.warnings.iter().any(|w| w.contains("429")));
}

#[tokio::test]
async fn unknown_item_name_is_a_typed_error() {
    let e = env().await;
    let mut cfg = e.cfg.clone();
    cfg.watch[0].item = "Does Not Exist".into();
    let mut state = State::default();
    let err = runner::poll_once(&cfg, &cache(), &e.client, &e.notifier, &mut state, now(0)).await.unwrap_err();
    assert!(matches!(err, RunError::Cache(CacheError::UnknownItem(_))), "{err:?}");
}

#[tokio::test]
async fn aggregated_rate_limit_is_fatal_for_the_cycle_only() {
    let e = env().await;
    Mock::given(method("GET")).respond_with(ResponseTemplate::new(429)).mount(&e.server).await;
    let mut state = State::default();
    let err = runner::poll_once(&e.cfg, &cache(), &e.client, &e.notifier, &mut state, now(0)).await.unwrap_err();
    assert!(matches!(err, RunError::Universalis(_)));
}

#[tokio::test]
async fn check_builds_rows_and_table() {
    let e = env().await;
    mock_aggregated(&e.server).await;
    let rows = runner::check(&e.cfg, &cache(), &e.client).await.unwrap();
    assert_eq!(rows.len(), 2);
    let r = &rows[0];
    assert_eq!((r.home_min, r.dc_min, r.dc_world.as_deref()), (Some(3000.0), Some(1750.0), Some("Ravana")));
    assert_eq!((r.avg_sale, r.velocity), (Some(3100.0), Some(1.5)));

    let table = runner::render_table(&e.cfg, &rows, false);
    for needle in ["moqueca", "3,000", "1,750", "Ravana", "3,100", "1.5", "Boring Item", "Phoenix", "Light"] {
        assert!(table.contains(needle), "{needle} missing:\n{table}");
    }
    assert!(!table.contains('\x1b'), "no ANSI when color=false");
    assert!(runner::render_table(&e.cfg, &rows, true).contains('\x1b'));
}

#[tokio::test]
async fn check_shows_dash_for_missing_data() {
    let e = env().await;
    Mock::given(method("GET")).and(path("/api/v2/aggregated/Phoenix/36115,2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results": [], "failedItems": [2]})))
        .mount(&e.server).await;
    let rows = runner::check(&e.cfg, &cache(), &e.client).await.unwrap();
    assert!(rows.iter().all(|r| r.home_min.is_none()));
    assert!(runner::render_table(&e.cfg, &rows, false).contains('—'));
}

#[tokio::test]
async fn update_db_writes_cache_with_only_marketable_items() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v2/worlds")).respond_with(ResponseTemplate::new(200)
        .set_body_json(json!([{"id": 33, "name": "Phoenix"}]))).mount(&server).await;
    Mock::given(path("/api/v2/data-centers")).respond_with(ResponseTemplate::new(200)
        .set_body_json(json!([{"name": "Light", "region": "Europe", "worlds": [33]}]))).mount(&server).await;
    Mock::given(path("/api/v2/marketable")).respond_with(ResponseTemplate::new(200)
        .set_body_json(json!([36115]))).mount(&server).await;
    Mock::given(path("/items.json")).respond_with(ResponseTemplate::new(200).set_body_json(json!({
        "1": {"en": "Gil", "de": "Gil"},
        "36115": {"en": "Moqueca", "fr": "Moqueca"}}))).mount(&server).await;

    let dir = tempfile::tempdir().unwrap();
    let client = Client::new(&server.uri()).unwrap();
    runner::update_db(&client, &format!("{}/items.json", server.uri()), dir.path()).await.unwrap();
    let c = Cache::load(dir.path()).unwrap();
    assert_eq!(c.item_id("moqueca").unwrap(), 36115);
    assert!(c.item_id("Gil").is_err());
    assert_eq!(c.world_name(33), Some("Phoenix"));
    assert_eq!(c.data_center("Light").unwrap().worlds, vec![33]);
}

#[tokio::test]
async fn update_db_failure_leaves_existing_cache_untouched() {
    let server = MockServer::start().await;
    Mock::given(path("/api/v2/worlds")).respond_with(ResponseTemplate::new(500)).mount(&server).await;
    let dir = tempfile::tempdir().unwrap();
    cache().save(dir.path()).unwrap();
    let client = Client::new(&server.uri()).unwrap();
    assert!(runner::update_db(&client, &format!("{}/items.json", server.uri()), dir.path()).await.is_err());
    assert_eq!(Cache::load(dir.path()).unwrap().item_id("Moqueca").unwrap(), 36115);
}

#[allow(dead_code)]
fn _unused(_: Duration) {}
