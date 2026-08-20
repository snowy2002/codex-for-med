//! Cloud-backed `query_antibody_training_records` tool.
//!
//! Historically this tool read a local SQLite file (`training_ready_v1.sqlite`)
//! containing the `antibody_training_records` table. The medical fork now ships
//! against a hosted Postgres deployment behind the codex-med gateway:
//!
//!   https://codex-med.opensii.ai/sql/query
//!
//! The gateway enforces a SELECT/WITH whitelist, LIMIT injection, and a 15 s
//! statement timeout server-side. We keep the client-side validator as a
//! defence-in-depth guard so obviously bad queries never leave the machine.
//!
//! Bearer token and endpoint URL are baked into the binary so `codex-med`
//! works out of the box. Both can be overridden with environment variables
//! (`CODEX_MED_SQL_API_URL`, `CODEX_MED_SQL_API_TOKEN`) — useful for rotating
//! credentials or pointing at a private mirror.

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::antibody_training_db_spec::QUERY_ANTIBODY_TRAINING_RECORDS_TOOL_NAME;
use crate::tools::handlers::antibody_training_db_spec::create_query_antibody_training_records_tool;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashSet;
use std::time::Duration;

const TABLE_NAME: &str = "antibodies";
const DEFAULT_MAX_ROWS: usize = 25;
const MAX_ROWS_CAP: usize = 100;
const DEFAULT_MAX_CELL_CHARS: usize = 2_000;
const MAX_CELL_CHARS_CAP: usize = 10_000;
const HTTP_TIMEOUT_SECONDS: u64 = 30;

const DEFAULT_SQL_API_URL: &str = "http://150.5.166.194/sql";

// The Postgres-backed schema behind the gateway. Kept static so that
// `include_schema: true` returns useful column docs even if the server is
// briefly unreachable.
const TABLE_COLUMNS: &[(&str, &str, &str)] = &[
    ("row_id", "BIGINT", "Surrogate primary key."),
    (
        "paper_id",
        "TEXT",
        "Patent / paper identifier (prediction.json top-level key).",
    ),
    ("document_title", "TEXT", "Source document title."),
    (
        "document_category",
        "TEXT",
        "Source document category, e.g. patent / paper.",
    ),
    (
        "antibody_name",
        "TEXT",
        "Antibody name as extracted from the source.",
    ),
    ("antibody_type", "TEXT", "Antibody format, e.g. mAb, ScFv."),
    (
        "antibody_isotype",
        "TEXT",
        "Antibody isotype, e.g. mouse IgG1.",
    ),
    (
        "source",
        "TEXT",
        "Provenance: murine / human / chimeric / humanized / ...",
    ),
    ("target_name", "TEXT", "Target antigen name."),
    (
        "target_type",
        "TEXT",
        "Target category, e.g. Tumor antigen.",
    ),
    (
        "cross_reactivity",
        "TEXT",
        "Reported cross-reactivity or lack thereof.",
    ),
    ("epitope", "TEXT", "Reported epitope."),
    (
        "experiment",
        "TEXT",
        "Assay used to characterise the antibody.",
    ),
    (
        "binding_kinetics_kd",
        "TEXT",
        "KD as reported in the source.",
    ),
    (
        "binding_kinetics_kon",
        "TEXT",
        "kon as reported in the source.",
    ),
    (
        "binding_kinetics_koff",
        "TEXT",
        "koff as reported in the source.",
    ),
    ("binding_ec50", "TEXT", "EC50 as reported."),
    (
        "mechanism_of_action",
        "TEXT",
        "Reported mechanism of action.",
    ),
    (
        "quantitative_metric",
        "TEXT",
        "Free-form numeric metric, e.g. `<8 ng/ml for OD 0.1`.",
    ),
    ("structure", "TEXT", "Reported structural information."),
    ("cdrh3_sequence", "TEXT", "CDR-H3 amino-acid sequence."),
    (
        "vh_sequence_aa",
        "TEXT",
        "Heavy-chain variable region sequence.",
    ),
    (
        "vl_sequence_aa",
        "TEXT",
        "Light-chain variable region sequence.",
    ),
    (
        "thermal_stability_tm",
        "TEXT",
        "Reported thermal stability Tm.",
    ),
    ("in_vivo_half_life", "TEXT", "Reported in-vivo half-life."),
    ("in_vivo_efficacy", "TEXT", "Reported in-vivo efficacy."),
    (
        "reference_source",
        "TEXT",
        "Citation string for the record.",
    ),
    ("imported_at", "TIMESTAMPTZ", "Row ingestion timestamp."),
];

