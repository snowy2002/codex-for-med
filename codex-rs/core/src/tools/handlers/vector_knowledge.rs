use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::qdrant_config::DEFAULT_QDRANT_COLLECTION;
use crate::tools::handlers::qdrant_config::DEFAULT_QDRANT_URL;
use crate::tools::handlers::vector_knowledge_spec::SEARCH_VECTOR_KNOWLEDGE_TOOL_NAME;
use crate::tools::handlers::vector_knowledge_spec::create_search_vector_knowledge_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderName;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;
use url::Url;

const DEFAULT_COLLECTION: &str = DEFAULT_QDRANT_COLLECTION;
const DEFAULT_TOP_K: usize = 8;
const MAX_TOP_K: usize = 30;
const HTTP_TIMEOUT_SECONDS: u64 = 60;
const QDRANT_API_KEY_HEADER: HeaderName = HeaderName::from_static("api-key");

const DEFAULT_EMBEDDING_URL: &str = "http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_embedding_4b/v1/embeddings";
const DEFAULT_EMBEDDING_MODEL: &str = "/model_dir/Qwen3-Embedding-4B";

const DEFAULT_RERANKER_URL: &str = "http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_reranker_4b/v1/rerank";
const DEFAULT_RERANKER_MODEL: &str = "/model_dir/Qwen3-Reranker-4B";

// Recall / rerank tuning. We over-recall from Qdrant, ask the reranker to
// re-score, then return the requested top_k. Match the docs' recommended
// 50 -> 8 pipeline.
const DEFAULT_RECALL_MULTIPLIER: usize = 6;
const MAX_RECALL_TOP_K: usize = 100;

#[derive(Default)]
pub struct VectorKnowledgeHandler;

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for VectorKnowledgeHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(SEARCH_VECTOR_KNOWLEDGE_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        create_search_vector_knowledge_tool()
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
                    "search_vector_knowledge received unsupported payload".to_string(),
                ));
            }
        };
        let args: SearchVectorKnowledgeArgs = parse_arguments(&arguments)?;
        let output = search_vector_knowledge(args).await?;

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            output,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for VectorKnowledgeHandler {}

#[derive(Debug, Deserialize)]
struct SearchVectorKnowledgeArgs {
    query: String,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    filters: BTreeMap<String, Value>,
    #[serde(default = "default_top_k")]
    top_k: usize,
    #[serde(default)]
    collection: Option<String>,
    #[serde(default)]
    qdrant_url: Option<String>,
    #[serde(default)]
    embedding_url: Option<String>,
    #[serde(default)]
    embedding_model: Option<String>,
    #[serde(default = "default_include_payload")]
    include_payload: bool,
    /// Disable the Qwen reranker second-stage sort. Off by default; the tool
    /// runs Qdrant recall -> reranker -> final top_k. When true, results come
    /// back in raw Qdrant order.
    #[serde(default)]
    disable_reranker: bool,
}

fn default_top_k() -> usize {
    DEFAULT_TOP_K
}

fn default_include_payload() -> bool {
    true
}

