use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::science_workbench_spec::DESCRIBE_MED_DATABASE_TOOL_NAME;
use crate::tools::handlers::science_workbench_spec::LIST_MED_KNOWLEDGE_COLLECTIONS_TOOL_NAME;
use crate::tools::handlers::science_workbench_spec::LITERATURE_MAP_TOOL_NAME;
use crate::tools::handlers::science_workbench_spec::PUBMED_LITERATURE_MAP_TOOL_NAME;
use crate::tools::handlers::science_workbench_spec::RESOLVE_LITERATURE_REVIEW_TOOL_NAME;
use crate::tools::handlers::science_workbench_spec::create_describe_med_database_tool;
use crate::tools::handlers::science_workbench_spec::create_list_med_knowledge_collections_tool;
use crate::tools::handlers::science_workbench_spec::create_literature_map_tool;
use crate::tools::handlers::science_workbench_spec::create_pubmed_literature_map_tool;
use crate::tools::handlers::science_workbench_spec::create_resolve_literature_review_tool;
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

#[path = "science_workbench/pubmed_literature_map.rs"]
mod pubmed_literature_map;
use self::pubmed_literature_map::PubmedLiteratureMapArgs;
use self::pubmed_literature_map::pubmed_literature_map as run_pubmed_literature_map;
#[path = "science_workbench/literature_artifacts.rs"]
mod literature_artifacts;
#[path = "science_workbench/literature_baseline.rs"]
mod literature_baseline;
#[path = "science_workbench/literature_legacy.rs"]
mod literature_legacy;
#[path = "science_workbench/literature_registry.rs"]
mod literature_registry;
#[path = "science_workbench/literature_registry_identity.rs"]
mod literature_registry_identity;
#[path = "science_workbench/literature_registry_merge.rs"]
mod literature_registry_merge;
#[path = "science_workbench/literature_registry_schema.rs"]
mod literature_registry_schema;
#[path = "science_workbench/literature_vector_jobs.rs"]
mod literature_vector_jobs;
#[path = "science_workbench/pubmed_cache.rs"]
mod pubmed_cache;
#[path = "science_workbench/pubmed_chunks.rs"]
mod pubmed_chunks;
#[path = "science_workbench/pubmed_vector_ingest.rs"]
mod pubmed_vector_ingest;
use self::literature_artifacts::commit_literature_run;
use self::literature_artifacts::new_run_id;
use self::literature_artifacts::recover_incomplete_literature_runs;
use self::literature_artifacts::render_literature_ids;
use self::literature_baseline::initialize_qdrant_baseline;
use self::literature_legacy::migrate_legacy_project;
use self::literature_registry::LiteratureInput;
use self::literature_registry::LiteratureRegistry;
use self::literature_registry::RegistrationOutcome;
use self::literature_registry::SourceRecord;
use self::literature_registry_identity::identifiers_from_source_uri;
use self::literature_registry_identity::paper_id_from_source_uri;
use self::pubmed_vector_ingest::PubmedVectorConfig;
use self::pubmed_vector_ingest::PubmedVectorIngestor;
#[path = "science_workbench/project_manifest.rs"]
mod project_manifest;
#[path = "science_workbench/rerank.rs"]
mod rerank;
use self::project_manifest::record_project_run_unlocked;
use self::project_manifest::with_project_lock;
use self::rerank::LITERATURE_MAX_RECALL;
use self::rerank::LITERATURE_RECALL_MULTIPLIER;
use self::rerank::RERANKER_MODEL;
use self::rerank::ScoredPoint;
use self::rerank::dedupe_points;
use self::rerank::rerank_points;

