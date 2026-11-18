use crate::git::Git;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

const FILE_NAME: &str = "undercurrent-cache.json";

#[derive(Clone, Debug, PartialEq)]
pub struct Stored {
    pub path: String,
    pub line: u32,
    pub detail: String,
}

pub struct Cache {
    location: Option<PathBuf>,
    previous: HashMap<String, Vec<Stored>>,
    current: HashMap<String, Vec<Stored>>,
    previous_fits: HashMap<String, Option<f64>>,
    current_fits: HashMap<String, Option<f64>>,
}

pub fn key(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn decode(value: &Value) -> Option<Vec<Stored>> {
    let entries = value.as_array()?.iter().map(|entry| {
        Some(Stored {
            path: entry["path"].as_str()?.to_string(),
            line: u32::try_from(entry["line"].as_u64()?).ok()?,
            detail: entry["detail"].as_str()?.to_string(),
        })
    });
    entries.collect()
}

fn encode(stored: &[Stored]) -> Value {
    let entries = stored
        .iter()
        .map(|s| json!({ "path": s.path, "line": s.line, "detail": s.detail }));
    Value::Array(entries.collect())
}

impl Cache {
    pub fn load(git: &Git) -> Cache {
        let location = git.git_dir().ok().map(|dir| dir.join(FILE_NAME));
        let text = location
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok());
        let parsed: Option<Value> = text.and_then(|text| serde_json::from_str(&text).ok());
        let mut previous = HashMap::new();
        if let Some(entries) = parsed
            .as_ref()
            .and_then(|value| value["entries"].as_object())
        {
            for (key, value) in entries {
                if let Some(stored) = decode(value) {
                    previous.insert(key.clone(), stored);
                }
            }
        }
        let fits = parsed.as_ref().and_then(|value| value["fits"].as_object());
        let previous_fits = fits.map(|fits| {
            fits.iter()
                .map(|(key, value)| (key.clone(), value.as_f64()))
                .collect()
        });
        Cache {
            location,
            previous,
            current: HashMap::new(),
            previous_fits: previous_fits.unwrap_or_default(),
            current_fits: HashMap::new(),
        }
    }

    pub fn fit(&mut self, fingerprint: &str) -> Option<Option<f64>> {
        let fit = *self.previous_fits.get(fingerprint)?;
        self.current_fits.insert(fingerprint.to_string(), fit);
        Some(fit)
    }

    pub fn put_fit(&mut self, fingerprint: String, threshold: Option<f64>) {
        self.current_fits.insert(fingerprint, threshold);
    }

    pub fn get(&mut self, key: &str) -> Option<Vec<Stored>> {
        let stored = self.previous.get(key)?.clone();
        self.current.insert(key.to_string(), stored.clone());
        Some(stored)
    }

    pub fn put(&mut self, key: String, stored: Vec<Stored>) {
        self.current.insert(key, stored);
    }

    pub fn save(&self) -> Result<()> {
        let Some(location) = &self.location else {
            return Ok(());
        };
        let entries: serde_json::Map<String, Value> = self
            .current
            .iter()
            .map(|(key, stored)| (key.clone(), encode(stored)))
            .collect();
        let fits: serde_json::Map<String, Value> = self
            .current_fits
            .iter()
            .map(|(key, threshold)| (key.clone(), json!(threshold)))
            .collect();
        let text = serde_json::to_string(&json!({ "entries": entries, "fits": fits }))?;
        std::fs::write(location, text).with_context(|| {
            format!(
                "could not write the verdict cache to {}; check it is writable",
                location.display()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_depend_on_part_boundaries() {
        assert_eq!(key(&["ab", "c"]), key(&["ab", "c"]));
        assert_ne!(key(&["ab", "c"]), key(&["a", "bc"]));
    }

    #[test]
    fn stored_findings_round_trip() {
        let stored = vec![Stored {
            path: "a.rs".to_string(),
            line: 3,
            detail: "x".to_string(),
        }];
        assert_eq!(decode(&encode(&stored)), Some(stored));
    }
}