async fn search_vector_knowledge(
    args: SearchVectorKnowledgeArgs,
) -> Result<String, FunctionCallError> {
    let query = validate_query(&args.query)?;
    let collection = resolve_collection(args.collection.as_deref())?;
    let qdrant_url = resolve_qdrant_url(args.qdrant_url.as_deref())?;
    let embedding_url = resolve_embedding_url(args.embedding_url.as_deref())?;
    let embedding_model = args
        .embedding_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| env_non_empty("CODEX_MED_EMBEDDING_MODEL"))
        .unwrap_or_else(|| DEFAULT_EMBEDDING_MODEL.to_string());
    let top_k = args.top_k.clamp(1, MAX_TOP_K);
    // Recall a wider pool so the reranker has enough material to reorder.
    let recall_k = (top_k * DEFAULT_RECALL_MULTIPLIER)
        .min(MAX_RECALL_TOP_K)
        .max(top_k);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS))
        .build()
        .map_err(|err| FunctionCallError::Fatal(format!("failed to build HTTP client: {err}")))?;

    let embedding = embed_query(&client, &embedding_url, &query, Some(&embedding_model)).await?;
    let filter = build_qdrant_filter(&args.categories, &args.filters)?;
    let search_response = qdrant_search(
        &client,
        &qdrant_url,
        &collection,
        &embedding,
        filter,
        recall_k,
        args.include_payload,
    )
    .await?;

    // Reranker stage — only skip when the caller explicitly asks or when we
    // ended up with zero recall hits.
    let mut rerank_meta = json!({
        "used": false,
        "reason": "disabled by caller",
        "recall_top_k": recall_k,
        "final_top_k": top_k,
    });
    let mut ordered_points: Vec<QdrantPoint> = search_response.result;
    let mut rerank_scores: std::collections::HashMap<String, f64> =
        std::collections::HashMap::new();

    if !args.disable_reranker && !ordered_points.is_empty() {
        let reranker_url = resolve_reranker_url()?;
        let reranker_model = env_non_empty("CODEX_MED_RERANKER_MODEL")
            .unwrap_or_else(|| DEFAULT_RERANKER_MODEL.to_string());
        let documents: Vec<String> = ordered_points
            .iter()
            .map(|point| extract_rerank_text(point))
            .collect();
        match rerank(&client, &reranker_url, &reranker_model, &query, &documents).await {
            Ok(ranks) => {
                // ranks is [(index_in_ordered_points, relevance_score), ...]
                // Reorder the recall list by rerank score (desc), keep top_k.
                let mut zipped: Vec<(QdrantPoint, f64)> = ranks
                    .into_iter()
                    .filter_map(|(idx, score)| {
                        ordered_points.get(idx).map(|point| (point.clone(), score))
                    })
                    .collect();
                zipped.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                zipped.truncate(top_k);
                for (point, score) in &zipped {
                    rerank_scores.insert(point_id_key(&point.id), *score);
                }
                ordered_points = zipped.into_iter().map(|(p, _)| p).collect();
                rerank_meta = json!({
                    "used": true,
                    "reranker_url": reranker_url.as_str(),
                    "reranker_model": reranker_model,
                    "recall_top_k": recall_k,
                    "final_top_k": top_k,
                });
            }
            Err(err) => {
                // Reranker failure should not sink the whole tool call — fall
                // back to Qdrant order but tell the model we tried.
                ordered_points.truncate(top_k);
                rerank_meta = json!({
                    "used": false,
                    "reason": format!("reranker failed, falling back to Qdrant order: {err}"),
                    "recall_top_k": recall_k,
                    "final_top_k": top_k,
                });
            }
        }
    } else {
        ordered_points.truncate(top_k);
    }

    let output = json!({
        "query": query,
        "backend": {
            "provider": "qdrant",
            "url": qdrant_url.as_str(),
            "collection": collection,
            "embedding_url": embedding_url.as_str(),
            "embedding_model": embedding_model,
        },
        "rerank": rerank_meta,
        "top_k": top_k,
        "returned_results": ordered_points.len(),
        "results": ordered_points.into_iter().enumerate().map(|(idx, point)| {
            let rerank_score = rerank_scores.get(&point_id_key(&point.id)).copied();
            point_to_result(idx + 1, point, rerank_score)
        }).collect::<Vec<_>>(),
    });

    serde_json::to_string_pretty(&output).map_err(|err| {
        FunctionCallError::Fatal(format!(
            "failed to serialize vector knowledge results: {err}"
        ))
    })
}

fn validate_query(query: &str) -> Result<String, FunctionCallError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "query must not be empty".to_string(),
        ));
    }
    if query.chars().count() > 4_000 {
        return Err(FunctionCallError::RespondToModel(
            "query is too long; keep vector knowledge searches under 4000 characters".to_string(),
        ));
    }
    Ok(query.to_string())
}

