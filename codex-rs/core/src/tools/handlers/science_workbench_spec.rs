use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub const LIST_MED_KNOWLEDGE_COLLECTIONS_TOOL_NAME: &str = "list_med_knowledge_collections";
pub const DESCRIBE_MED_DATABASE_TOOL_NAME: &str = "describe_med_database";
pub const LITERATURE_MAP_TOOL_NAME: &str = "literature_map";
pub const PUBMED_LITERATURE_MAP_TOOL_NAME: &str = "pubmed_literature_map";

pub fn create_list_med_knowledge_collections_tool() -> ToolSpec {
    ToolSpec::Function(ResponsesApiTool {
        name: LIST_MED_KNOWLEDGE_COLLECTIONS_TOOL_NAME.to_string(),
        description: "List the codex-med knowledge backends available to the model, including Qdrant vector collections and the SQL antibody database. Use this before choosing a medical retrieval strategy."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(BTreeMap::new(), None, Some(false.into())),
        output_schema: None,
    })
}

pub fn create_describe_med_database_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "include_schema".to_string(),
            JsonSchema::boolean(Some(
                "Whether to include SQL column metadata. Defaults to true.".to_string(),
            )),
        ),
        (
            "include_vector_details".to_string(),
            JsonSchema::boolean(Some(
                "Whether to include Qdrant collection metadata and payload schema. Defaults to true."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: DESCRIBE_MED_DATABASE_TOOL_NAME.to_string(),
        description: "Describe the deployed codex-med SQL and vector databases: table purpose, row counts, fields, vector collection size, payload schema, categories, source types, and recommended use cases."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(properties, None, Some(false.into())),
        output_schema: None,
    })
}

pub fn create_literature_map_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "topic".to_string(),
            JsonSchema::string(Some(
                "Required scientific topic or question to map, for example `integrated stress response aging neurodegeneration`."
                    .to_string(),
            )),
        ),
        (
            "project_id".to_string(),
            JsonSchema::string(Some(
                "Optional filesystem-safe research project id. Defaults to a slug derived from topic."
                    .to_string(),
            )),
        ),
        (
            "year_range".to_string(),
            JsonSchema::string(Some(
                "Optional year range label to record in the report, for example `2015-2026`."
                    .to_string(),
            )),
        ),
        (
            "top_k".to_string(),
            JsonSchema::integer(Some(
                "Number of distinct documents to keep after reranking and deduplication, capped at 30. Defaults to 12."
                    .to_string(),
            )),
        ),
        (
            "category".to_string(),
            JsonSchema::string(Some(
                "Optional vector category filter. Defaults to bio_literature, which is the current deployed collection content."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: LITERATURE_MAP_TOOL_NAME.to_string(),
        description: "Map the literature for a topic from the codex-med vector knowledge base into a reproducible research_projects/<project_id>/ directory of reranked, per-document evidence: an evidence table, report, BibTeX citations, and a provenance record. Use when a topic calls for a durable, citable evidence package rather than an answer in chat, and re-run the same project_id to append a run to its history. Use search_vector_knowledge instead for a quick lookup that needs no files on disk."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["topic".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_pubmed_literature_map_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "topic".to_string(),
            JsonSchema::string(Some(
                "Required scientific topic or question to map into a PubMed-backed research project, for example `integrated stress response aging neurodegeneration`."
                    .to_string(),
            )),
        ),
        (
            "pubmed_query".to_string(),
            JsonSchema::string(Some(
                "Optional explicit PubMed query. Defaults to `topic`; use this for field tags and boolean logic such as `integrated stress response AND neurodegeneration[Title/Abstract]`."
                    .to_string(),
            )),
        ),
        (
            "project_id".to_string(),
            JsonSchema::string(Some(
                "Optional filesystem-safe research project id. Defaults to a slug derived from topic."
                    .to_string(),
            )),
        ),
        (
            "retmax".to_string(),
            JsonSchema::integer(Some(
                "Maximum PubMed records to include, capped at 50. Defaults to 10.".to_string(),
            )),
        ),
        (
            "sort".to_string(),
            JsonSchema::string_enum(
                vec![serde_json::json!("relevance"), serde_json::json!("pub_date")],
                Some("PubMed esearch ordering. Defaults to relevance.".to_string()),
            ),
        ),
        (
            "min_year".to_string(),
            JsonSchema::integer(Some(
                "Optional earliest publication year, inclusive. Requires max_year to also be set."
                    .to_string(),
            )),
        ),
        (
            "max_year".to_string(),
            JsonSchema::integer(Some(
                "Optional latest publication year, inclusive. Requires min_year to also be set."
                    .to_string(),
            )),
        ),
        (
            "fetch_abstracts".to_string(),
            JsonSchema::boolean(Some(
                "Whether to fetch MEDLINE details for each PMID, including abstracts and MeSH terms. Defaults to true."
                    .to_string(),
            )),
        ),
        (
            "max_mesh_terms".to_string(),
            JsonSchema::integer(Some(
                "Maximum MeSH headings to keep per PubMed record, capped at 200. Defaults to 50."
                    .to_string(),
            )),
        ),
        (
            "validate_citations".to_string(),
            JsonSchema::boolean(Some(
                "Whether to validate DOI-bearing PubMed records against Crossref and annotate citation status in CSV, JSONL, report, and provenance. Defaults to false."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: PUBMED_LITERATURE_MAP_TOOL_NAME.to_string(),
        description: "Build a PubMed-backed literature map without changing the vector-only literature_map workflow: search PubMed, optionally fetch abstracts and MeSH terms for each PMID, then write research_projects/<project_id>/ literature records, report, BibTeX citations, provenance, and project manifest."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["topic".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

#[cfg(test)]
#[path = "science_workbench_spec_tests.rs"]
mod tests;
