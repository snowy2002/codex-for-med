use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

pub const QUERY_ANTIBODY_TRAINING_RECORDS_TOOL_NAME: &str = "query_antibody_training_records";

pub fn create_query_antibody_training_records_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "sql".to_string(),
            JsonSchema::string(Some(
                "Required read-only SQLite SELECT or WITH query. Query `antibody_training_records` for antibody affinity records, or `literature_documents` / `literature_documents_fts` for imported patent and paper markdown knowledge. Example: `SELECT antibody_name, standard_target_name, antigen_uniprot_id, assay_value_nM FROM antibody_training_records WHERE antigen_uniprot_id = 'Q07011' ORDER BY assay_value_nM LIMIT 10`."
                    .to_string(),
            )),
        ),
        (
            "max_rows".to_string(),
            JsonSchema::integer(Some(
                "Maximum rows returned after applying the query, capped at 100. Defaults to 25."
                    .to_string(),
            )),
        ),
        (
            "max_cell_chars".to_string(),
            JsonSchema::integer(Some(
                "Maximum characters returned for a text cell, capped at 10000. Defaults to 2000."
                    .to_string(),
            )),
        ),
        (
            "include_schema".to_string(),
            JsonSchema::boolean(Some(
                "Whether to include table schema metadata in the response. Defaults to false."
                    .to_string(),
            )),
        ),
        (
            "database_path".to_string(),
            JsonSchema::string(Some(
                "Optional SQLite database path. Relative paths are resolved against the Codex working directory. Defaults to `training_ready_v1.sqlite` in the working directory."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: QUERY_ANTIBODY_TRAINING_RECORDS_TOOL_NAME.to_string(),
        description: "Run a guarded read-only SQL query against the local biomedical SQLite knowledge database. It contains `antibody_training_records` loaded from training_ready_v1.tsv and `literature_documents` for imported patent/paper markdown content, with `literature_documents_fts` available for SQLite FTS5 full-text search."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["sql".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

#[cfg(test)]
#[path = "antibody_training_db_spec_tests.rs"]
mod tests;