fn resolve_collection(collection: Option<&str>) -> Result<String, FunctionCallError> {
    let collection = collection
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| env_non_empty("CODEX_MED_VECTOR_COLLECTION"))
        .unwrap_or_else(|| DEFAULT_COLLECTION.to_string());
    if !collection
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(FunctionCallError::RespondToModel(
            "collection may only contain ASCII letters, digits, underscore, hyphen, or dot"
                .to_string(),
        ));
    }
    Ok(collection)
}

fn resolve_qdrant_url(qdrant_url: Option<&str>) -> Result<Url, FunctionCallError> {
    let qdrant_url = qdrant_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| env_non_empty("CODEX_MED_VECTOR_QDRANT_URL"))
        .unwrap_or_else(|| DEFAULT_QDRANT_URL.to_string());
    parse_http_url(&qdrant_url, "qdrant_url")
}

fn resolve_embedding_url(embedding_url: Option<&str>) -> Result<Url, FunctionCallError> {
    let embedding_url = embedding_url
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| env_non_empty("CODEX_MED_EMBEDDING_URL"))
        .unwrap_or_else(|| DEFAULT_EMBEDDING_URL.to_string());
    parse_http_url(&embedding_url, "embedding_url")
}

fn resolve_reranker_url() -> Result<Url, FunctionCallError> {
    let reranker_url =
        env_non_empty("CODEX_MED_RERANKER_URL").unwrap_or_else(|| DEFAULT_RERANKER_URL.to_string());
    parse_http_url(&reranker_url, "reranker_url")
}

fn parse_http_url(value: &str, field_name: &str) -> Result<Url, FunctionCallError> {
    let url = Url::parse(value).map_err(|err| {
        FunctionCallError::RespondToModel(format!("{field_name} is not a valid URL: {err}"))
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(FunctionCallError::RespondToModel(format!(
            "{field_name} must use http or https"
        )));
    }
    Ok(url)
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn embed_query(
    client: &reqwest::Client,
    embedding_url: &Url,
    query: &str,
    embedding_model: Option<&str>,
) -> Result<Vec<f64>, FunctionCallError> {
    let mut body = json!({ "input": query });
    if let Some(model) = embedding_model {
        body["model"] = Value::String(model.to_string());
    }
    let mut request = client.post(embedding_url.clone()).json(&body);
    let token =
        env_non_empty("CODEX_MED_EMBEDDING_API_KEY").or_else(|| env_non_empty("EMBEDDING_API_KEY"));
    if token.is_none() && embedding_url.as_str() == DEFAULT_EMBEDDING_URL {
        return Err(FunctionCallError::RespondToModel(
            "CODEX_MED_EMBEDDING_API_KEY must be set for the default embedding service".to_string(),
        ));
    }
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("embedding request failed: {err}"))
    })?;
    let status = response.status();
    let body = response.text().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read embedding response: {err}"))
    })?;
    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "embedding request returned HTTP {status}: {}",
            truncate_for_error(&body)
        )));
    }
    let value: Value = serde_json::from_str(&body).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse embedding response JSON: {err}"))
    })?;
    parse_embedding_response(&value)
}

fn parse_embedding_response(value: &Value) -> Result<Vec<f64>, FunctionCallError> {
    if let Some(embedding) = value.get("embedding") {
        return parse_embedding_array(embedding);
    }
    if let Some(embedding) = value
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("embedding"))
    {
        return parse_embedding_array(embedding);
    }
    if value.is_array() {
        return parse_embedding_array(value);
    }
    Err(FunctionCallError::RespondToModel(
        "embedding response must contain `embedding`, OpenAI-compatible `data[0].embedding`, or a raw array"
            .to_string(),
    ))
}

fn parse_embedding_array(value: &Value) -> Result<Vec<f64>, FunctionCallError> {
    let values = value.as_array().ok_or_else(|| {
        FunctionCallError::RespondToModel("embedding must be an array".to_string())
    })?;
    if values.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "embedding must not be empty".to_string(),
        ));
    }
    values
        .iter()
        .map(|value| {
            value.as_f64().ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "embedding values must be JSON numbers".to_string(),
                )
            })
        })
        .collect()
}

