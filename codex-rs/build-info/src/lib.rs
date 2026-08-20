use serde::Deserialize;
use serde::Serialize;
use std::sync::LazyLock;

/// Audited upstream baseline used for OpenAI protocol compatibility.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamBaseline {
    pub repository: String,
    pub revision: String,
    pub npm_release: String,
    pub protocol_client_version: String,
}

/// One upstream file whose drift can affect model discovery or selection.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct TrackedCompatibilityFile {
    pub path: String,
    pub sha256: String,
}

/// Machine-readable release and protocol compatibility metadata for Codex Med.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelCompatibilityManifest {
    pub schema_version: u32,
    pub codex_med_version: String,
    pub model_cache_schema_version: u32,
    pub upstream: UpstreamBaseline,
    pub tracked_files: Vec<TrackedCompatibilityFile>,
}

const MODEL_COMPATIBILITY_MANIFEST_JSON: &str = include_str!("../upstream_compatibility.json");

static UPSTREAM_PROTOCOL_CLIENT_VERSION: LazyLock<String> =
    LazyLock::new(|| match model_compatibility_manifest() {
        Ok(manifest) => manifest.upstream.protocol_client_version,
        Err(error) => panic!("embedded upstream compatibility manifest must be valid: {error}"),
    });

/// Load the compatibility manifest embedded in the release binary.
pub fn model_compatibility_manifest() -> Result<ModelCompatibilityManifest, serde_json::Error> {
    serde_json::from_str(MODEL_COMPATIBILITY_MANIFEST_JSON)
}

/// Return the audited official Codex version used for OpenAI protocol capability negotiation.
///
/// Codex Med keeps its independent package version for display and release management. Only
/// OpenAI-facing protocol requests use this value, which is advanced after the upstream model and
/// request compatibility audit passes.
pub fn upstream_protocol_client_version() -> &'static str {
    UPSTREAM_PROTOCOL_CLIENT_VERSION.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn embedded_manifest_separates_distribution_and_protocol_versions() {
        let manifest = model_compatibility_manifest().expect("compatibility manifest should parse");

        assert_eq!(manifest.schema_version, 2);
        assert_eq!(manifest.codex_med_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.model_cache_schema_version, 1);
        assert_eq!(manifest.upstream.revision.len(), 40);
        assert_eq!(
            upstream_protocol_client_version(),
            manifest.upstream.protocol_client_version
        );
        assert_eq!(
            manifest.upstream.protocol_client_version,
            manifest.upstream.npm_release
        );
        assert!(!manifest.tracked_files.is_empty());
        assert!(
            manifest
                .tracked_files
                .iter()
                .all(|tracked| tracked.sha256.len() == 64)
        );
    }
}
