//! Watchlist configuration parsing (`watchlist.yaml`).

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ConfigError {
    #[error("failed to read config file: {0}")]
    Io(String),
    #[error("invalid YAML: {0}")]
    Yaml(String),
    #[error("environment variable `{0}` is not set")]
    MissingEnv(String),
    #[error("invalid duration `{0}` (expected e.g. 30s, 5m, 1h)")]
    InvalidDuration(String),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TelegramConfig {
    pub bot_token: String,
    pub chat_id: String,
    pub api_base: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Webhooks {
    pub discord: Option<String>,
    pub telegram: Option<TelegramConfig>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WatchItem {
    pub item: String,
    pub hq: bool,
    pub alert_below: Option<u64>,
    pub arbitrage_margin: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub home_world: String,
    pub data_center: String,
    pub poll_interval: Duration,
    pub cooldown: Duration,
    pub webhooks: Webhooks,
    pub watch: Vec<WatchItem>,
}

pub fn parse_duration(s: &str) -> Result<Duration, ConfigError> {
    let bad = || ConfigError::InvalidDuration(s.to_string());
    let unit = s.chars().last().ok_or_else(bad)?;
    let mult = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        _ => return Err(bad()),
    };
    let digits = &s[..s.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let n: u64 = digits.parse().map_err(|_| bad())?;
    if n == 0 {
        return Err(bad());
    }
    Ok(Duration::from_secs(n.saturating_mul(mult)))
}

const DEFAULT_POLL: &str = "60s";
const DEFAULT_COOLDOWN: &str = "6h";
const DEFAULT_TELEGRAM_API: &str = "https://api.telegram.org";

#[derive(Deserialize)]
struct RawConfig {
    home_world: String,
    data_center: String,
    poll_interval: Option<String>,
    cooldown: Option<String>,
    #[serde(default)]
    webhooks: RawWebhooks,
    watch: Vec<RawWatch>,
}

#[derive(Deserialize, Default)]
struct RawWebhooks {
    discord: Option<String>,
    telegram: Option<RawTelegram>,
}

#[derive(Deserialize)]
struct RawTelegram {
    bot_token: String,
    chat_id: String,
    api_base: Option<String>,
}

#[derive(Deserialize)]
struct RawWatch {
    item: String,
    #[serde(default)]
    hq: bool,
    alert_below: Option<i64>,
    arbitrage_margin: Option<f64>,
}

/// Recursively expand `${VAR}` in every string of a YAML value.
fn expand_value(
    v: &mut serde_yaml::Value,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(), ConfigError> {
    match v {
        serde_yaml::Value::String(s) => *s = expand_str(s, env)?,
        serde_yaml::Value::Sequence(seq) => {
            for x in seq {
                expand_value(x, env)?;
            }
        }
        serde_yaml::Value::Mapping(m) => {
            for (_, x) in m.iter_mut() {
                expand_value(x, env)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn expand_str(s: &str, env: &dyn Fn(&str) -> Option<String>) -> Result<String, ConfigError> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            // No closing brace: keep literally.
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let name = &after[..end];
        out.push_str(&env(name).ok_or_else(|| ConfigError::MissingEnv(name.to_string()))?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

impl Config {
    /// Parse YAML, expanding `${VAR}` references using `env`.
    pub fn from_yaml(
        yaml: &str,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Config, ConfigError> {
        let mut value: serde_yaml::Value =
            serde_yaml::from_str(yaml).map_err(|e| ConfigError::Yaml(e.to_string()))?;
        expand_value(&mut value, env)?;
        let raw: RawConfig =
            serde_yaml::from_value(value).map_err(|e| ConfigError::Yaml(e.to_string()))?;

        let poll_interval = parse_duration(raw.poll_interval.as_deref().unwrap_or(DEFAULT_POLL))?;
        let cooldown = parse_duration(raw.cooldown.as_deref().unwrap_or(DEFAULT_COOLDOWN))?;

        if raw.watch.is_empty() {
            return Err(ConfigError::Invalid("`watch` must contain at least one item".into()));
        }
        let mut watch = Vec::with_capacity(raw.watch.len());
        for w in raw.watch {
            if w.alert_below.is_none() && w.arbitrage_margin.is_none() {
                return Err(ConfigError::Invalid(format!(
                    "item `{}` needs `alert_below` and/or `arbitrage_margin`",
                    w.item
                )));
            }
            let alert_below = match w.alert_below {
                Some(n) if n < 0 => {
                    return Err(ConfigError::Invalid(format!(
                        "item `{}`: alert_below must not be negative",
                        w.item
                    )))
                }
                Some(n) => Some(n as u64),
                None => None,
            };
            if let Some(m) = w.arbitrage_margin {
                if !(m > 0.0 && m <= 1.0) {
                    return Err(ConfigError::Invalid(format!(
                        "item `{}`: arbitrage_margin must be in (0, 1]",
                        w.item
                    )));
                }
            }
            watch.push(WatchItem {
                item: w.item,
                hq: w.hq,
                alert_below,
                arbitrage_margin: w.arbitrage_margin,
            });
        }

        Ok(Config {
            home_world: raw.home_world,
            data_center: raw.data_center,
            poll_interval,
            cooldown,
            webhooks: Webhooks {
                discord: raw.webhooks.discord,
                telegram: raw.webhooks.telegram.map(|t| TelegramConfig {
                    bot_token: t.bot_token,
                    chat_id: t.chat_id,
                    api_base: t.api_base.unwrap_or_else(|| DEFAULT_TELEGRAM_API.into()),
                }),
            },
            watch,
        })
    }

    /// Load from a file using the process environment.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(format!("{}: {e}", path.display())))?;
        Config::from_yaml(&text, &|k| std::env::var(k).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    const VALID: &str = r#"
home_world: "Phoenix"
data_center: "Light"
poll_interval: "60s"
webhooks:
  discord: "https://discord.test/hook"
watch:
  - item: "Grade 2 Gemdraught of Strength"
    hq: true
    alert_below: 1800
    arbitrage_margin: 0.25
  - item: "Moqueca"
    hq: true
    alert_below: 2200
"#;

    #[test]
    fn parses_valid_yaml() {
        let c = Config::from_yaml(VALID, &no_env).unwrap();
        assert_eq!(c.home_world, "Phoenix");
        assert_eq!(c.data_center, "Light");
        assert_eq!(c.poll_interval, Duration::from_secs(60));
        assert_eq!(c.cooldown, Duration::from_secs(6 * 3600));
        assert_eq!(c.webhooks.discord.as_deref(), Some("https://discord.test/hook"));
        assert_eq!(c.watch.len(), 2);
        assert_eq!(c.watch[0].alert_below, Some(1800));
        assert_eq!(c.watch[0].arbitrage_margin, Some(0.25));
        assert!(c.watch[1].hq);
        assert_eq!(c.watch[1].arbitrage_margin, None);
    }

    #[test]
    fn hq_defaults_to_false_and_poll_interval_defaults() {
        let y = "home_world: A\ndata_center: B\nwatch:\n  - item: X\n    alert_below: 5\n";
        let c = Config::from_yaml(y, &no_env).unwrap();
        assert!(!c.watch[0].hq);
        assert_eq!(c.poll_interval, Duration::from_secs(60));
    }

    #[test]
    fn expands_env_vars() {
        let y = VALID.replace("https://discord.test/hook", "${HOOK}");
        let env = |k: &str| (k == "HOOK").then(|| "https://expanded.test/x".to_string());
        let c = Config::from_yaml(&y, &env).unwrap();
        assert_eq!(c.webhooks.discord.as_deref(), Some("https://expanded.test/x"));
    }

    #[test]
    fn missing_env_var_is_an_error() {
        let y = VALID.replace("https://discord.test/hook", "${NOPE}");
        assert_eq!(
            Config::from_yaml(&y, &no_env).unwrap_err(),
            ConfigError::MissingEnv("NOPE".into())
        );
    }

    #[test]
    fn missing_required_fields() {
        for y in [
            "data_center: B\nwatch:\n  - item: X\n    alert_below: 1\n",
            "home_world: A\nwatch:\n  - item: X\n    alert_below: 1\n",
            "home_world: A\ndata_center: B\n",
            "home_world: A\ndata_center: B\nwatch:\n  - alert_below: 1\n",
        ] {
            assert!(matches!(
                Config::from_yaml(y, &no_env),
                Err(ConfigError::Yaml(_))
            ), "{y}");
        }
    }

    #[test]
    fn empty_watch_list_rejected() {
        let y = "home_world: A\ndata_center: B\nwatch: []\n";
        assert!(matches!(Config::from_yaml(y, &no_env), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn item_without_any_rule_rejected() {
        let y = "home_world: A\ndata_center: B\nwatch:\n  - item: X\n";
        assert!(matches!(Config::from_yaml(y, &no_env), Err(ConfigError::Invalid(_))));
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        for bad in ["", "abc", "10", "-5s", "0s", "5x", "s"] {
            assert_eq!(
                parse_duration(bad).unwrap_err(),
                ConfigError::InvalidDuration(bad.into()),
                "{bad}"
            );
        }
    }

    #[test]
    fn invalid_poll_interval_rejected() {
        let y = VALID.replace("60s", "soon");
        assert_eq!(
            Config::from_yaml(&y, &no_env).unwrap_err(),
            ConfigError::InvalidDuration("soon".into())
        );
    }

    #[test]
    fn negative_threshold_rejected() {
        let y = VALID.replace("1800", "-1");
        assert!(Config::from_yaml(&y, &no_env).is_err());
    }

    #[test]
    fn out_of_range_margin_rejected() {
        for m in ["0", "-0.1", "1.5"] {
            let y = VALID.replace("0.25", m);
            assert!(
                matches!(Config::from_yaml(&y, &no_env), Err(ConfigError::Invalid(_))),
                "{m}"
            );
        }
    }

    #[test]
    fn telegram_config_parses() {
        let y = format!(
            "{VALID}"
        )
        .replace(
            "  discord: \"https://discord.test/hook\"",
            "  telegram:\n    bot_token: tok\n    chat_id: \"42\"\n    api_base: http://tg.test",
        );
        let c = Config::from_yaml(&y, &no_env).unwrap();
        let t = c.webhooks.telegram.unwrap();
        assert_eq!((t.bot_token.as_str(), t.chat_id.as_str(), t.api_base.as_str()), ("tok", "42", "http://tg.test"));
        assert!(c.webhooks.discord.is_none());
    }

    #[test]
    fn load_reads_file_and_reports_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("w.yaml");
        std::fs::write(&p, VALID).unwrap();
        assert!(Config::load(&p).is_ok());
        assert!(matches!(
            Config::load(&dir.path().join("missing.yaml")),
            Err(ConfigError::Io(_))
        ));
    }
}