#[derive(Default)]
pub struct AntibodyTrainingDbHandler;

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for AntibodyTrainingDbHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(QUERY_ANTIBODY_TRAINING_RECORDS_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_query_antibody_training_records_tool()
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation { payload, .. } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "query_antibody_training_records received unsupported payload".to_string(),
                ));
            }
        };
        let args: QueryAntibodyTrainingRecordsArgs = parse_arguments(&arguments)?;
        let output = query_antibody_training_records(args).await?;
        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            output,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for AntibodyTrainingDbHandler {}

#[derive(Debug, Deserialize)]
struct QueryAntibodyTrainingRecordsArgs {
    sql: String,
    #[serde(default = "default_max_rows")]
    max_rows: usize,
    #[serde(default = "default_max_cell_chars")]
    max_cell_chars: usize,
    #[serde(default)]
    include_schema: bool,
    /// Legacy field from the local-SQLite version. Ignored server-side, but
    /// accepted here so old prompts don't break.
    #[serde(default)]
    #[allow(dead_code)]
    database_path: Option<String>,
}

fn default_max_rows() -> usize {
    DEFAULT_MAX_ROWS
}

fn default_max_cell_chars() -> usize {
    DEFAULT_MAX_CELL_CHARS
}

fn resolve_sql_api_base() -> String {
    env_non_empty("CODEX_MED_SQL_API_URL").unwrap_or_else(|| DEFAULT_SQL_API_URL.to_string())
}

fn resolve_sql_api_token() -> Result<String, FunctionCallError> {
    env_non_empty("CODEX_MED_SQL_API_TOKEN").ok_or_else(|| {
        FunctionCallError::RespondToModel(
            "CODEX_MED_SQL_API_TOKEN must be set in the Codex Med runtime environment".to_string(),
        )
    })
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn query_antibody_training_records(
    args: QueryAntibodyTrainingRecordsArgs,
) -> Result<String, FunctionCallError> {
    let sql = validate_read_only_sql(&args.sql)?;
    let max_rows = args.max_rows.clamp(1, MAX_ROWS_CAP);
    let max_cell_chars = args.max_cell_chars.clamp(1, MAX_CELL_CHARS_CAP);

    let base = resolve_sql_api_base();
    let token = resolve_sql_api_token()?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS))
        .build()
        .map_err(|err| FunctionCallError::Fatal(format!("failed to build HTTP client: {err}")))?;

    let response = call_query_endpoint(&client, &base, &token, &sql, max_rows).await?;

    // Server returns { sql, applied_sql, returned_rows, rows, ... }
    let mut rows_value = response
        .get("rows")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    truncate_cells(&mut rows_value, max_cell_chars);
    let columns = extract_columns(&rows_value);

    let mut output = json!({
        "backend": {
            "provider": "codex-med-sql-gateway",
            "url": format!("{}/query", trim_trailing_slash(&base)),
            "table": TABLE_NAME,
        },
        "sql": sql,
        "applied_sql": response.get("applied_sql").cloned().unwrap_or(Value::Null),
        "max_rows": max_rows,
        "returned_rows": rows_value.as_array().map(std::vec::Vec::len).unwrap_or(0),
        "columns": columns,
        "rows": rows_value,
        "server_duration_ms": response.get("duration_ms").cloned().unwrap_or(Value::Null),
    });

    if args.include_schema {
        let schema = fetch_schema(&client, &base, &token)
            .await
            .unwrap_or_else(|_| {
                // Server unreachable during schema fetch is not fatal — fall back
                // to the static column list so the model still gets useful docs.
                json!({
                    "table": TABLE_NAME,
                    "columns": TABLE_COLUMNS.iter().map(|(name, ty, description)| {
                        json!({"name": name, "type": ty, "description": description})
                    }).collect::<Vec<_>>(),
                    "note": "static fallback; live schema endpoint was unreachable",
                })
            });
        output["schema"] = schema;
    }

    serde_json::to_string_pretty(&output).map_err(|err| {
        FunctionCallError::Fatal(format!("failed to serialize SQL query result: {err}"))
    })
}

