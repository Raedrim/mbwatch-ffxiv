//! Local metadata cache: item dictionary, worlds and data centers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("unknown item `{0}` (run `mbwatch update-db`, or check the spelling)")]
    UnknownItem(String),
    #[error("unknown world or data center `{0}`")]
    UnknownWorld(String),
    #[error("cache not found at {0} — run `mbwatch update-db` first")]
    Missing(PathBuf),
    #[error("cache I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("corrupt cache file: {0}")]
    Corrupt(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataCenter {
    pub name: String,
    pub region: String,
    pub worlds: Vec<u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cache {
    pub items: HashMap<u32, String>,
    pub worlds: HashMap<u32, String>,
    pub data_centers: Vec<DataCenter>,
    #[serde(skip)]
    name_index: HashMap<String, u32>,
}

impl Cache {
    pub const FILE_NAME: &'static str = "cache.json";

    pub fn new(
        items: HashMap<u32, String>,
        worlds: HashMap<u32, String>,
        data_centers: Vec<DataCenter>,
    ) -> Self {
        let mut c = Cache {
            items,
            worlds,
            data_centers,
            name_index: HashMap::new(),
        };
        c.rebuild_index();
        c
    }

    fn rebuild_index(&mut self) {
        // Iterate in ID order so duplicate names resolve deterministically (lowest ID).
        let mut ids: Vec<_> = self.items.keys().copied().collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        self.name_index = ids
            .into_iter()
            .map(|id| (self.items[&id].trim().to_lowercase(), id))
            .collect();
    }

    /// Case-insensitive item name → ID.
    pub fn item_id(&self, name: &str) -> Result<u32, CacheError> {
        self.name_index
            .get(&name.trim().to_lowercase())
            .copied()
            .ok_or_else(|| CacheError::UnknownItem(name.to_string()))
    }

    pub fn world_name(&self, id: u32) -> Option<&str> {
        self.worlds.get(&id).map(String::as_str)
    }

    pub fn world_id(&self, name: &str) -> Result<u32, CacheError> {
        self.worlds
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name.trim()))
            .map(|(id, _)| *id)
            .ok_or_else(|| CacheError::UnknownWorld(name.to_string()))
    }

    pub fn data_center(&self, name: &str) -> Option<&DataCenter> {
        self.data_centers
            .iter()
            .find(|d| d.name.eq_ignore_ascii_case(name.trim()))
    }

    /// Atomically write to `<dir>/cache.json` (temp file + rename).
    pub fn save(&self, dir: &Path) -> Result<(), CacheError> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{}.tmp", Self::FILE_NAME));
        std::fs::write(&tmp, serde_json::to_vec(self)?)?;
        std::fs::rename(&tmp, dir.join(Self::FILE_NAME))?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Cache, CacheError> {
        let path = dir.join(Self::FILE_NAME);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(CacheError::Missing(path))
            }
            Err(e) => return Err(e.into()),
        };
        let mut c: Cache = serde_json::from_slice(&bytes)?;
        c.rebuild_index();
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Cache {
        Cache::new(
            HashMap::from([
                (1, "Moqueca".to_string()),
                (2, "Grade 2 Gemdraught of Strength".to_string()),
            ]),
            HashMap::from([(21, "Ravana".to_string()), (33, "Phoenix".to_string())]),
            vec![DataCenter {
                name: "Light".into(),
                region: "Europe".into(),
                worlds: vec![33],
            }],
        )
    }

    #[test]
    fn item_lookup_is_case_insensitive_and_trims() {
        let c = sample();
        assert_eq!(c.item_id("moqueca").unwrap(), 1);
        assert_eq!(c.item_id("  MOQUECA ").unwrap(), 1);
        assert_eq!(c.item_id("grade 2 gemdraught of strength").unwrap(), 2);
    }

    #[test]
    fn unknown_item_is_typed_error() {
        let e = sample().item_id("Nope").unwrap_err();
        assert!(matches!(e, CacheError::UnknownItem(ref n) if n == "Nope"));
    }

    #[test]
    fn world_lookups() {
        let c = sample();
        assert_eq!(c.world_name(33), Some("Phoenix"));
        assert_eq!(c.world_name(999), None);
        assert_eq!(c.world_id("phoenix").unwrap(), 33);
        assert!(matches!(
            c.world_id("Narnia"),
            Err(CacheError::UnknownWorld(_))
        ));
    }

    #[test]
    fn data_center_lookup_is_case_insensitive() {
        let c = sample();
        assert_eq!(c.data_center("light").unwrap().region, "Europe");
        assert!(c.data_center("Chaos").is_none());
    }

    #[test]
    fn save_and_load_roundtrip_in_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        sample().save(dir.path()).unwrap();
        let loaded = Cache::load(dir.path()).unwrap();
        assert_eq!(loaded.item_id("Moqueca").unwrap(), 1);
        assert_eq!(loaded.world_name(21), Some("Ravana"));
        // no temp files left behind
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, vec!["cache.json".to_string()]);
    }

    #[test]
    fn save_creates_missing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b");
        sample().save(&nested).unwrap();
        assert!(nested.join("cache.json").exists());
    }

    #[test]
    fn load_missing_is_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            Cache::load(dir.path()),
            Err(CacheError::Missing(_))
        ));
    }

    #[test]
    fn load_corrupt_is_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cache.json"), "{not json").unwrap();
        assert!(matches!(
            Cache::load(dir.path()),
            Err(CacheError::Corrupt(_))
        ));
    }

    #[test]
    fn overwrite_replaces_previous_contents() {
        let dir = tempfile::tempdir().unwrap();
        sample().save(dir.path()).unwrap();
        Cache::new(HashMap::from([(7, "Other".into())]), HashMap::new(), vec![])
            .save(dir.path())
            .unwrap();
        let c = Cache::load(dir.path()).unwrap();
        assert!(c.item_id("Moqueca").is_err());
        assert_eq!(c.item_id("other").unwrap(), 7);
    }
}
