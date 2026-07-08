use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::science_workbench_spec::DESCRIBE_MED_DATABASE_TOOL_NAME;
use crate::tools::handlers::science_workbench_spec::LIST_MED_KNOWLEDGE_COLLECTIONS_TOOL_NAME;
use crate::tools::handlers::science_workbench_spec::LITERATURE_MAP_TOOL_NAME;
use crate::tools::handlers::science_workbench_spec::create_describe_med_database_tool;
use crate::tools::handlers::science_workbench_spec::create_list_med_knowledge_collections_tool;
use crate::tools::handlers::science_workbench_spec::create_literature_map_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::time::Duration;

const HTTP_TIMEOUT: Duration = Duration::from_secs(45);

const SQL_API_URL: &str = "http://150.5.166.194/sql";
const SQL_API_TOKEN: &str = "bc62ea6d3039564fd945291fd29534e1b7e08c6cfe19d239dc28bbed69a8962c";

const QDRANT_URL: &str = "http://150.5.166.194/vector";
const QDRANT_COLLECTION: &str = "medical_knowledge_qwen3_4b";
const QDRANT_API_KEY: &str = "e7d682ca3d11a77ac70a747018439892c137ee556aeec86f7bf4f5da40caf32a";

const EMBEDDING_URL: &str = "http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_embedding_4b/v1/embeddings";
const EMBEDDING_MODEL: &str = "/model_dir/Qwen3-Embedding-4B";
const EMBEDDING_API_KEY: &str = "OTE4MDhiNDE1YmIwYTEzNjE1ZTA2YjFhMTVhNmU3MzczNGVlMTkzZA==";

#[derive(Clone, Copy)]
enum ScienceWorkbenchToolKind {
    ListMedKnowledgeCollections,
    DescribeMedDatabase,
    LiteratureMap,
}

pub struct ScienceWorkbenchHandler {
    kind: ScienceWorkbenchToolKind,
}

impl ScienceWorkbenchHandler {
    fn new(kind: ScienceWorkbenchToolKind) -> Self {
        Self { kind }
    }

    pub fn list_med_knowledge_collections() -> Self {
        Self::new(ScienceWorkbenchToolKind::ListMedKnowledgeCollections)
    }

    pub fn describe_med_database() -> Self {
        Self::new(ScienceWorkbenchToolKind::DescribeMedDatabase)
    }

    pub fn literature_map() -> Self {
        Self::new(ScienceWorkbenchToolKind::LiteratureMap)
    }

    fn client() -> Result<reqwest::Client, FunctionCallError> {
        reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent("codex-for-med/science-workbench")
            .build()
            .map_err(|err| FunctionCallError::Fatal(format!("failed to build HTTP client: {err}")))
    }
}

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for ScienceWorkbenchHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(match self.kind {
            ScienceWorkbenchToolKind::ListMedKnowledgeCollections => {
                LIST_MED_KNOWLEDGE_COLLECTIONS_TOOL_NAME
            }
            ScienceWorkbenchToolKind::DescribeMedDatabase => DESCRIBE_MED_DATABASE_TOOL_NAME,
            ScienceWorkbenchToolKind::LiteratureMap => LITERATURE_MAP_TOOL_NAME,
        })
    }

    fn spec(&self) -> ToolSpec {
        match self.kind {
            ScienceWorkbenchToolKind::ListMedKnowledgeCollections => {
                create_list_med_knowledge_collections_tool()
            }
            ScienceWorkbenchToolKind::DescribeMedDatabase => create_describe_med_database_tool(),
            ScienceWorkbenchToolKind::LiteratureMap => create_literature_map_tool(),
        }
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
                return Err(FunctionCallError::RespondToModel(format!(
                    "{} handler received unsupported payload",
                    self.tool_name()
                )));
            }
        };

        let client = Self::client()?;
        let output = match self.kind {
            ScienceWorkbenchToolKind::ListMedKnowledgeCollections => {
                list_med_knowledge_collections(&client).await?
            }
            ScienceWorkbenchToolKind::DescribeMedDatabase => {
                let args: DescribeMedDatabaseArgs = parse_arguments(&arguments)?;
                describe_med_database(&client, args).await?
            }
            ScienceWorkbenchToolKind::LiteratureMap => {
                let args: LiteratureMapArgs = parse_arguments(&arguments)?;
                #[allow(deprecated)]
                let cwd = turn.cwd.as_path();
                literature_map(&client, args, cwd).await?
            }
        };

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            output,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for ScienceWorkbenchHandler {}

