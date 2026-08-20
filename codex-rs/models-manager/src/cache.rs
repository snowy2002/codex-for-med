use chrono::DateTime;
use chrono::Utc;
use codex_protocol::openai_models::ModelInfo;
use serde::Deserialize;
use serde::Serialize;
use std::io;
use std::io::ErrorKind;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::fs;
use tracing::error;
use tracing::info;

const MODELS_CACHE_SCHEMA_VERSION: u32 = 1;

/// Manages loading and saving of models cache to disk.
#[derive(Debug)]
pub(crate) struct ModelsCacheManager {
    cache_path: PathBuf,
    cache_ttl: Duration,
}

impl ModelsCacheManager {
    /// Create a new cache manager with the given path and TTL.
    pub(crate) fn new(cache_path: PathBuf, cache_ttl: Duration) -> Self {
        Self {
            cache_path,
            cache_ttl,
        }
    }

    /// Attempt to load a fresh cache entry. Returns `None` if the cache doesn't exist or is stale.
    pub(crate) async fn load_fresh(
        &self,
        expected_version: &str,
        expected_provider: &str,
    ) -> Option<ModelsCache> {
        info!(
                cache_path = %self.cache_path.display(),
                expected_version,
            "models cache: attempting load_fresh"
        );
        let cache = match self.load().await {
            Ok(cache) => cache?,
            Err(err) => {
                error!("failed to load models cache: {err}");
                return None;
            }
        };
        if !cache.is_schema_compatible() {
            info!(
                cache_path = %self.cache_path.display(),
                cached_schema_version = cache.schema_version,
                supported_schema_version = MODELS_CACHE_SCHEMA_VERSION,
                "models cache: incompatible cache schema"
            );
            return None;
        }
        if !cache.matches_provider(expected_provider) {
            info!(
                cache_path = %self.cache_path.display(),
                expected_provider,
                cached_provider = ?cache.provider_identity,
                "models cache: provider identity mismatch"
            );
            return None;
        }
        info!(
            cache_path = %self.cache_path.display(),
            cached_version = ?cache.client_version,
            fetched_at = %cache.fetched_at,
            "models cache: loaded cache file"
        );
        if cache.client_version.as_deref() != Some(expected_version) {
            info!(
                cache_path = %self.cache_path.display(),
                expected_version,
                cached_version = ?cache.client_version,
                "models cache: cache version mismatch"
            );
            return None;
        }
        if !cache.is_fresh(self.cache_ttl) {
            info!(
                cache_path = %self.cache_path.display(),
                cache_ttl_secs = self.cache_ttl.as_secs(),
                fetched_at = %cache.fetched_at,
                "models cache: cache is stale"
            );
            return None;
        }
        info!(
            cache_path = %self.cache_path.display(),
            cache_ttl_secs = self.cache_ttl.as_secs(),
            "models cache: cache hit"
        );
        Some(cache)
    }

    /// Load the last compatible catalog regardless of age or client version.
    ///
    /// This is used only when the network is unavailable (or in explicit offline mode), so an
    /// older but decodable catalog can keep the client usable without suppressing normal refreshes.
    pub(crate) async fn load_last_known_good(
        &self,
        expected_provider: &str,
    ) -> Option<ModelsCache> {
        let cache = match self.load().await {
            Ok(cache) => cache?,
            Err(err) => {
                error!("failed to load last-known-good models cache: {err}");
                return None;
            }
        };
        if !cache.is_schema_compatible()
            || !cache.matches_provider(expected_provider)
            || cache.models.is_empty()
        {
            info!(
                cache_path = %self.cache_path.display(),
                cached_schema_version = cache.schema_version,
                cached_provider = ?cache.provider_identity,
                expected_provider,
                models_count = cache.models.len(),
                "models cache: last-known-good entry is not usable"
            );
            return None;
        }
        info!(
            cache_path = %self.cache_path.display(),
            cached_version = ?cache.client_version,
            fetched_at = %cache.fetched_at,
            models_count = cache.models.len(),
            "models cache: using last-known-good catalog"
        );
        Some(cache)
    }

