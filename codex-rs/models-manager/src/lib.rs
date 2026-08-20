pub(crate) mod cache;
pub mod collaboration_mode_presets;
pub mod compatibility;
pub(crate) mod config;
pub mod manager;
pub mod model_info;
pub mod model_presets;
pub mod test_support;

pub use codex_app_server_protocol::AuthMode;
pub use config::ModelsManagerConfig;

/// Load the bundled model catalog shipped with `codex-models-manager`.
pub fn bundled_models_response()
-> std::result::Result<codex_protocol::openai_models::ModelsResponse, serde_json::Error> {
    serde_json::from_str(include_str!("../models.json"))
}

/// Return the audited official Codex version used for model-catalog capability negotiation.
pub fn client_version_to_whole() -> String {
    codex_build_info::upstream_protocol_client_version().to_string()
}
