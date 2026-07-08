use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub const LIST_MED_KNOWLEDGE_COLLECTIONS_TOOL_NAME: &str = "list_med_knowledge_collections";
pub const DESCRIBE_MED_DATABASE_TOOL_NAME: &str = "describe_med_database";
pub const LITERATURE_MAP_TOOL_NAME: &str = "literature_map";

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
                "Number of vector evidence chunks to collect, capped at 30. Defaults to 12."
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
        description: "Run a read-only literature-map workflow over the codex-med vector knowledge base and create a reproducible research_projects/<project_id>/ directory with evidence_table.csv, report.md, citations.bib, and provenance/run.json."
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
