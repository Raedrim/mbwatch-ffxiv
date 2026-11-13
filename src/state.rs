//! Alert de-duplication state persisted to `state.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("state I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt state file {0}: {1}")]
    Corrupt(PathBuf, serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub last_alerted_price: u64,
    pub last_alerted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    entries: HashMap<String, Entry>,
}

impl State {
    pub fn key(item_id: u32, hq: bool, world_id: u32) -> String {
        format!("{item_id}:{}:{world_id}", if hq { "hq" } else { "nq" })
    }

    /// Fire when: never alerted, price dropped further, or cooldown elapsed.
    pub fn should_alert(
        &self,
        key: &str,
        price: u64,
        now: DateTime<Utc>,
        cooldown: Duration,
    ) -> bool {
        let Some(e) = self.entries.get(key) else { return true };
        if price < e.last_alerted_price {
            return true;
        }
        let cooldown = chrono::Duration::from_std(cooldown).unwrap_or(chrono::Duration::MAX);
        now.signed_duration_since(e.last_alerted_at) >= cooldown
    }

    pub fn record(&mut self, key: String, price: u64, now: DateTime<Utc>) {
        self.entries
            .insert(key, Entry { last_alerted_price: price, last_alerted_at: now });
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.entries.get(key)
    }

    /// Missing file → empty state.
    pub fn load(path: &Path) -> Result<State, StateError> {
        match std::fs::read(path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| StateError::Corrupt(path.to_path_buf(), e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, serde_json::to_vec_pretty(self).expect("state serializes"))?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const COOLDOWN: Duration = Duration::from_secs(6 * 3600);

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, h, 0, 0).unwrap()
    }

    #[test]
    fn key_isolates_hq_and_world() {
        assert_ne!(State::key(1, true, 5), State::key(1, false, 5));
        assert_ne!(State::key(1, true, 5), State::key(1, true, 6));
        assert_eq!(State::key(1, true, 5), State::key(1, true, 5));
    }

    #[test]
    fn first_alert_always_fires() {
        assert!(State::default().should_alert("k", 100, t(0), COOLDOWN));
    }

    #[test]
    fn same_price_within_cooldown_is_suppressed() {
        let mut s = State::default();
        s.record("k".into(), 100, t(0));
        assert!(!s.should_alert("k", 100, t(1), COOLDOWN));
    }

    #[test]
    fn higher_price_within_cooldown_is_suppressed() {
        let mut s = State::default();
        s.record("k".into(), 100, t(0));
        assert!(!s.should_alert("k", 150, t(1), COOLDOWN));
    }

    #[test]
    fn lower_price_refires_within_cooldown() {
        let mut s = State::default();
        s.record("k".into(), 100, t(0));
        assert!(s.should_alert("k", 99, t(1), COOLDOWN));
    }

    #[test]
    fn refires_exactly_at_and_after_cooldown() {
        let mut s = State::default();
        s.record("k".into(), 100, t(0));
        assert!(!s.should_alert("k", 100, t(5), COOLDOWN));
        assert!(s.should_alert("k", 100, t(6), COOLDOWN));
        assert!(s.should_alert("k", 100, t(23), COOLDOWN));
    }

    #[test]
    fn other_keys_unaffected() {
        let mut s = State::default();
        s.record("a".into(), 100, t(0));
        assert!(s.should_alert("b", 100, t(0), COOLDOWN));
    }

    #[test]
    fn record_overwrites() {
        let mut s = State::default();
        s.record("k".into(), 100, t(0));
        s.record("k".into(), 80, t(2));
        let e = s.get("k").unwrap();
        assert_eq!((e.last_alerted_price, e.last_alerted_at), (80, t(2)));
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(&dir.path().join("state.json")).unwrap(), State::default());
    }

    #[test]
    fn save_load_roundtrip_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub/state.json");
        let mut s = State::default();
        s.record(State::key(1, true, 33), 1234, t(3));
        s.save(&p).unwrap();
        assert_eq!(State::load(&p).unwrap(), s);
        let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        let e = &json["entries"]["1:hq:33"];
        assert_eq!(e["last_alerted_price"], 1234);
        assert!(e["last_alerted_at"].is_string());
    }

    #[test]
    fn corrupt_file_is_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("state.json");
        std::fs::write(&p, "garbage").unwrap();
        assert!(matches!(State::load(&p), Err(StateError::Corrupt(..))));
    }
}