const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

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
    PubmedLiteratureMap,
    ResolveLiteratureReview,
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

    pub fn pubmed_literature_map() -> Self {
        Self::new(ScienceWorkbenchToolKind::PubmedLiteratureMap)
    }

    pub fn resolve_literature_review() -> Self {
        Self::new(ScienceWorkbenchToolKind::ResolveLiteratureReview)
    }

    fn client() -> Result<reqwest::Client, FunctionCallError> {
        reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .connect_timeout(HTTP_CONNECT_TIMEOUT)
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
            ScienceWorkbenchToolKind::PubmedLiteratureMap => PUBMED_LITERATURE_MAP_TOOL_NAME,
            ScienceWorkbenchToolKind::ResolveLiteratureReview => {
                RESOLVE_LITERATURE_REVIEW_TOOL_NAME
            }
        })
    }

    fn spec(&self) -> ToolSpec {
        match self.kind {
            ScienceWorkbenchToolKind::ListMedKnowledgeCollections => {
                create_list_med_knowledge_collections_tool()
            }
            ScienceWorkbenchToolKind::DescribeMedDatabase => create_describe_med_database_tool(),
            ScienceWorkbenchToolKind::LiteratureMap => create_literature_map_tool(),
            ScienceWorkbenchToolKind::PubmedLiteratureMap => create_pubmed_literature_map_tool(),
            ScienceWorkbenchToolKind::ResolveLiteratureReview => {
                create_resolve_literature_review_tool()
            }
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
            ScienceWorkbenchToolKind::PubmedLiteratureMap => {
                let args: PubmedLiteratureMapArgs = parse_arguments(&arguments)?;
                #[allow(deprecated)]
                let cwd = turn.cwd.as_path();
                run_pubmed_literature_map(&client, args, cwd).await?
            }
            ScienceWorkbenchToolKind::ResolveLiteratureReview => {
                let args: ResolveLiteratureReviewArgs = parse_arguments(&arguments)?;
                #[allow(deprecated)]
                let cwd = turn.cwd.as_path();
                resolve_literature_review(args, cwd).await?
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
struct ResolveLiteratureReviewArgs {
    action: ResolveLiteratureReviewAction,
    review_case_id: String,
    reason: String,
    #[serde(default)]
    canonical_literature_id: Option<String>,
    #[serde(default)]
    alias_literature_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResolveLiteratureReviewAction {
    MergeSame,
    KeepDifferent,
}

async fn resolve_literature_review(
    args: ResolveLiteratureReviewArgs,
    cwd: &Path,
) -> Result<String, FunctionCallError> {
    let review_case_id = args.review_case_id.trim();
    let reason = args.reason.trim();
    if review_case_id.is_empty() || reason.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "review_case_id and reason must not be empty".to_string(),
        ));
    }
    let registry = LiteratureRegistry::open_workspace(cwd)
        .await
        .map_err(|err| {
            FunctionCallError::Fatal(format!("failed to open literature registry: {err:#}"))
        })?;
    match args.action {
        ResolveLiteratureReviewAction::MergeSame => {
            let canonical = args
                .canonical_literature_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "canonical_literature_id is required for merge_same".to_string(),
                    )
                })?;
            let alias = args
                .alias_literature_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "alias_literature_id is required for merge_same".to_string(),
                    )
                })?;
            let canonical = registry
                .merge_literatures(
                    canonical,
                    alias,
                    reason,
                    Some(review_case_id),
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to resolve literature review: {err:#}"
                    ))
                })?;
            pretty_json(json!({
                "status": "resolved_same_literature",
                "review_case_id": review_case_id,
                "canonical_literature_id": canonical,
                "alias_literature_id": alias,
            }))
        }
        ResolveLiteratureReviewAction::KeepDifferent => {
            registry
                .resolve_review_as_different(
                    review_case_id,
                    reason,
                    &chrono::Utc::now().to_rfc3339(),
                )
                .await
                .map_err(|err| {
                    FunctionCallError::RespondToModel(format!(
                        "failed to resolve literature review: {err:#}"
                    ))
                })?;
            pretty_json(json!({
                "status": "resolved_different_literatures",
                "review_case_id": review_case_id,
            }))
        }
    }
}

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
    let started_at = chrono::Utc::now();
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
    let recall_k = (top_k * LITERATURE_RECALL_MULTIPLIER)
        .min(LITERATURE_MAX_RECALL)
        .max(top_k);
    let recall_points = qdrant_search(client, &embedding, recall_k, category).await?;

    // Rerank the recall pool with the shared Qwen3 reranker so evidence ordering
    // matches search_vector_knowledge, then aggregate chunks of the same document.
    let (ranked_points, rerank_used, rerank_note) =
        rerank_points(client, topic, recall_points).await;
    let (deduped, total_chunks) = dedupe_points(ranked_points, LITERATURE_MAX_RECALL);

    let observed_at = chrono::Utc::now();
    let now_rfc3339 = observed_at.to_rfc3339();
    let run_id = new_run_id(&started_at, "literature_map");
    let registry = LiteratureRegistry::open_workspace(cwd)
        .await
        .map_err(|err| {
            FunctionCallError::Fatal(format!("failed to open literature registry: {err:#}"))
        })?;
    let baseline_initialization = if std::env::var("CODEX_MED_INITIALIZE_LITERATURE_BASELINE")
        .is_ok_and(|value| value == "1")
    {
        let qdrant_url = std::env::var("CODEX_MED_VECTOR_QDRANT_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| QDRANT_URL.to_string());
        let collection = std::env::var("CODEX_MED_VECTOR_COLLECTION")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| QDRANT_COLLECTION.to_string());
        let api_key = std::env::var("CODEX_MED_VECTOR_QDRANT_API_KEY")
            .ok()
            .or_else(|| std::env::var("QDRANT_API_KEY").ok())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| QDRANT_API_KEY.to_string());
        Some(
            initialize_qdrant_baseline(
                client,
                &registry,
                cwd,
                &qdrant_url,
                &collection,
                &api_key,
                &now_rfc3339,
            )
            .await
            .map_err(|err| {
                FunctionCallError::Fatal(format!(
                    "failed to initialize literature registry baseline: {err:#}"
                ))
            })?,
        )
    } else {
        None
    };
    let mut evidence_rows: Vec<EvidenceRow> = Vec::new();
    let mut canonical_indexes: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    let mut registration_conflicts = Vec::new();
    for scored in &deduped {
        let mut row = evidence_row(evidence_rows.len() + 1, scored);
        let (pmid, doi) = identifiers_from_source_uri(&row.source_uri);
        let paper_id = (!row.paper_id.trim().is_empty())
            .then(|| row.paper_id.clone())
            .or_else(|| paper_id_from_source_uri(&row.source_uri));
        let source = (!row.document_id.trim().is_empty()).then(|| SourceRecord {
            system: format!("qdrant:{QDRANT_COLLECTION}"),
            key: row.document_id.clone(),
            match_method: "qdrant_document".to_string(),
        });
        let registration = registry
            .register(
                LiteratureInput {
                    pmid,
                    doi,
                    paper_id,
                    title: Some(row.title.clone()),
                    metadata: json!({
                        "raw_identifiers": {
                            "paper_id": row.paper_id,
                            "source_uri": row.source_uri,
                        },
                        "field_sources": {
                            "title": {
                                "source": format!("qdrant:{QDRANT_COLLECTION}"),
                                "observed_at": now_rfc3339,
                            }
                        }
                    }),
                    ..Default::default()
                },
                source,
                &now_rfc3339,
            )
            .await
            .map_err(|err| {
                FunctionCallError::Fatal(format!(
                    "failed to register local literature metadata: {err:#}"
                ))
            })?;
        let registered = match registration {
            RegistrationOutcome::Registered(registered) => registered,
            RegistrationOutcome::Conflict {
                review_case_id,
                matched_literature_ids,
            } => {
                registration_conflicts.push(json!({
                    "review_case_id": review_case_id,
                    "matched_literature_ids": matched_literature_ids,
                    "document_id": row.document_id,
                    "paper_id": row.paper_id,
                }));
                continue;
            }
        };
        row.literature_id = registered.literature_id;
        row.review_case_id = registered.review_case_id;
        row.match_method = registered.match_method;
        row.duplicate_status = if row.review_case_id.is_some() {
            "possible_duplicate".to_string()
        } else {
            "canonical".to_string()
        };
        if let Some(existing_index) = canonical_indexes.get(&row.literature_id).copied() {
            evidence_rows[existing_index].chunk_count += row.chunk_count;
            evidence_rows[existing_index]
                .matched_documents
                .push(row.document_id);
            continue;
        }
        canonical_indexes.insert(row.literature_id.clone(), evidence_rows.len());
        row.rank = evidence_rows.len() + 1;
        evidence_rows.push(row);
        if evidence_rows.len() >= top_k {
            break;
        }
    }

    let literature_ids = evidence_rows
        .iter()
        .map(|row| row.literature_id.clone())
        .collect::<Vec<_>>();
    for row in &mut evidence_rows {
        let literature = registry
            .get(&row.literature_id)
            .await
            .map_err(|err| {
                FunctionCallError::Fatal(format!(
                    "failed to resolve global literature metadata: {err:#}"
                ))
            })?
            .ok_or_else(|| {
                FunctionCallError::Fatal(format!(
                    "global literature metadata {} disappeared during the run",
                    row.literature_id
                ))
            })?;
        row.apply_global_metadata(&literature);
    }
    let ids_csv = render_literature_ids(&literature_ids);
    let report = render_literature_report(topic, args.year_range.as_deref(), &evidence_rows);
    let citations = render_citations_bib(&evidence_rows);
    let project_dir = cwd.join("research_projects").join(&project_id);
    let finished_at = chrono::Utc::now().to_rfc3339();
    let run_status = if registration_conflicts.is_empty() {
        "completed"
    } else {
        "completed_with_errors"
    };

    let mut run = json!({
        "schema_version": 1,
        "workflow": "literature_map",
        "run_id": run_id,
        "started_at": started_at.to_rfc3339(),
        "finished_at": finished_at,
        "status": run_status,
        "project_id": project_id,
        "topic": topic,
        "year_range": args.year_range,
        "top_k": top_k,
        "recall_top_k": recall_k,
        "category": category,
        "input": {
            "original": {
                "topic": args.topic,
                "project_id": args.project_id,
                "year_range": args.year_range,
                "top_k": args.top_k,
                "category": args.category,
            },
            "normalized": {
                "topic": topic,
                "project_id": project_id,
                "year_range": args.year_range,
                "top_k": top_k,
                "category": category,
            }
        },
        "backends": {
            "vector": {
                "provider": "qdrant",
                "url": QDRANT_URL,
                "collection": QDRANT_COLLECTION,
                "embedding_model": EMBEDDING_MODEL,
                "reranker_model": RERANKER_MODEL
            }
        },
        "rerank": {
            "used": rerank_used,
            "note": rerank_note
        },
        "deduplication": {
            "enabled": true,
            "document_keys": ["paper_id", "document_id", "source_uri", "title"],
            "final_key": "canonical_literature_id",
            "distinct_literatures": evidence_rows.len(),
            "chunks_considered": total_chunks
        },
        "registry": relative_display(cwd, registry.path()),
        "baseline_initialization": baseline_initialization,
        "registration_conflicts": registration_conflicts,
        "errors": registration_conflicts.iter().map(|conflict| json!({
            "stage": "sqlite_registration",
            "retryable": false,
            "recovery": "Resolve the review_case_id before rerunning the affected record.",
            "details": conflict,
        })).collect::<Vec<_>>(),
        "hits": evidence_rows.iter().map(|row| json!({
            "rank": row.rank,
            "literature_id": row.literature_id,
            "paper_id": row.paper_id,
            "document_id": row.document_id,
            "source_uri": row.source_uri,
            "external_identifiers": {
                "paper_id": row.paper_id,
                "doi": row.doi,
            },
            "matched_documents": row.matched_documents,
            "match_method": row.match_method,
            "duplicate_status": row.duplicate_status,
            "vector_job_status": "already_vectorized",
            "vector_score": row.score,
            "rerank_score": row.rerank_score,
            "chunk_count": row.chunk_count,
            "review_case_id": row.review_case_id,
        })).collect::<Vec<_>>(),
        "counts": {
            "input_chunks": total_chunks,
            "output_literatures": evidence_rows.len(),
            "registration_conflicts": registration_conflicts.len(),
            "vector_statuses": {
                "already_vectorized": evidence_rows.len(),
            },
        }
    });

    let (committed, manifest_path) = with_project_lock(&project_dir, || {
        let legacy_migration = migrate_legacy_project(&project_dir)?;
        let recovered_runs = recover_incomplete_literature_runs(&project_dir)?;
        if let Some(object) = run.as_object_mut() {
            object.insert("legacy_migration".to_string(), json!(legacy_migration));
            object.insert("recovered_runs".to_string(), json!(recovered_runs));
        }
        let committed = commit_literature_run(
            cwd,
            &project_id,
            "local",
            "literature_map",
            &run_id,
            &ids_csv,
            &report,
            &citations,
            run.clone(),
        )?;
        let manifest_path = record_project_run_unlocked(
            &committed.project_dir,
            &project_id,
            topic,
            &finished_at,
            run.clone(),
            "local",
            &committed.provenance,
            &[
                ("literature_ids", &committed.latest_ids),
                ("report", &committed.latest_report),
                ("citations", &committed.latest_citations),
            ],
        )?;
        Ok((committed, manifest_path))
    })?;

    let output = json!({
        "project_id": project_id,
        "project_dir": project_dir,
        "topic": topic,
        "evidence_count": evidence_rows.len(),
        "recall_top_k": recall_k,
        "distinct_documents": evidence_rows.len(),
        "chunks_considered": total_chunks,
        "rerank_used": rerank_used,
        "manifest": manifest_path,
        "created_files": [
            committed.latest_ids,
            committed.latest_report,
            committed.latest_citations,
            committed.provenance,
            committed.snapshot_dir,
            manifest_path
        ],
        "next_steps": [
            "Review literature/local/literature_ids.csv and report.md.",
            "Inspect this run's provenance for scores, chunk counts, and review cases.",
            "Add human curation notes before using the map in a manuscript."
        ]
    });
    pretty_json(output)
}