    /// Persist the cache to disk, creating parent directories as needed.
    pub(crate) async fn persist_cache(
        &self,
        models: &[ModelInfo],
        etag: Option<String>,
        client_version: String,
        provider_identity: String,
    ) {
        let cache = ModelsCache {
            schema_version: MODELS_CACHE_SCHEMA_VERSION,
            fetched_at: Utc::now(),
            etag,
            client_version: Some(client_version),
            provider_identity: Some(provider_identity),
            models: models.to_vec(),
        };
        if let Err(err) = self.save_internal(&cache).await {
            error!("failed to write models cache: {err}");
        }
    }

    /// Renew the cache TTL by updating the fetched_at timestamp to now.
    pub(crate) async fn renew_cache_ttl(&self) -> io::Result<()> {
        let mut cache = match self.load().await? {
            Some(cache) => cache,
            None => return Err(io::Error::new(ErrorKind::NotFound, "cache not found")),
        };
        cache.fetched_at = Utc::now();
        self.save_internal(&cache).await
    }

    async fn load(&self) -> io::Result<Option<ModelsCache>> {
        match fs::read(&self.cache_path).await {
            Ok(contents) => {
                let cache = serde_json::from_slice(&contents)
                    .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;
                Ok(Some(cache))
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    async fn save_internal(&self, cache: &ModelsCache) -> io::Result<()> {
        if let Some(parent) = self.cache_path.parent() {
            fs::create_dir_all(parent).await?;
        }
        let json = serde_json::to_vec_pretty(cache)
            .map_err(|err| io::Error::new(ErrorKind::InvalidData, err.to_string()))?;
        let cache_path = self.cache_path.clone();
        tokio::task::spawn_blocking(move || {
            let parent = cache_path.parent().ok_or_else(|| {
                io::Error::new(
                    ErrorKind::InvalidInput,
                    format!("models cache path has no parent: {}", cache_path.display()),
                )
            })?;
            let mut temp = NamedTempFile::new_in(parent)?;
            temp.write_all(&json)?;
            temp.as_file().sync_all()?;
            temp.persist(&cache_path).map_err(|err| err.error)?;
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }

    #[cfg(test)]
    /// Set the cache TTL.
    pub(crate) fn set_ttl(&mut self, ttl: Duration) {
        self.cache_ttl = ttl;
    }

    #[cfg(test)]
    /// Manipulate cache file for testing. Allows setting a custom fetched_at timestamp.
    pub(crate) async fn manipulate_cache_for_test<F>(&self, f: F) -> io::Result<()>
    where
        F: FnOnce(&mut DateTime<Utc>),
    {
        let mut cache = match self.load().await? {
            Some(cache) => cache,
            None => return Err(io::Error::new(ErrorKind::NotFound, "cache not found")),
        };
        f(&mut cache.fetched_at);
        self.save_internal(&cache).await
    }

    #[cfg(test)]
    /// Mutate the full cache contents for testing.
    pub(crate) async fn mutate_cache_for_test<F>(&self, f: F) -> io::Result<()>
    where
        F: FnOnce(&mut ModelsCache),
    {
        let mut cache = match self.load().await? {
            Some(cache) => cache,
            None => return Err(io::Error::new(ErrorKind::NotFound, "cache not found")),
        };
        f(&mut cache);
        self.save_internal(&cache).await
    }
}

/// Serialized snapshot of models and metadata cached on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ModelsCache {
    #[serde(default = "default_models_cache_schema_version")]
    pub(crate) schema_version: u32,
    pub(crate) fetched_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) client_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) provider_identity: Option<String>,
    pub(crate) models: Vec<ModelInfo>,
}

impl ModelsCache {
    fn is_schema_compatible(&self) -> bool {
        self.schema_version == MODELS_CACHE_SCHEMA_VERSION
    }

    fn matches_provider(&self, expected_provider: &str) -> bool {
        self.provider_identity
            .as_deref()
            .is_none_or(|provider| provider == expected_provider)
    }

    /// Returns `true` when the cache entry has not exceeded the configured TTL.
    fn is_fresh(&self, ttl: Duration) -> bool {
        if ttl.is_zero() {
            return false;
        }
        let Ok(ttl_duration) = chrono::Duration::from_std(ttl) else {
            return false;
        };
        let age = Utc::now().signed_duration_since(self.fetched_at);
        age <= ttl_duration
    }
}

const fn default_models_cache_schema_version() -> u32 {
    MODELS_CACHE_SCHEMA_VERSION
}