#[derive(Debug, Deserialize)]
struct DescribeMedDatabaseArgs {
    #[serde(default = "default_true")]
    include_schema: bool,
    #[serde(default = "default_true")]
    include_vector_details: bool,
}

#[derive(Debug, Deserialize)]
struct LiteratureMapArgs {
    topic: String,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    year_range: Option<String>,
    #[serde(default = "default_literature_top_k")]
    top_k: usize,
    #[serde(default)]
    category: Option<String>,
}

fn default_true() -> bool {
    true
}

fn default_literature_top_k() -> usize {
    12
}

async fn list_med_knowledge_collections(
    client: &reqwest::Client,
) -> Result<String, FunctionCallError> {
    let vector = qdrant_collection_info(client).await?;
    let sql = sql_schema(client).await?;
    let output = json!({
        "backends": [
            {
                "kind": "vector",
                "provider": "qdrant",
                "url": QDRANT_URL,
                "collection": QDRANT_COLLECTION,
                "status": vector.pointer("/result/status").cloned().unwrap_or(Value::Null),
                "points_count": vector.pointer("/result/points_count").cloned().unwrap_or(Value::Null),
                "indexed_vectors_count": vector.pointer("/result/indexed_vectors_count").cloned().unwrap_or(Value::Null),
                "dimensions": vector.pointer("/result/config/params/vectors/size").cloned().unwrap_or(Value::Null),
                "distance": vector.pointer("/result/config/params/vectors/distance").cloned().unwrap_or(Value::Null),
                "current_categories": ["bio_literature"],
                "current_source_types": ["ocr_repaired_markdown"],
                "recommended_tool": "search_vector_knowledge",
                "notes": "Current deployed vector content is OCR-repaired biomedical/patent markdown chunks from data-extract-new."
            },
            {
                "kind": "sql",
                "provider": "codex-med-sql-gateway",
                "url": SQL_API_URL,
                "table": sql.get("table").cloned().unwrap_or_else(|| json!("antibodies")),
                "row_count": sql.get("row_count").cloned().unwrap_or(Value::Null),
                "recommended_tool": "query_antibody_training_records",
                "notes": "Read-only PostgreSQL antibody metadata extracted from patents and papers."
            }
        ]
    });
    pretty_json(output)
}

async fn describe_med_database(
    client: &reqwest::Client,
    args: DescribeMedDatabaseArgs,
) -> Result<String, FunctionCallError> {
    let mut output = json!({
        "summary": {
            "sql": "Structured antibody metadata and sequence/assay fields.",
            "vector": "Semantic search over OCR-repaired biomedical markdown chunks.",
            "write_support": "Read-only from Codex tools. Use offline ingestion/admin pipelines to add data."
        },
        "recommended_usage": [
            "Use query_antibody_training_records for exact antibody, target, sequence, KD, EC50, and patent/paper queries.",
            "Use search_vector_knowledge for semantic retrieval of source context from OCR-repaired biomedical literature/patent markdown.",
            "Use literature_map to create a reproducible research_projects/<project_id>/ evidence package."
        ]
    });

    if args.include_schema {
        output["sql_database"] = sql_schema(client).await?;
    }

    if args.include_vector_details {
        let info = qdrant_collection_info(client).await?;
        let counts = vector_category_counts(client).await?;
        output["vector_database"] = json!({
            "provider": "qdrant",
            "url": QDRANT_URL,
            "collection": QDRANT_COLLECTION,
            "collection_info": info.get("result").cloned().unwrap_or(info),
            "observed_counts": counts,
            "payload_fields": [
                "document_id",
                "chunk_id",
                "chunk_index",
                "category",
                "source_type",
                "source_uri",
                "paper_id",
                "title",
                "snippet",
                "text",
                "tags",
                "is_deleted",
                "imported_at",
                "project_id"
            ]
        });
    }

    pretty_json(output)
}

