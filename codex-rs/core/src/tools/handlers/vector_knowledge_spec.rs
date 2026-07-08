use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub const SEARCH_VECTOR_KNOWLEDGE_TOOL_NAME: &str = "search_vector_knowledge";

pub fn create_search_vector_knowledge_tool() -> ToolSpec {
    let scalar_filter_value = JsonSchema::any_of(
        vec![
            JsonSchema::string(None),
            JsonSchema::number(None),
            JsonSchema::integer(None),
            JsonSchema::boolean(None),
        ],
        None,
    );
    let properties = BTreeMap::from([
        (
            "query".to_string(),
            JsonSchema::string(Some(
                "Required natural language query to embed and search against the medical vector knowledge collection."
                    .to_string(),
            )),
        ),
        (
            "categories".to_string(),
            JsonSchema::array(
                JsonSchema::string(None),
                Some(
                    "Optional knowledge categories to search, for example bio_literature, web_knowledge, database_record, experiment_record, protocol, clinical_guideline, patent, or internal_note."
                        .to_string(),
                ),
            ),
        ),
        (
            "filters".to_string(),
            JsonSchema::object(
                BTreeMap::new(),
                None,
                Some(JsonSchema::any_of(
                    vec![
                        JsonSchema::string(None),
                        JsonSchema::number(None),
                        JsonSchema::integer(None),
                        JsonSchema::boolean(None),
                        JsonSchema::array(scalar_filter_value, None),
                    ],
                    None,
                ).into()),
            ),
        ),
        (
            "top_k".to_string(),
            JsonSchema::integer(Some(
                "Maximum number of results to return, capped at 30. Defaults to 8.".to_string(),
            )),
        ),
        (
            "collection".to_string(),
            JsonSchema::string(Some(
                "Optional Qdrant collection override. Defaults to CODEX_MED_VECTOR_COLLECTION or medical_knowledge_qwen3_4b."
                    .to_string(),
            )),
        ),
        (
            "qdrant_url".to_string(),
            JsonSchema::string(Some(
                "Optional Qdrant base URL override. Defaults to CODEX_MED_VECTOR_QDRANT_URL or the shared codex-med gateway at http://150.5.166.194/vector."
                    .to_string(),
            )),
        ),
        (
            "embedding_url".to_string(),
            JsonSchema::string(Some(
                "Optional embedding endpoint override. Defaults to CODEX_MED_EMBEDDING_URL or the shared Qwen3-Embedding-4B endpoint. The endpoint should accept {\"input\": \"...\"} and return either {\"embedding\": [...]} or OpenAI-compatible {\"data\": [{\"embedding\": [...]}]}."
                    .to_string(),
            )),
        ),
        (
            "embedding_model".to_string(),
            JsonSchema::string(Some(
                "Optional embedding model name sent to the embedding endpoint. Defaults to CODEX_MED_EMBEDDING_MODEL or /model_dir/Qwen3-Embedding-4B."
                    .to_string(),
            )),
        ),
        (
            "include_payload".to_string(),
            JsonSchema::boolean(Some(
                "Whether to include result payload metadata and snippets. Defaults to true.".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: SEARCH_VECTOR_KNOWLEDGE_TOOL_NAME.to_string(),
        description: "Search the codex-med Qdrant vector knowledge database directly from Codex. By default this uses the shared medical_knowledge_qwen3_4b collection, Qwen3-Embedding-4B query embeddings, and a Qwen3 reranker stage. It supports category and metadata filters for biomedical literature, web knowledge, real database records, experiment records, protocols, guidelines, patents, and internal notes."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["query".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

#[cfg(test)]
#[path = "vector_knowledge_spec_tests.rs"]
mod tests;