async fn sql_schema(client: &reqwest::Client) -> Result<Value, FunctionCallError> {
    let url = format!("{SQL_API_URL}/schema");
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
    literature_id: String,
    review_case_id: Option<String>,
    score: f64,
    rerank_score: Option<f64>,
    chunk_count: usize,
    document_id: String,
    chunk_id: String,
    paper_id: String,
    title: String,
    source_uri: String,
    snippet: String,
    authors: Vec<String>,
    journal: String,
    publication_date: String,
    doi: String,
    match_method: String,
    duplicate_status: String,
    matched_documents: Vec<String>,
}

fn evidence_row(rank: usize, scored: &ScoredPoint) -> EvidenceRow {
    let point = &scored.point;
    let payload = point.get("payload").unwrap_or(&Value::Null);
    EvidenceRow {
        rank,
        literature_id: String::new(),
        review_case_id: None,
        score: point
            .get("score")
            .and_then(Value::as_f64)
            .unwrap_or_default(),
        rerank_score: scored.rerank_score,
        chunk_count: scored.chunk_count,
        document_id: payload_string(payload, "document_id"),
        chunk_id: payload_string(payload, "chunk_id"),
        paper_id: payload_string(payload, "paper_id"),
        title: payload_string(payload, "title"),
        source_uri: payload_string(payload, "source_uri"),
        snippet: first_non_empty_payload(payload, &["snippet", "text", "content"]),
        authors: Vec::new(),
        journal: String::new(),
        publication_date: String::new(),
        doi: String::new(),
        match_method: String::new(),
        duplicate_status: "canonical".to_string(),
        matched_documents: vec![payload_string(payload, "document_id")],
    }
}