async fn literature_map(
    client: &reqwest::Client,
    args: LiteratureMapArgs,
    cwd: &Path,
) -> Result<String, FunctionCallError> {
    let topic = args.topic.trim();
    if topic.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "topic must not be empty".to_string(),
        ));
    }
    let project_id = args
        .project_id
        .as_deref()
        .map(slugify_project_id)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| slugify_project_id(topic));
    let top_k = args.top_k.clamp(1, 30);
    let category = args
        .category
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("bio_literature");

    let embedding = embed_query(client, topic).await?;
    let points = qdrant_search(client, &embedding, top_k, category).await?;

    let project_dir = cwd.join("research_projects").join(&project_id);
    let literature_dir = project_dir.join("literature");
    let code_dir = project_dir.join("code");
    let provenance_dir = project_dir.join("provenance");
    let figures_dir = project_dir.join("figures");
    let analysis_dir = project_dir.join("analysis");
    fs::create_dir_all(&literature_dir).map_err(fs_error("create literature directory"))?;
    fs::create_dir_all(&code_dir).map_err(fs_error("create code directory"))?;
    fs::create_dir_all(&provenance_dir).map_err(fs_error("create provenance directory"))?;
    fs::create_dir_all(&figures_dir).map_err(fs_error("create figures directory"))?;
    fs::create_dir_all(&analysis_dir).map_err(fs_error("create analysis directory"))?;

    let evidence_rows = points
        .iter()
        .enumerate()
        .map(|(idx, point)| evidence_row(idx + 1, point))
        .collect::<Vec<_>>();

    let evidence_csv = render_evidence_csv(&evidence_rows);
    let report = render_literature_report(topic, args.year_range.as_deref(), &evidence_rows);
    let citations = render_citations_bib(&evidence_rows);
    let run = json!({
        "workflow": "literature_map",
        "project_id": project_id,
        "topic": topic,
        "year_range": args.year_range,
        "top_k": top_k,
        "category": category,
        "backends": {
            "vector": {
                "provider": "qdrant",
                "url": QDRANT_URL,
                "collection": QDRANT_COLLECTION,
                "embedding_model": EMBEDDING_MODEL
            }
        },
        "outputs": {
            "evidence_table": relative_display(&project_dir, &literature_dir.join("evidence_table.csv")),
            "report": relative_display(&project_dir, &literature_dir.join("report.md")),
            "citations": relative_display(&project_dir, &literature_dir.join("citations.bib"))
        },
        "evidence_count": evidence_rows.len()
    });

    write_file(&literature_dir.join("evidence_table.csv"), &evidence_csv)?;
    write_file(&literature_dir.join("report.md"), &report)?;
    write_file(&literature_dir.join("citations.bib"), &citations)?;
    write_file(
        &provenance_dir.join("run.json"),
        &serde_json::to_string_pretty(&run).map_err(json_error("serialize run provenance"))?,
    )?;

    let output = json!({
        "project_id": project_id,
        "project_dir": project_dir,
        "topic": topic,
        "evidence_count": evidence_rows.len(),
        "created_files": [
            literature_dir.join("evidence_table.csv"),
            literature_dir.join("report.md"),
            literature_dir.join("citations.bib"),
            provenance_dir.join("run.json")
        ],
        "next_steps": [
            "Review literature/evidence_table.csv for noisy OCR chunks.",
            "Use report.md as the first evidence map draft.",
            "Add human curation notes before using the map in a manuscript."
        ]
    });
    pretty_json(output)
}

async fn sql_schema(client: &reqwest::Client) -> Result<Value, FunctionCallError> {
    let url = format!("{}/schema", SQL_API_URL);
    let response = client
        .get(url)
        .bearer_auth(SQL_API_TOKEN)
        .send()
        .await
        .map_err(http_error("SQL schema request failed"))?;
    parse_http_json(response, "SQL schema").await
}

async fn qdrant_collection_info(client: &reqwest::Client) -> Result<Value, FunctionCallError> {
    let url = format!(
        "{}/collections/{}",
        QDRANT_URL.trim_end_matches('/'),
        QDRANT_COLLECTION
    );
    let response = client
        .get(url)
        .header("api-key", QDRANT_API_KEY)
        .send()
        .await
        .map_err(http_error("Qdrant collection request failed"))?;
    parse_http_json(response, "Qdrant collection").await
}

async fn vector_category_counts(client: &reqwest::Client) -> Result<Value, FunctionCallError> {
    let total = qdrant_count(client, None).await?;
    let bio_literature = qdrant_count(
        client,
        Some(json!({"must": [{"key": "category", "match": {"value": "bio_literature"}}]})),
    )
    .await?;
    let ocr_repaired_markdown = qdrant_count(
        client,
        Some(
            json!({"must": [{"key": "source_type", "match": {"value": "ocr_repaired_markdown"}}]}),
        ),
    )
    .await?;
    Ok(json!({
        "total": total,
        "by_category": {
            "bio_literature": bio_literature,
            "web_knowledge": 0,
            "database_record": 0,
            "experiment_record": 0,
            "protocol": 0,
            "clinical_guideline": 0,
            "patent": 0,
            "internal_note": 0
        },
        "by_source_type": {
            "ocr_repaired_markdown": ocr_repaired_markdown
        }
    }))
}

