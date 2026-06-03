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
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use sqlx::Column;
use sqlx::ConnectOptions;
use sqlx::Row;
use sqlx::TypeInfo;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use tracing::log::LevelFilter;

const DEFAULT_DATABASE_FILENAME: &str = "training_ready_v1.sqlite";
const TABLE_NAME: &str = "antibody_training_records";
const DEFAULT_MAX_ROWS: usize = 25;
const MAX_ROWS_CAP: usize = 100;
const DEFAULT_MAX_CELL_CHARS: usize = 2_000;
const MAX_CELL_CHARS_CAP: usize = 10_000;

const TABLE_COLUMNS: &[(&str, &str, &str)] = &[
    (
        "row_id",
        "INTEGER",
        "Synthetic row id assigned during TSV import.",
    ),
    ("antibody_id", "TEXT", "Stable antibody record id."),
    ("paper_id", "TEXT", "Patent, paper, or source document id."),
    ("dataset", "TEXT", "Source dataset label."),
    (
        "antibody_name",
        "TEXT",
        "Antibody name as extracted or normalized.",
    ),
    (
        "raw_target_name",
        "TEXT",
        "Raw target name from source text.",
    ),
    ("standard_target_name", "TEXT", "Normalized target name."),
    (
        "target_modality",
        "TEXT",
        "Target modality, for example protein.",
    ),
    ("sequence_scope", "TEXT", "Antigen sequence scope."),
    (
        "antigen_uniprot_id",
        "TEXT",
        "UniProt accession for the antigen target.",
    ),
    ("antigen_sequence", "TEXT", "Antigen amino-acid sequence."),
    (
        "vh_sequence_aa",
        "TEXT",
        "Antibody heavy-chain amino-acid sequence.",
    ),
    (
        "vl_sequence_aa",
        "TEXT",
        "Antibody light-chain amino-acid sequence, nullable.",
    ),
    (
        "assay_type",
        "TEXT",
        "Assay type used for the affinity label.",
    ),
    ("assay_value_raw", "TEXT", "Original assay value string."),
    ("assay_value_nM", "REAL", "Assay value normalized to nM."),
    ("assay_value_M", "REAL", "Assay value normalized to M."),
    (
        "label_log10_KD_M",
        "REAL",
        "Training label as log10(KD in M).",
    ),
    (
        "label_relation",
        "TEXT",
        "Relation for the label, for example exact.",
    ),
    ("mapping_status", "TEXT", "Target mapping status."),
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
        let ToolInvocation { payload, turn, .. } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "query_antibody_training_records received unsupported payload".to_string(),
                ));
            }
        };
        let args: QueryAntibodyTrainingRecordsArgs = parse_arguments(&arguments)?;
        let cwd = {
            #[allow(deprecated)]
            turn.cwd.as_path().to_path_buf()
        };
        let output = query_antibody_training_records(args, &cwd).await?;

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
    #[serde(default)]
    database_path: Option<String>,
}

fn default_max_rows() -> usize {
    DEFAULT_MAX_ROWS
}

fn default_max_cell_chars() -> usize {
    DEFAULT_MAX_CELL_CHARS
}

async fn query_antibody_training_records(
    args: QueryAntibodyTrainingRecordsArgs,
    cwd: &Path,
) -> Result<String, FunctionCallError> {
    let sql = validate_read_only_sql(&args.sql)?;
    let database_path = resolve_database_path(args.database_path.as_deref(), cwd);
    if !database_path.is_file() {
        return Err(FunctionCallError::RespondToModel(format!(
            "antibody training SQLite database not found at {}; expected a database imported from training_ready_v1.tsv",
            database_path.display()
        )));
    }

    let max_rows = args.max_rows.clamp(1, MAX_ROWS_CAP);
    let max_cell_chars = args.max_cell_chars.clamp(1, MAX_CELL_CHARS_CAP);
    let limited_sql = format!("SELECT * FROM ({sql}) LIMIT {max_rows}");
    let pool = open_read_only_sqlite(&database_path).await?;
    let query_result = run_limited_query(&pool, &limited_sql, max_cell_chars).await;
    let schema_result = if args.include_schema {
        Some(load_schema(&pool).await)
    } else {
        None
    };
    pool.close().await;

    let (columns, rows) = query_result?;
    let mut output = json!({
        "database_path": database_path,
        "table": TABLE_NAME,
        "sql": sql,
        "applied_sql": limited_sql,
        "max_rows": max_rows,
        "returned_rows": rows.len(),
        "columns": columns,
        "rows": rows,
    });

    if let Some(schema) = schema_result {
        output["schema"] = schema?;
    }

    serde_json::to_string_pretty(&output).map_err(|err| {
        FunctionCallError::Fatal(format!("failed to serialize SQL query result: {err}"))
    })
}