async fn call_query_endpoint(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    sql: &str,
    limit: usize,
) -> Result<Value, FunctionCallError> {
    let url = format!("{}/query", trim_trailing_slash(base));
    let body = json!({ "sql": sql, "limit": limit });
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let response = client
        .post(&url)
        .headers(headers)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "codex-med SQL gateway request failed: {err}"
            ))
        })?;
    let status = response.status();
    let body_text = response.text().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to read codex-med SQL gateway response: {err}"
        ))
    })?;
    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "codex-med SQL gateway returned HTTP {status}: {}",
            truncate_for_error(&body_text)
        )));
    }
    serde_json::from_str::<Value>(&body_text).map_err(|err| {
        FunctionCallError::RespondToModel(format!(
            "failed to parse codex-med SQL gateway response JSON: {err}"
        ))
    })
}

async fn fetch_schema(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Result<Value, FunctionCallError> {
    let url = format!("{}/schema", trim_trailing_slash(base));
    let response = client
        .get(&url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("schema request failed: {err}"))
        })?;
    let status = response.status();
    let body_text = response.text().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read schema response: {err}"))
    })?;
    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "schema endpoint returned HTTP {status}: {}",
            truncate_for_error(&body_text)
        )));
    }
    serde_json::from_str::<Value>(&body_text).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse schema JSON: {err}"))
    })
}

fn extract_columns(rows: &Value) -> Vec<String> {
    rows.as_array()
        .and_then(|arr| arr.first())
        .and_then(Value::as_object)
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default()
}

fn truncate_cells(rows: &mut Value, max_cell_chars: usize) {
    let Some(arr) = rows.as_array_mut() else {
        return;
    };
    for row in arr {
        let Some(obj) = row.as_object_mut() else {
            continue;
        };
        for (_key, cell) in obj.iter_mut() {
            if let Value::String(s) = cell
                && s.chars().count() > max_cell_chars
            {
                let truncated: String = s.chars().take(max_cell_chars).collect();
                let dropped = s.chars().count() - max_cell_chars;
                *cell = Value::String(format!("{truncated}...<truncated {dropped} chars>"));
            }
        }
    }
}

fn truncate_for_error(value: &str) -> String {
    let max_chars = 500;
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    format!(
        "{}...<truncated>",
        value.chars().take(max_chars).collect::<String>()
    )
}

fn trim_trailing_slash(value: &str) -> &str {
    value.trim_end_matches('/')
}