async fn qdrant_count(
    client: &reqwest::Client,
    filter: Option<Value>,
) -> Result<u64, FunctionCallError> {
    let url = format!(
        "{}/collections/{}/points/count",
        QDRANT_URL.trim_end_matches('/'),
        QDRANT_COLLECTION
    );
    let mut body = json!({"exact": true});
    if let Some(filter) = filter {
        body["filter"] = filter;
    }
    let response = client
        .post(url)
        .header("api-key", QDRANT_API_KEY)
        .json(&body)
        .send()
        .await
        .map_err(http_error("Qdrant count request failed"))?;
    let value = parse_http_json(response, "Qdrant count").await?;
    Ok(value
        .pointer("/result/count")
        .and_then(Value::as_u64)
        .unwrap_or(0))
}

async fn embed_query(client: &reqwest::Client, query: &str) -> Result<Vec<f64>, FunctionCallError> {
    let response = client
        .post(EMBEDDING_URL)
        .bearer_auth(EMBEDDING_API_KEY)
        .json(&json!({"model": EMBEDDING_MODEL, "input": query}))
        .send()
        .await
        .map_err(http_error("embedding request failed"))?;
    let value = parse_http_json(response, "embedding").await?;
    let embedding = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("embedding"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "embedding response missing data[0].embedding".to_string(),
            )
        })?;
    embedding
        .iter()
        .map(|value| {
            value.as_f64().ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "embedding response contains a non-numeric value".to_string(),
                )
            })
        })
        .collect()
}

async fn qdrant_search(
    client: &reqwest::Client,
    embedding: &[f64],
    limit: usize,
    category: &str,
) -> Result<Vec<Value>, FunctionCallError> {
    let url = format!(
        "{}/collections/{}/points/search",
        QDRANT_URL.trim_end_matches('/'),
        QDRANT_COLLECTION
    );
    let body = json!({
        "vector": embedding,
        "limit": limit,
        "with_payload": true,
        "with_vector": false,
        "filter": {
            "must": [{"key": "category", "match": {"value": category}}],
            "must_not": [{"key": "is_deleted", "match": {"value": true}}]
        }
    });
    let response = client
        .post(url)
        .header("api-key", QDRANT_API_KEY)
        .json(&body)
        .send()
        .await
        .map_err(http_error("Qdrant search request failed"))?;
    let value = parse_http_json(response, "Qdrant search").await?;
    Ok(value
        .get("result")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

async fn parse_http_json(
    response: reqwest::Response,
    label: &str,
) -> Result<Value, FunctionCallError> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(http_error("failed to read HTTP response"))?;
    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "{label} request returned HTTP {status}: {}",
            truncate_for_error(&text)
        )));
    }
    serde_json::from_str(&text).map_err(|err| {
        FunctionCallError::RespondToModel(format!("{label} response was not valid JSON: {err}"))
    })
}

fn http_error(
    context: &'static str,
) -> impl FnOnce(reqwest::Error) -> FunctionCallError + Send + 'static {
    move |err| FunctionCallError::RespondToModel(format!("{context}: {err}"))
}

fn json_error(
    context: &'static str,
) -> impl FnOnce(serde_json::Error) -> FunctionCallError + Send + 'static {
    move |err| FunctionCallError::Fatal(format!("{context}: {err}"))
}

fn fs_error(
    context: &'static str,
) -> impl FnOnce(std::io::Error) -> FunctionCallError + Send + 'static {
    move |err| FunctionCallError::RespondToModel(format!("{context}: {err}"))
}

fn write_file(path: &Path, contents: &str) -> Result<(), FunctionCallError> {
    fs::write(path, contents).map_err(fs_error("write research project file"))
}

fn pretty_json(value: Value) -> Result<String, FunctionCallError> {
    serde_json::to_string_pretty(&value).map_err(json_error("serialize JSON output"))
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

#[derive(Debug)]
struct EvidenceRow {
    rank: usize,
    score: f64,
    point_id: String,
    document_id: String,
    chunk_id: String,
    paper_id: String,
    category: String,
    source_type: String,
    title: String,
    source_uri: String,
    snippet: String,
}

fn evidence_row(rank: usize, point: &Value) -> EvidenceRow {
    let payload = point.get("payload").unwrap_or(&Value::Null);
    EvidenceRow {
        rank,
        score: point
            .get("score")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        point_id: value_to_string(point.get("id").unwrap_or(&Value::Null)),
        document_id: payload_string(payload, "document_id"),
        chunk_id: payload_string(payload, "chunk_id"),
        paper_id: payload_string(payload, "paper_id"),
        category: payload_string(payload, "category"),
        source_type: payload_string(payload, "source_type"),
        title: payload_string(payload, "title"),
        source_uri: payload_string(payload, "source_uri"),
        snippet: first_non_empty_payload(payload, &["snippet", "text", "content"]),
    }
}

fn payload_string(payload: &Value, key: &str) -> String {
    payload
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn first_non_empty_payload(payload: &Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(value) = payload.get(*key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.chars().take(1_200).collect();
            }
        }
    }
    String::new()
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn render_evidence_csv(rows: &[EvidenceRow]) -> String {
    let mut out =
        "rank,score,point_id,document_id,chunk_id,paper_id,category,source_type,title,source_uri,snippet\n"
            .to_string();
    for row in rows {
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{}\n",
            row.rank,
            row.score,
            csv_escape(&row.point_id),
            csv_escape(&row.document_id),
            csv_escape(&row.chunk_id),
            csv_escape(&row.paper_id),
            csv_escape(&row.category),
            csv_escape(&row.source_type),
            csv_escape(&row.title),
            csv_escape(&row.source_uri),
            csv_escape(&row.snippet),
        ));
    }
    out
}

