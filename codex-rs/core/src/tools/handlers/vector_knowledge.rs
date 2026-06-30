use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::parse_arguments;
use crate::tools::handlers::vector_knowledge_spec::SEARCH_VECTOR_KNOWLEDGE_TOOL_NAME;
use crate::tools::handlers::vector_knowledge_spec::create_search_vector_knowledge_tool;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use reqwest::header::AUTHORIZATION;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use serde::Deserialize;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::Duration;
use url::Url;

const DEFAULT_QDRANT_URL: &str = "http://127.0.0.1:6333";
const DEFAULT_COLLECTION: &str = "medical_knowledge";
const DEFAULT_TOP_K: usize = 8;
const MAX_TOP_K: usize = 30;
const HTTP_TIMEOUT_SECONDS: u64 = 60;

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
        .or_else(|| env_non_empty("CODEX_MED_EMBEDDING_MODEL"));
    let top_k = args.top_k.clamp(1, MAX_TOP_K);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECONDS))
        .build()
        .map_err(|err| FunctionCallError::Fatal(format!("failed to build HTTP client: {err}")))?;

    let embedding =
        embed_query(&client, &embedding_url, &query, embedding_model.as_deref()).await?;
    let filter = build_qdrant_filter(&args.categories, &args.filters)?;
    let search_response = qdrant_search(
        &client,
        &qdrant_url,
        &collection,
        &embedding,
        filter,
        top_k,
        args.include_payload,
    )
    .await?;

    let output = json!({
        "query": query,
        "backend": {
            "provider": "qdrant",
            "url": qdrant_url.as_str(),
            "collection": collection,
            "embedding_url": embedding_url.as_str(),
            "embedding_model": embedding_model,
        },
        "top_k": top_k,
        "returned_results": search_response.result.len(),
        "results": search_response.result.into_iter().enumerate().map(|(idx, point)| {
            point_to_result(idx + 1, point)
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
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "embedding_url is required; pass it in the tool call or set CODEX_MED_EMBEDDING_URL"
                    .to_string(),
            )
        })?;
    parse_http_url(&embedding_url, "embedding_url")
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
    if let Some(token) =
        env_non_empty("CODEX_MED_EMBEDDING_API_KEY").or_else(|| env_non_empty("EMBEDDING_API_KEY"))
    {
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
    if let Some(api_key) =
        env_non_empty("CODEX_MED_VECTOR_QDRANT_API_KEY").or_else(|| env_non_empty("QDRANT_API_KEY"))
    {
        let value = HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|err| {
            FunctionCallError::RespondToModel(format!("invalid Qdrant API key header: {err}"))
        })?;
        headers.insert(AUTHORIZATION, value);
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

#[derive(Debug, Deserialize)]
struct QdrantPoint {
    id: Value,
    score: f64,
    #[serde(default)]
    payload: Option<Value>,
}

fn point_to_result(rank: usize, point: QdrantPoint) -> Value {
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
            .and(path("/collections/medical_knowledge/points/search"))
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
        })
        .await
        .expect("search succeeds");

        let value: Value = serde_json::from_str(&output).expect("json output");
        assert_eq!(value["returned_results"], 1);
        assert_eq!(value["results"][0]["document_id"], "paper-1");
        assert_eq!(value["results"][0]["category"], "bio_literature");
    }
}
