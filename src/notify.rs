//! Discord / Telegram notifications.

use serde_json::{json, Value};

use crate::config::TelegramConfig;
use crate::evaluator::{Alert, Reason};
use crate::universalis::Listing;

#[derive(Debug, thiserror::Error)]
pub enum NotifyError {
    #[error("{target} returned HTTP {status}")]
    Status { target: &'static str, status: u16 },
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
}

/// `1234567` → `"1,234,567"`.
pub fn format_gil(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn title(alert: &Alert) -> String {
    if alert.hq {
        format!("{} (HQ)", alert.item_name)
    } else {
        alert.item_name.clone()
    }
}

fn world_label(alert: &Alert) -> String {
    alert
        .world_name
        .clone()
        .unwrap_or_else(|| format!("world {}", alert.world_id))
}

fn stack_lines(listings: &[Listing]) -> Vec<String> {
    listings
        .iter()
        .map(|l| {
            format!(
                "{} × {} gil — {}",
                l.quantity,
                format_gil(l.price_per_unit),
                l.world_name.as_deref().unwrap_or("?")
            )
        })
        .collect()
}

fn savings_line(alert: &Alert) -> Option<String> {
    alert.reasons.iter().find_map(|r| match r {
        Reason::Arbitrage { savings_pct, .. } => Some(match alert.home_price {
            Some(h) => format!(
                "{savings_pct:.1}% cheaper than home world ({} gil)",
                format_gil(h)
            ),
            None => format!("{savings_pct:.1}% cheaper than home world"),
        }),
        _ => None,
    })
}

pub fn discord_payload(alert: &Alert, listings: &[Listing]) -> Value {
    let mut fields = vec![
        json!({"name": "World", "value": world_label(alert), "inline": true}),
        json!({"name": "Unit price", "value": format!("{} gil", format_gil(alert.price)), "inline": true}),
    ];
    if !listings.is_empty() {
        fields.push(json!({"name": "Stacks", "value": stack_lines(listings).join("\n")}));
    }
    if let Some(s) = savings_line(alert) {
        fields.push(json!({"name": "Arbitrage savings", "value": s}));
    }
    json!({"embeds": [{
        "title": title(alert),
        "url": format!("https://universalis.app/market/{}", alert.item_id),
        "color": 0x2ecc71,
        "fields": fields,
    }]})
}

pub fn telegram_text(alert: &Alert, listings: &[Listing]) -> String {
    let mut t = format!(
        "{}: {} gil on {}",
        title(alert),
        format_gil(alert.price),
        world_label(alert)
    );
    if let Some(s) = savings_line(alert) {
        t.push_str(&format!("\n{s}"));
    }
    for l in stack_lines(listings) {
        t.push_str(&format!("\n• {l}"));
    }
    t.push_str(&format!(
        "\nhttps://universalis.app/market/{}",
        alert.item_id
    ));
    t
}

#[derive(Debug, Clone, Default)]
pub struct Notifier {
    http: reqwest::Client,
    discord: Option<String>,
    telegram: Option<TelegramConfig>,
}

impl Notifier {
    pub fn new(discord: Option<String>, telegram: Option<TelegramConfig>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(crate::universalis::USER_AGENT)
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .unwrap_or_default();
        Notifier {
            http,
            discord,
            telegram,
        }
    }

    pub fn has_targets(&self) -> bool {
        self.discord.is_some() || self.telegram.is_some()
    }

    async fn post(&self, target: &'static str, url: &str, body: &Value) -> Result<(), NotifyError> {
        let resp = self.http.post(url).json(body).send().await?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(NotifyError::Status {
                target,
                status: status.as_u16(),
            })
        }
    }

    /// Deliver to every configured target. Returns how many succeeded; errors
    /// only if every attempted target failed.
    pub async fn send(&self, alert: &Alert, listings: &[Listing]) -> Result<usize, NotifyError> {
        let mut ok = 0;
        let mut first_err = None;
        let mut record = |r: Result<(), NotifyError>| match r {
            Ok(()) => ok += 1,
            Err(e) => {
                first_err.get_or_insert(e);
            }
        };
        if let Some(url) = &self.discord {
            record(
                self.post("Discord", url, &discord_payload(alert, listings))
                    .await,
            );
        }
        if let Some(tg) = &self.telegram {
            let url = format!(
                "{}/bot{}/sendMessage",
                tg.api_base.trim_end_matches('/'),
                tg.bot_token
            );
            let body = json!({"chat_id": tg.chat_id, "text": telegram_text(alert, listings)});
            record(self.post("Telegram", &url, &body).await);
        }
        match (ok, first_err) {
            (0, Some(e)) => Err(e),
            _ => Ok(ok),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::Reason;
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn alert() -> Alert {
        Alert {
            item_id: 36115,
            item_name: "Moqueca".into(),
            hq: true,
            world_id: 21,
            world_name: Some("Ravana".into()),
            price: 1750,
            home_price: Some(3000),
            reasons: vec![
                Reason::Below { threshold: 1800 },
                Reason::Arbitrage {
                    margin: 0.25,
                    savings_pct: 41.666,
                },
            ],
        }
    }

    fn listings() -> Vec<Listing> {
        vec![
            Listing {
                price_per_unit: 1750,
                quantity: 12,
                world_name: Some("Ravana".into()),
                hq: true,
            },
            Listing {
                price_per_unit: 1800,
                quantity: 3,
                world_name: Some("Phoenix".into()),
                hq: true,
            },
        ]
    }

    #[test]
    fn gil_formatting() {
        assert_eq!(format_gil(0), "0");
        assert_eq!(format_gil(999), "999");
        assert_eq!(format_gil(1000), "1,000");
        assert_eq!(format_gil(1234567), "1,234,567");
    }

    #[test]
    fn discord_embed_structure() {
        let v = discord_payload(&alert(), &listings());
        let e = &v["embeds"][0];
        assert_eq!(e["title"], "Moqueca (HQ)");
        let fields = e["fields"].as_array().unwrap();
        let get = |n: &str| {
            fields
                .iter()
                .find(|f| f["name"] == n)
                .unwrap_or_else(|| panic!("no field {n}"))["value"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(get("World"), "Ravana");
        assert_eq!(get("Unit price"), "1,750 gil");
        let stacks = get("Stacks");
        assert!(stacks.contains("12 × 1,750 gil — Ravana"), "{stacks}");
        assert!(stacks.contains("3 × 1,800 gil — Phoenix"), "{stacks}");
        let arb = get("Arbitrage savings");
        assert!(arb.contains("41.7%") && arb.contains("3,000"), "{arb}");
        assert!(e["url"].as_str().unwrap().ends_with("/36115"));
    }

    #[test]
    fn discord_embed_omits_optional_fields() {
        let mut a = alert();
        a.hq = false;
        a.home_price = None;
        a.reasons = vec![Reason::Below { threshold: 1800 }];
        let v = discord_payload(&a, &[]);
        let e = &v["embeds"][0];
        assert_eq!(e["title"], "Moqueca");
        let names: Vec<_> = e["fields"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["World", "Unit price"]);
    }

    #[test]
    fn telegram_text_mentions_key_facts() {
        let t = telegram_text(&alert(), &listings());
        for needle in ["Moqueca", "HQ", "Ravana", "1,750", "12 ×"] {
            assert!(t.contains(needle), "{needle} missing in {t}");
        }
    }

    #[tokio::test]
    async fn posts_embed_to_discord() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hook"))
            .and(body_partial_json(
                serde_json::json!({"embeds": [{"title": "Moqueca (HQ)"}]}),
            ))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        let n = Notifier::new(Some(format!("{}/hook", server.uri())), None);
        assert!(n.has_targets());
        assert_eq!(n.send(&alert(), &listings()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn posts_to_telegram() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/botTOKEN/sendMessage"))
            .and(body_partial_json(serde_json::json!({"chat_id": "42"})))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;
        let tg = TelegramConfig {
            bot_token: "TOKEN".into(),
            chat_id: "42".into(),
            api_base: server.uri(),
        };
        let n = Notifier::new(None, Some(tg));
        assert_eq!(n.send(&alert(), &[]).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn http_5xx_is_propagated() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        let n = Notifier::new(Some(format!("{}/hook", server.uri())), None);
        let e = n.send(&alert(), &[]).await.unwrap_err();
        assert!(
            matches!(e, NotifyError::Status { status: 500, .. }),
            "{e:?}"
        );
    }

    #[tokio::test]
    async fn partial_failure_still_counts_success() {
        let ok = MockServer::start().await;
        let bad = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(204))
            .mount(&ok)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(502))
            .mount(&bad)
            .await;
        let tg = TelegramConfig {
            bot_token: "T".into(),
            chat_id: "1".into(),
            api_base: bad.uri(),
        };
        let n = Notifier::new(Some(format!("{}/h", ok.uri())), Some(tg));
        assert_eq!(n.send(&alert(), &[]).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn no_targets_sends_nothing() {
        let n = Notifier::new(None, None);
        assert!(!n.has_targets());
        assert_eq!(n.send(&alert(), &[]).await.unwrap(), 0);
    }
}
