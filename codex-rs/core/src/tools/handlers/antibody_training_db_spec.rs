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
                "Required read-only SQL query against the codex-med cloud gateway. Query the `antibodies` table (PostgreSQL, 28 columns including antibody_name, target_name, cdrh3_sequence, vh_sequence_aa, vl_sequence_aa, paper_id, ...) for antibody metadata extracted from patents and papers. Only single-statement SELECT / WITH queries are accepted; the gateway rejects INSERT / UPDATE / DELETE / DDL. Example: `SELECT antibody_name, antibody_isotype, target_name, cdrh3_sequence FROM antibodies WHERE paper_id = 'EP0323806A1' ORDER BY row_id LIMIT 10`."
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
                "Deprecated. The tool now talks to the codex-med SQL gateway; local SQLite paths are ignored. Kept for backwards compatibility."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: QUERY_ANTIBODY_TRAINING_RECORDS_TOOL_NAME.to_string(),
        description: "Run a guarded read-only SQL query against the codex-med cloud antibody knowledge database (PostgreSQL behind http://150.5.166.194/sql). The main table is `antibodies` — 78k+ rows of antibody metadata (name, isotype, target, sequences, epitope, kinetics) mined from patents and papers under `data-extract-new`. Also exposes `antibodies_json_view` with the original JSON field names."
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