fn build_qdrant_filter(
    categories: &[String],
    filters: &BTreeMap<String, Value>,
) -> Result<Option<Value>, FunctionCallError> {
    let mut must = Vec::new();
    let mut must_not = vec![json!({
        "key": "is_deleted",
        "match": { "value": true }
    })];

    let categories = categories
        .iter()
        .map(|category| category.trim())
        .filter(|category| !category.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    if !categories.is_empty() {
        must.push(json!({
            "key": "category",
            "match": { "any": categories }
        }));
    }

    for (key, value) in filters {
        let key = validate_payload_key(key)?;
        if key == "is_deleted" && value == &Value::Bool(true) {
            must_not.clear();
            must.push(json!({
                "key": "is_deleted",
                "match": { "value": true }
            }));
            continue;
        }
        must.push(filter_condition(&key, value)?);
    }

    if must.is_empty() && must_not.is_empty() {
        return Ok(None);
    }

    let mut object = Map::new();
    if !must.is_empty() {
        object.insert("must".to_string(), Value::Array(must));
    }
    if !must_not.is_empty() {
        object.insert("must_not".to_string(), Value::Array(must_not));
    }
    Ok(Some(Value::Object(object)))
}

fn validate_payload_key(key: &str) -> Result<String, FunctionCallError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "filter keys must not be empty".to_string(),
        ));
    }
    if !key
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "filter key `{key}` may only contain ASCII letters, digits, underscore, hyphen, or dot"
        )));
    }
    Ok(key.to_string())
}

fn filter_condition(key: &str, value: &Value) -> Result<Value, FunctionCallError> {
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) => Ok(json!({
            "key": key,
            "match": { "value": value }
        })),
        Value::Array(values) => {
            if values.is_empty() {
                return Err(FunctionCallError::RespondToModel(format!(
                    "filter `{key}` array must not be empty"
                )));
            }
            for item in values {
                if !matches!(item, Value::String(_) | Value::Number(_) | Value::Bool(_)) {
                    return Err(FunctionCallError::RespondToModel(format!(
                        "filter `{key}` array values must be strings, numbers, or booleans"
                    )));
                }
            }
            Ok(json!({
                "key": key,
                "match": { "any": values }
            }))
        }
        _ => Err(FunctionCallError::RespondToModel(format!(
            "filter `{key}` must be a string, number, boolean, or array of scalar values"
        ))),
    }
}

async fn qdrant_search(
    client: &reqwest::Client,
    qdrant_url: &Url,
    collection: &str,
    embedding: &[f64],
    filter: Option<Value>,
    top_k: usize,
    include_payload: bool,
) -> Result<QdrantSearchResponse, FunctionCallError> {
    let url = qdrant_points_search_url(qdrant_url, collection)?;
    let mut body = json!({
        "vector": embedding,
        "limit": top_k,
        "with_payload": include_payload,
        "with_vector": false,
    });
    if let Some(filter) = filter {
        body["filter"] = filter;
    }

    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let api_key = env_non_empty("CODEX_MED_VECTOR_QDRANT_API_KEY")
        .or_else(|| env_non_empty("QDRANT_API_KEY"))
        .unwrap_or_default();
    if !api_key.is_empty() {
        let value = HeaderValue::from_str(&api_key).map_err(|err| {
            FunctionCallError::RespondToModel(format!("invalid Qdrant API key header: {err}"))
        })?;
        headers.insert(QDRANT_API_KEY_HEADER, value);
    }

    let response = client
        .post(url)
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|err| FunctionCallError::RespondToModel(format!("Qdrant search failed: {err}")))?;
    let status = response.status();
    let body = response.text().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read Qdrant response: {err}"))
    })?;
    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "Qdrant search returned HTTP {status}: {}",
            truncate_for_error(&body)
        )));
    }
    serde_json::from_str::<QdrantSearchResponse>(&body).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse Qdrant search response: {err}"))
    })
}

