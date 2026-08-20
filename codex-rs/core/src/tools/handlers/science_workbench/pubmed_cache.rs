//! Seven-day raw-response cache for NCBI E-utilities.

use anyhow::Context;
use anyhow::Result;
use chrono::Utc;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

const CACHE_TTL: chrono::Duration = chrono::Duration::days(7);

pub(super) struct PubmedCache {
    directory: PathBuf,
    force_refresh: bool,
}

impl PubmedCache {
    pub(super) fn new(workspace_root: &Path, force_refresh: bool) -> Self {
        Self {
            directory: workspace_root
                .join(".codex-med")
                .join("cache")
                .join("pubmed"),
            force_refresh,
        }
    }

    pub(super) fn get(&self, url: &str, query: &[(&str, String)]) -> Option<String> {
        if self.force_refresh {
            return None;
        }
        let text = fs::read_to_string(self.path(url, query)).ok()?;
        let cached: Value = serde_json::from_str(&text).ok()?;
        let fetched_at = cached
            .get("fetched_at")
            .and_then(Value::as_str)
            .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())?
            .with_timezone(&Utc);
        if Utc::now().signed_duration_since(fetched_at) > CACHE_TTL {
            return None;
        }
        cached
            .get("body")
            .and_then(Value::as_str)
            .map(ToString::to_string)
    }

    pub(super) fn put(&self, url: &str, query: &[(&str, String)], body: &str) -> Result<()> {
        fs::create_dir_all(&self.directory).with_context(|| {
            format!("failed to create PubMed cache {}", self.directory.display())
        })?;
        let path = self.path(url, query);
        let temporary = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let entry = json!({
            "schema_version": 1,
            "fetched_at": Utc::now().to_rfc3339(),
            "endpoint": url,
            "body": body,
        });
        fs::write(&temporary, serde_json::to_vec(&entry)?)
            .with_context(|| format!("failed to write PubMed cache {}", temporary.display()))?;
        fs::rename(&temporary, &path)
            .with_context(|| format!("failed to commit PubMed cache {}", path.display()))
    }

    fn path(&self, url: &str, query: &[(&str, String)]) -> PathBuf {
        let mut key = url.to_string();
        for (name, value) in query {
            key.push('\n');
            key.push_str(name);
            key.push('=');
            key.push_str(value);
        }
        let digest = Sha256::digest(key.as_bytes());
        self.directory.join(format!("{digest:x}.json"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_and_force_refresh_bypasses_entries() {
        let temp = tempfile::tempdir().expect("temp dir");
        let query = [("db", "pubmed".to_string()), ("id", "123".to_string())];
        let cache = PubmedCache::new(temp.path(), /*force_refresh*/ false);
        cache
            .put("https://example.invalid/efetch", &query, "record")
            .expect("cache put");
        assert_eq!(
            cache.get("https://example.invalid/efetch", &query),
            Some("record".to_string())
        );
        assert!(
            PubmedCache::new(temp.path(), /*force_refresh*/ true)
                .get("https://example.invalid/efetch", &query)
                .is_none()
        );
    }
}