fn render_literature_report(topic: &str, year_range: Option<&str>, rows: &[EvidenceRow]) -> String {
    let mut out = String::new();
    out.push_str("# Literature Map\n\n");
    out.push_str(&format!("Topic: {topic}\n\n"));
    if let Some(year_range) = year_range
        && !year_range.trim().is_empty()
    {
        out.push_str(&format!("Year range: {}\n\n", year_range.trim()));
    }
    out.push_str("## Evidence Table Summary\n\n");
    out.push_str("| Rank | Score | Paper ID | Title | Source |\n");
    out.push_str("| ---: | ---: | --- | --- | --- |\n");
    for row in rows {
        out.push_str(&format!(
            "| {} | {:.4} | {} | {} | {} |\n",
            row.rank,
            row.score,
            markdown_escape(&row.paper_id),
            markdown_escape(&row.title),
            markdown_escape(&row.source_uri),
        ));
    }
    out.push_str("\n## Notes For Human Curation\n\n");
    out.push_str("- The current vector collection is OCR-repaired markdown and may contain table/OCR noise.\n");
    out.push_str("- Treat this as a first-pass evidence map; verify source documents before manuscript use.\n");
    out.push_str("- Add mechanism, disease, model system, evidence level, and citation status columns during curation.\n\n");
    out.push_str("## Retrieved Snippets\n\n");
    for row in rows {
        out.push_str(&format!(
            "### {}. {}\n\nSource: `{}`\n\n{}\n\n",
            row.rank,
            if row.title.is_empty() {
                &row.paper_id
            } else {
                &row.title
            },
            row.source_uri,
            row.snippet
        ));
    }
    out
}

fn render_citations_bib(rows: &[EvidenceRow]) -> String {
    let mut out = String::new();
    for row in rows {
        let key = if row.paper_id.is_empty() {
            format!("codex_med_chunk_{}", row.rank)
        } else {
            sanitize_bib_key(&row.paper_id)
        };
        out.push_str(&format!(
            "@misc{{{},\n  title = {{{}}},\n  howpublished = {{{}}},\n  note = {{codex-med vector chunk {}; document_id={}}}\n}}\n\n",
            key,
            bib_escape(if row.title.is_empty() {
                &row.document_id
            } else {
                &row.title
            }),
            bib_escape(&row.source_uri),
            row.chunk_id,
            row.document_id,
        ));
    }
    out
}

fn csv_escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn markdown_escape(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn bib_escape(value: &str) -> String {
    value.replace('{', "\\{").replace('}', "\\}")
}

fn sanitize_bib_key(value: &str) -> String {
    let key = value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>();
    if key.is_empty() {
        "codex_med_source".to_string()
    } else {
        key
    }
}

fn slugify_project_id(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('_');
            last_dash = true;
        }
    }
    while slug.ends_with('_') {
        slug.pop();
    }
    if slug.is_empty() {
        "literature_map".to_string()
    } else {
        slug.chars().take(80).collect()
    }
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugifies_project_ids() {
        assert_eq!(
            slugify_project_id("Integrated Stress Response + Aging"),
            "integrated_stress_response_aging"
        );
        assert_eq!(slugify_project_id(""), "literature_map");
    }

    #[test]
    fn csv_escapes_special_characters() {
        assert_eq!(csv_escape("plain"), "plain");
        assert_eq!(csv_escape("a,b"), "\"a,b\"");
        assert_eq!(csv_escape("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn renders_evidence_csv_header() {
        let csv = render_evidence_csv(&[]);
        assert!(csv.starts_with("rank,score,point_id"));
    }
}
