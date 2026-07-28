//! Shared runtime configuration for Qdrant-backed tools.

use crate::function_tool::FunctionCallError;
use reqwest::RequestBuilder;
use reqwest::header::HeaderValue;
use url::Url;

pub(super) const DEFAULT_QDRANT_URL: &str = "http://150.5.166.194/vector";
pub(super) const DEFAULT_QDRANT_COLLECTION: &str = "medical_knowledge_qwen3_4b";

#[derive(Debug, Clone)]
pub(super) struct QdrantRuntimeConfig {
    base_url: String,
    collection: String,
    api_key: Option<String>,
}

impl QdrantRuntimeConfig {
    pub(super) fn from_environment(
        url_override: Option<&str>,
        collection_override: Option<&str>,
    ) -> Result<Self, FunctionCallError> {
        let base_url = url_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .or_else(|| env_non_empty("CODEX_MED_VECTOR_QDRANT_URL"))
            .unwrap_or_else(|| DEFAULT_QDRANT_URL.to_string());
        let parsed = Url::parse(&base_url).map_err(|error| {
            FunctionCallError::RespondToModel(format!(
                "invalid CODEX_MED_VECTOR_QDRANT_URL: {error}"
            ))
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(FunctionCallError::RespondToModel(
                "CODEX_MED_VECTOR_QDRANT_URL must use http or https".to_string(),
            ));
        }

        let collection = collection_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .or_else(|| env_non_empty("CODEX_MED_VECTOR_COLLECTION"))
            .unwrap_or_else(|| DEFAULT_QDRANT_COLLECTION.to_string());
        if !collection
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        {
            return Err(FunctionCallError::RespondToModel(
                "Qdrant collection may only contain ASCII letters, digits, underscore, hyphen, or dot"
                    .to_string(),
            ));
        }

        let api_key = env_non_empty("CODEX_MED_VECTOR_QDRANT_API_KEY")
            .or_else(|| env_non_empty("QDRANT_API_KEY"));
        if let Some(api_key) = &api_key {
            HeaderValue::from_str(api_key).map_err(|error| {
                FunctionCallError::RespondToModel(format!("invalid Qdrant API key header: {error}"))
            })?;
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            collection,
            api_key,
        })
    }

    pub(super) fn base_url(&self) -> &str {
        &self.base_url
    }

    pub(super) fn collection(&self) -> &str {
        &self.collection
    }

    pub(super) fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    pub(super) fn authenticate(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.api_key {
            Some(api_key) => request.header("api-key", api_key),
            None => request,
        }
    }

    pub(super) fn collection_url(&self, suffix: &str) -> String {
        format!(
            "{}/collections/{}{}",
            self.base_url, self.collection, suffix
        )
    }
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_urls_and_invalid_collection_names() {
        assert!(
            QdrantRuntimeConfig::from_environment(Some("file:///tmp/qdrant"), Some("ok")).is_err()
        );
        assert!(
            QdrantRuntimeConfig::from_environment(
                Some("http://127.0.0.1:6333"),
                Some("../not-a-collection"),
            )
            .is_err()
        );
    }
}
