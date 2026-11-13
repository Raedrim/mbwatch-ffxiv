use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::json;
use std::path::Path;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn mock_universalis(server: &MockServer) {
    Mock::given(path("/api/v2/worlds"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!([{"id": 33, "name": "Phoenix"}, {"id": 21, "name": "Ravana"}]),
            ),
        )
        .mount(server)
        .await;
    Mock::given(path("/api/v2/data-centers"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!([{"name": "Light", "region": "Europe", "worlds": [33, 21]}])),
        )
        .mount(server)
        .await;
    Mock::given(path("/api/v2/marketable"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([36115])))
        .mount(server)
        .await;
    Mock::given(path("/items.json"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"36115": {"en": "Moqueca"}, "1": {"en": "Gil"}})),
        )
        .mount(server)
        .await;
    Mock::given(path("/api/v2/aggregated/Phoenix/36115")).respond_with(ResponseTemplate::new(200)
        .set_body_json(json!({"results": [{"itemId": 36115, "hq": {
            "minListing": {"world": {"price": 3000, "worldId": 33}, "dc": {"price": 1750, "worldId": 21}},
            "averageSalePrice": {"world": {"price": 3100.0}},
            "dailySaleVelocity": {"world": {"quantity": 2.5}}}}], "failedItems": []}))).mount(server).await;
    Mock::given(method("GET"))
        .and(path("/api/v2/Light/36115"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"listings": [
            {"pricePerUnit": 1750, "quantity": 12, "worldName": "Ravana", "hq": true}]})),
        )
        .mount(server)
        .await;
}

fn write_config(dir: &Path, webhook: &str) -> std::path::PathBuf {
    let p = dir.join("watchlist.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
home_world: Phoenix
data_center: Light
poll_interval: "60s"
webhooks:
  discord: "{webhook}"
watch:
  - item: moqueca
    hq: true
    alert_below: 1800
"#
        ),
    )
    .unwrap();
    p
}

fn mbwatch(server: &MockServer, dir: &Path) -> Command {
    let mut c = Command::cargo_bin("mbwatch").unwrap();
    c.args(["--universalis-url", &server.uri()])
        .args(["--items-url", &format!("{}/items.json", server.uri())])
        .args(["--cache-dir", dir.join("cache").to_str().unwrap()])
        .args(["--state-file", dir.join("state.json").to_str().unwrap()])
        .env_remove("MBWATCH_CACHE_DIR");
    c
}

#[test]
fn help_lists_subcommands() {
    Command::cargo_bin("mbwatch")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("update-db")
                .and(predicate::str::contains("check"))
                .and(predicate::str::contains("watch")),
        );
}

#[tokio::test(flavor = "multi_thread")]
async fn update_db_then_check_prints_table() {
    let server = MockServer::start().await;
    mock_universalis(&server).await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "http://unused.invalid/hook");

    mbwatch(&server, dir.path())
        .arg("update-db")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 items").and(predicate::str::contains("2 worlds")));
    assert!(dir.path().join("cache/cache.json").exists());

    mbwatch(&server, dir.path())
        .args(["check", "--config", cfg.to_str().unwrap()])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("moqueca")
                .and(predicate::str::contains("3,000"))
                .and(predicate::str::contains("1,750"))
                .and(predicate::str::contains("Ravana"))
                .and(predicate::str::contains("3,100"))
                .and(predicate::str::contains("2.5")),
        )
        .stdout(predicate::str::contains("\x1b").not());
}

#[tokio::test(flavor = "multi_thread")]
async fn check_without_cache_fails_with_hint() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let cfg = write_config(dir.path(), "http://unused.invalid/hook");
    mbwatch(&server, dir.path())
        .args(["check", "--config", cfg.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("update-db"));
}

#[tokio::test(flavor = "multi_thread")]
async fn check_unknown_item_fails() {
    let server = MockServer::start().await;
    mock_universalis(&server).await;
    let dir = tempfile::tempdir().unwrap();
    mbwatch(&server, dir.path())
        .arg("update-db")
        .assert()
        .success();
    let cfg = write_config(dir.path(), "http://unused.invalid/hook");
    let text = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("moqueca", "Mystery Meat");
    std::fs::write(&cfg, text).unwrap();
    mbwatch(&server, dir.path())
        .args(["check", "--config", cfg.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Mystery Meat"));
}

#[test]
fn missing_config_file_fails() {
    Command::cargo_bin("mbwatch")
        .unwrap()
        .args(["check", "--config", "/definitely/not/here.yaml"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("here.yaml"));
}

#[tokio::test(flavor = "multi_thread")]
async fn update_db_http_error_exits_nonzero() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    mbwatch(&server, dir.path())
        .arg("update-db")
        .assert()
        .failure();
    assert!(!dir.path().join("cache/cache.json").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_once_sends_alert_with_env_expanded_webhook_and_dedups_across_runs() {
    let server = MockServer::start().await;
    mock_universalis(&server).await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    mbwatch(&server, dir.path())
        .arg("update-db")
        .assert()
        .success();
    let cfg = write_config(dir.path(), "${E2E_HOOK}");
    let hook = format!("{}/hook", server.uri());

    for _ in 0..2 {
        mbwatch(&server, dir.path())
            .env("E2E_HOOK", &hook)
            .args(["watch", "--once", "--config", cfg.to_str().unwrap()])
            .assert()
            .success();
    }
    let state = std::fs::read_to_string(dir.path().join("state.json")).unwrap();
    assert!(state.contains("36115:hq:21"), "{state}");
    // `expect(1)` is verified when the MockServer drops.
}

#[tokio::test(flavor = "multi_thread")]
async fn watch_without_webhooks_fails() {
    let server = MockServer::start().await;
    mock_universalis(&server).await;
    let dir = tempfile::tempdir().unwrap();
    mbwatch(&server, dir.path())
        .arg("update-db")
        .assert()
        .success();
    let cfg = dir.path().join("w.yaml");
    std::fs::write(
        &cfg,
        "home_world: Phoenix\ndata_center: Light\nwatch:\n  - item: moqueca\n    alert_below: 5\n",
    )
    .unwrap();
    mbwatch(&server, dir.path())
        .args(["watch", "--once", "--config", cfg.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("webhook"));
}