fn qdrant_points_search_url(base_url: &Url, collection: &str) -> Result<Url, FunctionCallError> {
    let mut url = base_url.clone();
    {
        let mut segments = url.path_segments_mut().map_err(|_| {
            FunctionCallError::RespondToModel("qdrant_url cannot be a base URL".to_string())
        })?;
        segments.pop_if_empty();
        segments.extend(["collections", collection, "points", "search"]);
    }
    Ok(url)
}

#[derive(Debug, Deserialize)]
struct QdrantSearchResponse {
    result: Vec<QdrantPoint>,
}

#[derive(Debug, Deserialize, Clone)]
struct QdrantPoint {
    id: Value,
    score: f64,
    #[serde(default)]
    payload: Option<Value>,
}

fn point_id_key(id: &Value) -> String {
    match id {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn extract_rerank_text(point: &QdrantPoint) -> String {
    let payload = match &point.payload {
        Some(payload) => payload,
        None => return String::new(),
    };
    // Prefer full `text`; fall back through the payload fields the tool
    // normally surfaces in its output.
    for key in ["text", "snippet", "content", "title"] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                // Reranker input is best kept under a few thousand chars; the
                // model still copes with more, but the payload cost adds up.
                if trimmed.chars().count() > 2_000 {
                    return trimmed.chars().take(2_000).collect();
                }
                return trimmed.to_string();
            }
        }
    }
    String::new()
}

async fn rerank(
    client: &reqwest::Client,
    reranker_url: &Url,
    model: &str,
    query: &str,
    documents: &[String],
) -> Result<Vec<(usize, f64)>, FunctionCallError> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let body = json!({
        "model": model,
        "query": query,
        "documents": documents,
    });
    let mut request = client.post(reranker_url.clone()).json(&body);
    let token = env_non_empty("CODEX_MED_RERANKER_API_KEY");
    if token.is_none() && reranker_url.as_str() == DEFAULT_RERANKER_URL {
        return Err(FunctionCallError::RespondToModel(
            "CODEX_MED_RERANKER_API_KEY must be set for the default reranker service".to_string(),
        ));
    }
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("reranker request failed: {err}"))
    })?;
    let status = response.status();
    let body = response.text().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read reranker response: {err}"))
    })?;
    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "reranker request returned HTTP {status}: {}",
            truncate_for_error(&body)
        )));
    }
    let value: Value = serde_json::from_str(&body).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse reranker response JSON: {err}"))
    })?;
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "reranker response missing `results` array".to_string(),
            )
        })?;
    let mut out = Vec::with_capacity(results.len());
    for item in results {
        let idx = item.get("index").and_then(Value::as_u64).ok_or_else(|| {
            FunctionCallError::RespondToModel("reranker result missing integer `index`".to_string())
        })?;
        let score = item
            .get("relevance_score")
            .and_then(Value::as_f64)
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(
                    "reranker result missing numeric `relevance_score`".to_string(),
                )
            })?;
        out.push((idx as usize, score));
    }
    Ok(out)
}