impl EvidenceRow {
    fn apply_global_metadata(&mut self, literature: &literature_registry::Literature) {
        if let Some(title) = literature.title.as_deref() {
            self.title = title.to_string();
        }
        if let Some(paper_id) = literature.paper_id.as_deref() {
            self.paper_id = paper_id.to_string();
        }
        self.authors = literature.authors.clone();
        self.journal = literature.journal.clone().unwrap_or_default();
        self.publication_date = literature.publication_date.clone().unwrap_or_default();
        self.doi = literature.doi.clone().unwrap_or_default();
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
    out.push_str(
        "| Rank | Literature ID | Rerank | Vector | Chunks | Paper ID | Title | Source |\n",
    );
    out.push_str("| ---: | --- | ---: | ---: | ---: | --- | --- | --- |\n");
    for row in rows {
        let rerank = row
            .rerank_score
            .map(|score| format!("{score:.4}"))
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "| {} | {} | {} | {:.4} | {} | {} | {} | {} |\n",
            row.rank,
            row.literature_id,
            rerank,
            row.score,
            row.chunk_count,
            markdown_escape(&row.paper_id),
            markdown_escape(&row.title),
            markdown_escape(&row.source_uri),
        ));
    }
    out.push_str("\n## Notes For Human Curation\n\n");
    out.push_str("- Chunks were reranked with the Qwen3 reranker and aggregated per document (paper_id/document_id/source_uri/title); `chunk_count` shows how many chunks backed each row.\n");
    out.push_str("- The current vector collection is OCR-repaired markdown and may contain table/OCR noise.\n");
    out.push_str("- Treat this as a first-pass evidence map; verify source documents before manuscript use.\n");
    out.push_str("- Add mechanism, disease, model system, evidence level, and citation status columns during curation.\n\n");
    out.push_str("## Retrieved Snippets\n\n");
    for row in rows {
        let rerank = row
            .rerank_score
            .map(|score| format!("{score:.4}"))
            .unwrap_or_else(|| "-".to_string());
        out.push_str(&format!(
            "### {}. {}\n\nSource: `{}`  ·  rerank: {}  ·  chunks: {}\n\n{}\n\n",
            row.rank,
            if row.title.is_empty() {
                &row.paper_id
            } else {
                &row.title
            },
            row.source_uri,
            rerank,
            row.chunk_count,
            row.snippet
        ));
    }
    out
}