fn validate_read_only_sql(sql: &str) -> Result<String, FunctionCallError> {
    let sql = sql.trim();
    if sql.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "sql must not be empty".to_string(),
        ));
    }
    if sql.len() > 20_000 {
        return Err(FunctionCallError::RespondToModel(
            "sql is too long; keep read-only queries under 20000 bytes".to_string(),
        ));
    }
    if sql.contains('\0') {
        return Err(FunctionCallError::RespondToModel(
            "sql must not contain NUL bytes".to_string(),
        ));
    }
    // Trailing ';' is stripped by the gateway anyway; anything more looks like
    // an attempt to smuggle a second statement.
    let effective = sql.trim_end_matches(';').trim();
    if effective.contains(';') {
        return Err(FunctionCallError::RespondToModel(
            "only one SELECT/WITH statement is allowed; omit intermediate semicolons".to_string(),
        ));
    }
    if effective.contains("--") || effective.contains("/*") {
        return Err(FunctionCallError::RespondToModel(
            "SQL comments (`--`, `/*`) are not allowed".to_string(),
        ));
    }

    let lower = effective.to_ascii_lowercase();
    if !(lower.starts_with("select") || lower.starts_with("with")) {
        return Err(FunctionCallError::RespondToModel(
            "only read-only SELECT or WITH queries are allowed".to_string(),
        ));
    }

    let forbidden = HashSet::from([
        "alter",
        "analyze",
        "attach",
        "call",
        "checkpoint",
        "cluster",
        "commit",
        "copy",
        "create",
        "delete",
        "detach",
        "do",
        "drop",
        "grant",
        "insert",
        "listen",
        "notify",
        "pragma",
        "reindex",
        "replace",
        "reset",
        "revoke",
        "rollback",
        "savepoint",
        "set",
        "truncate",
        "update",
        "vacuum",
    ]);
    let tokens = lower
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_'))
        .filter(|token| !token.is_empty());
    for token in tokens {
        if forbidden.contains(token) {
            return Err(FunctionCallError::RespondToModel(format!(
                "read-only SQL rejected forbidden keyword `{token}`"
            )));
        }
    }

    Ok(effective.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    #[test]
    fn validates_read_only_sql_shape() {
        assert!(validate_read_only_sql("SELECT * FROM antibodies").is_ok());
        assert!(
            validate_read_only_sql("WITH best AS (SELECT * FROM antibodies) SELECT * FROM best")
                .is_ok()
        );
        // Trailing ; is fine (matches server behaviour).
        assert!(validate_read_only_sql("SELECT * FROM antibodies;").is_ok());
        assert!(validate_read_only_sql("DELETE FROM antibodies").is_err());
        assert!(validate_read_only_sql("SELECT * FROM x; SELECT * FROM y").is_err());
        assert!(validate_read_only_sql("SELECT * -- comment\nFROM antibodies").is_err());
        assert!(validate_read_only_sql("PRAGMA table_info(antibodies)").is_err());
        assert!(validate_read_only_sql("UPDATE antibodies SET target_name = 'x'").is_err());
    }

    #[tokio::test]
    async fn queries_gateway_and_returns_rows() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "sql": "SELECT antibody_name FROM antibodies WHERE paper_id = 'EP0323806A1'",
                "applied_sql": "SELECT * FROM (...) LIMIT 25",
                "returned_rows": 2,
                "duration_ms": 3.1,
                "rows": [
                    {"antibody_name": "CE 25"},
                    {"antibody_name": "CE 75-5-6"},
                ]
            })))
            .mount(&server)
            .await;

        // Force the tool at the mock server for the duration of this test.
        // SAFETY: no other threads are inspecting these env vars in tests.
        unsafe {
            std::env::set_var("CODEX_MED_SQL_API_URL", server.uri());
            std::env::set_var("CODEX_MED_SQL_API_TOKEN", "test-token");
        }

        let output = query_antibody_training_records(QueryAntibodyTrainingRecordsArgs {
            sql: "SELECT antibody_name FROM antibodies WHERE paper_id = 'EP0323806A1'".to_string(),
            max_rows: 25,
            max_cell_chars: 100,
            include_schema: false,
            database_path: None,
        })
        .await
        .expect("query should succeed");

        unsafe {
            std::env::remove_var("CODEX_MED_SQL_API_URL");
            std::env::remove_var("CODEX_MED_SQL_API_TOKEN");
        }

        let value: Value = serde_json::from_str(&output).expect("output should be JSON");
        assert_eq!(value["returned_rows"], 2);
        assert_eq!(value["rows"][0]["antibody_name"], "CE 25");
        assert_eq!(value["rows"][1]["antibody_name"], "CE 75-5-6");
    }
}