fn point_to_result(rank: usize, point: QdrantPoint, rerank_score: Option<f64>) -> Value {
    let payload = point.payload.unwrap_or(Value::Null);
    let document_id = payload
        .get("document_id")
        .cloned()
        .unwrap_or_else(|| point.id.clone());
    let chunk_id = payload
        .get("chunk_id")
        .cloned()
        .unwrap_or_else(|| point.id.clone());
    let category = payload.get("category").cloned().unwrap_or(Value::Null);
    let title = payload.get("title").cloned().unwrap_or(Value::Null);
    let snippet = payload
        .get("snippet")
        .or_else(|| payload.get("text"))
        .or_else(|| payload.get("content"))
        .cloned()
        .unwrap_or(Value::Null);
    let source = payload
        .get("source")
        .or_else(|| payload.get("source_uri"))
        .or_else(|| payload.get("url"))
        .cloned()
        .unwrap_or(Value::Null);
    let citation = payload
        .get("citation")
        .or_else(|| payload.get("doi"))
        .or_else(|| payload.get("pmid"))
        .or_else(|| payload.get("experiment_id"))
        .cloned()
        .unwrap_or(Value::Null);

    json!({
        "rank": rank,
        "score": point.score,
        "rerank_score": rerank_score,
        "point_id": point.id,
        "document_id": document_id,
        "chunk_id": chunk_id,
        "category": category,
        "title": title,
        "snippet": snippet,
        "source": source,
        "citation": citation,
        "payload": payload,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    #[test]
    fn parses_supported_embedding_shapes() {
        assert_eq!(
            parse_embedding_response(&json!({"embedding": [0.1, 0.2]})).expect("simple"),
            vec![0.1, 0.2]
        );
        assert_eq!(
            parse_embedding_response(&json!({"data": [{"embedding": [0.3, 0.4]}]}))
                .expect("openai"),
            vec![0.3, 0.4]
        );
        assert_eq!(
            parse_embedding_response(&json!([0.5, 0.6])).expect("array"),
            vec![0.5, 0.6]
        );
    }

    #[test]
    fn builds_category_and_payload_filter() {
        let filter = build_qdrant_filter(
            &[
                "bio_literature".to_string(),
                "experiment_record".to_string(),
            ],
            &BTreeMap::from([
                ("project_id".to_string(), json!("dlp-affinity")),
                ("tags".to_string(), json!(["antibody", "affinity"])),
            ]),
        )
        .expect("filter")
        .expect("some filter");

        assert_eq!(
            filter,
            json!({
                "must": [
                    {
                        "key": "category",
                        "match": {"any": ["bio_literature", "experiment_record"]}
                    },
                    {
                        "key": "project_id",
                        "match": {"value": "dlp-affinity"}
                    },
                    {
                        "key": "tags",
                        "match": {"any": ["antibody", "affinity"]}
                    }
                ],
                "must_not": [
                    {
                        "key": "is_deleted",
                        "match": {"value": true}
                    }
                ]
            })
        );
    }

    #[test]
    fn rejects_invalid_filter_shape() {
        let err = build_qdrant_filter(
            &[],
            &BTreeMap::from([("bad key".to_string(), json!("value"))]),
        )
        .expect_err("invalid key");
        assert!(
            err.to_string()
                .contains("may only contain ASCII letters, digits")
        );
    }

    #[test]
    fn builds_qdrant_search_url() {
        let url = qdrant_points_search_url(
            &Url::parse("http://127.0.0.1:6333").expect("url"),
            "medical_knowledge",
        )
        .expect("search url");
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:6333/collections/medical_knowledge/points/search"
        );
    }

    #[tokio::test]
    async fn searches_qdrant_with_embedding_endpoint() {
        let embedding_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "embedding": [0.1, 0.2, 0.3]
            })))
            .mount(&embedding_server)
            .await;

        let qdrant_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(
                "/collections/medical_knowledge_qwen3_4b/points/search",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": [
                    {
                        "id": "chunk-1",
                        "score": 0.91,
                        "payload": {
                            "document_id": "paper-1",
                            "chunk_id": "paper-1:0001",
                            "category": "bio_literature",
                            "title": "Affinity paper",
                            "snippet": "Antibody affinity evidence",
                            "source_uri": "file:///paper-1.md",
                            "pmid": "12345"
                        }
                    }
                ],
                "status": "ok"
            })))
            .mount(&qdrant_server)
            .await;

        let output = search_vector_knowledge(SearchVectorKnowledgeArgs {
            query: "antibody affinity".to_string(),
            categories: vec!["bio_literature".to_string()],
            filters: BTreeMap::new(),
            top_k: 5,
            collection: None,
            qdrant_url: Some(qdrant_server.uri()),
            embedding_url: Some(format!("{}/embed", embedding_server.uri())),
            embedding_model: None,
            include_payload: true,
            disable_reranker: true,
        })
        .await
        .expect("search succeeds");

        let value: Value = serde_json::from_str(&output).expect("json output");
        assert_eq!(value["returned_results"], 1);
        assert_eq!(value["results"][0]["document_id"], "paper-1");
        assert_eq!(value["results"][0]["category"], "bio_literature");
    }
}