fn resolve_database_path(database_path: Option<&str>, cwd: &Path) -> PathBuf {
    match database_path.map(str::trim).filter(|path| !path.is_empty()) {
        Some(path) => {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        }
        None => cwd.join(DEFAULT_DATABASE_FILENAME),
    }
}

async fn open_read_only_sqlite(
    database_path: &Path,
) -> Result<sqlx::SqlitePool, FunctionCallError> {
    let options = SqliteConnectOptions::new()
        .filename(database_path)
        .create_if_missing(false)
        .read_only(true)
        .immutable(true)
        .log_statements(LevelFilter::Off);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!(
                "failed to open antibody training SQLite database {} read-only: {err}",
                database_path.display()
            ))
        })
}

async fn run_limited_query(
    pool: &sqlx::SqlitePool,
    sql: &str,
    max_cell_chars: usize,
) -> Result<(Vec<String>, Vec<Value>), FunctionCallError> {
    let rows = sqlx::query(sql).fetch_all(pool).await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("antibody training SQL query failed: {err}"))
    })?;
    let columns = rows
        .first()
        .map(|row| {
            row.columns()
                .iter()
                .map(|column| column.name().to_string())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let rows = rows
        .iter()
        .map(|row| row_to_json(row, max_cell_chars))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((columns, rows))
}

fn row_to_json(
    row: &sqlx::sqlite::SqliteRow,
    max_cell_chars: usize,
) -> Result<Value, FunctionCallError> {
    let mut object = Map::new();
    for (idx, column) in row.columns().iter().enumerate() {
        let value = sqlite_cell_to_json(row, idx, max_cell_chars)?;
        object.insert(column.name().to_string(), value);
    }
    Ok(Value::Object(object))
}

fn sqlite_cell_to_json(
    row: &sqlx::sqlite::SqliteRow,
    idx: usize,
    max_cell_chars: usize,
) -> Result<Value, FunctionCallError> {
    if let Ok(value) = row.try_get::<Option<i64>, _>(idx) {
        return Ok(value.map_or(Value::Null, Value::from));
    }
    if let Ok(value) = row.try_get::<Option<f64>, _>(idx) {
        return Ok(value.map_or(Value::Null, Value::from));
    }
    if let Ok(value) = row.try_get::<Option<String>, _>(idx) {
        return Ok(match value {
            Some(value) => Value::String(truncate_cell(value, max_cell_chars)),
            None => Value::Null,
        });
    }
    if let Ok(value) = row.try_get::<Option<Vec<u8>>, _>(idx) {
        return Ok(match value {
            Some(value) => Value::String(format!("<{} byte blob>", value.len())),
            None => Value::Null,
        });
    }
    let column_type = row
        .columns()
        .get(idx)
        .map(|column| column.type_info().name().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    Err(FunctionCallError::RespondToModel(format!(
        "failed to decode SQLite column {idx} with type {column_type}"
    )))
}

fn truncate_cell(value: String, max_cell_chars: usize) -> String {
    let length = value.chars().count();
    if length <= max_cell_chars {
        return value;
    }
    let truncated = value.chars().take(max_cell_chars).collect::<String>();
    format!(
        "{truncated}...<truncated {} chars>",
        length.saturating_sub(max_cell_chars)
    )
}

async fn load_schema(pool: &sqlx::SqlitePool) -> Result<Value, FunctionCallError> {
    let row_count = sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {TABLE_NAME}"))
        .fetch_one(pool)
        .await
        .map_err(|err| {
            FunctionCallError::RespondToModel(format!("failed to read antibody table count: {err}"))
        })?;

    Ok(json!({
        "table": TABLE_NAME,
        "row_count": row_count,
        "columns": TABLE_COLUMNS.iter().map(|(name, ty, description)| {
            json!({
                "name": name,
                "type": ty,
                "description": description,
            })
        }).collect::<Vec<_>>(),
        "recommended_indexes": [
            "antibody_name",
            "standard_target_name",
            "antigen_uniprot_id",
            "paper_id",
            "assay_value_nM",
            "label_log10_KD_M"
        ],
    }))
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
    if sql.contains(';') {
        return Err(FunctionCallError::RespondToModel(
            "only one SELECT/WITH statement is allowed; omit semicolons".to_string(),
        ));
    }

    let lower = sql.trim_start().to_ascii_lowercase();
    if !(lower.starts_with("select") || lower.starts_with("with")) {
        return Err(FunctionCallError::RespondToModel(
            "only read-only SELECT or WITH queries are allowed".to_string(),
        ));
    }

    let forbidden = HashSet::from([
        "alter", "analyze", "attach", "create", "delete", "detach", "drop", "insert", "pragma",
        "reindex", "replace", "update", "vacuum",
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

    Ok(sql.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::Value;

    #[test]
    fn validates_read_only_sql_shape() {
        assert!(validate_read_only_sql("SELECT * FROM antibody_training_records").is_ok());
        assert!(
            validate_read_only_sql(
                "WITH best AS (SELECT * FROM antibody_training_records) SELECT * FROM best"
            )
            .is_ok()
        );
        assert!(validate_read_only_sql("DELETE FROM antibody_training_records").is_err());
        assert!(validate_read_only_sql("SELECT * FROM x; SELECT * FROM y").is_err());
        assert!(validate_read_only_sql("PRAGMA table_info(antibody_training_records)").is_err());
    }

    #[test]
    fn resolves_default_database_path_against_cwd() {
        let cwd = Path::new("/tmp/workspace");
        assert_eq!(
            resolve_database_path(None, cwd),
            PathBuf::from("/tmp/workspace/training_ready_v1.sqlite")
        );
        assert_eq!(
            resolve_database_path(Some("data/sample.sqlite"), cwd),
            PathBuf::from("/tmp/workspace/data/sample.sqlite")
        );
        assert_eq!(
            resolve_database_path(Some("/tmp/sample.sqlite"), cwd),
            PathBuf::from("/tmp/sample.sqlite")
        );
    }

    #[tokio::test]
    async fn queries_sample_antibody_training_database() {
        let temp = tempfile::tempdir().expect("create temp dir");
        let db_path = temp.path().join(DEFAULT_DATABASE_FILENAME);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(&db_path)
                    .create_if_missing(true)
                    .log_statements(LevelFilter::Off),
            )
            .await
            .expect("open sqlite");
        sqlx::query(
            r#"
CREATE TABLE antibody_training_records (
    row_id INTEGER PRIMARY KEY,
    antibody_id TEXT NOT NULL,
    antibody_name TEXT NOT NULL,
    standard_target_name TEXT NOT NULL,
    antigen_uniprot_id TEXT NOT NULL,
    assay_value_nM REAL NOT NULL
)
            "#,
        )
        .execute(&pool)
        .await
        .expect("create table");
        sqlx::query(
            "INSERT INTO antibody_training_records (antibody_id, antibody_name, standard_target_name, antigen_uniprot_id, assay_value_nM) VALUES (?, ?, ?, ?, ?)",
        )
        .bind("row-1")
        .bind("A-Na-16")
        .bind("Tumor necrosis factor receptor superfamily member 9")
        .bind("Q07011")
        .bind(20.8_f64)
        .execute(&pool)
        .await
        .expect("insert row");
        pool.close().await;

        let output = query_antibody_training_records(
            QueryAntibodyTrainingRecordsArgs {
                sql: "SELECT antibody_name, antigen_uniprot_id, assay_value_nM FROM antibody_training_records WHERE antigen_uniprot_id = 'Q07011'".to_string(),
                max_rows: 10,
                max_cell_chars: 100,
                include_schema: false,
                database_path: None,
            },
            temp.path(),
        )
        .await
        .expect("query should succeed");
        let value: Value = serde_json::from_str(&output).expect("output should be JSON");
        assert_eq!(value["returned_rows"], 1);
        assert_eq!(value["rows"][0]["antibody_name"], "A-Na-16");
        assert_eq!(value["rows"][0]["assay_value_nM"], 20.8);
    }
}