fn render_citations_bib(rows: &[EvidenceRow]) -> String {
    let mut out = String::new();
    for row in rows {
        let key = format!(
            "lit_{}",
            row.literature_id
                .chars()
                .filter(char::is_ascii_hexdigit)
                .take(12)
                .collect::<String>()
        );
        out.push_str(&format!(
            "@misc{{{},\n  title = {{{}}},\n",
            key,
            bib_escape(if row.title.is_empty() {
                &row.document_id
            } else {
                &row.title
            }),
        ));
        if !row.authors.is_empty() {
            out.push_str(&format!(
                "  author = {{{}}},\n",
                bib_escape(&row.authors.join(" and "))
            ));
        }
        if !row.journal.is_empty() {
            out.push_str(&format!("  journal = {{{}}},\n", bib_escape(&row.journal)));
        }
        if let Some(year) = row.publication_date.get(..4)
            && year.chars().all(|ch| ch.is_ascii_digit())
        {
            out.push_str(&format!("  year = {{{year}}},\n"));
        }
        if !row.doi.is_empty() {
            out.push_str(&format!("  doi = {{{}}},\n", bib_escape(&row.doi)));
        }
        out.push_str(&format!(
            "  howpublished = {{{}}},\n  note = {{codex-med vector chunk {}; document_id={}; aggregated_chunks={}}}\n}}\n\n",
            bib_escape(&row.source_uri),
            row.chunk_id,
            row.document_id,
            row.chunk_count,
        ));
    }
    out
}

fn markdown_escape(value: &str) -> String {
    value.replace('|', "\\|").replace('\n', " ")
}

fn bib_escape(value: &str) -> String {
    value.replace('{', "\\{").replace('}', "\\}")
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
}
