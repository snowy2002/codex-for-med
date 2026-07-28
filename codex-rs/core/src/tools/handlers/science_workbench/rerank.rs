use super::*;

const RERANKER_URL: &str = "http://gw-bzokqkvr2cblz8ok6y.cn-wulanchabu-acdr-1.pai-eas.aliyuncs.com/api/predict/qwen3_reranker_4b/v1/rerank";
pub(super) const RERANKER_MODEL: &str = "/model_dir/Qwen3-Reranker-4B";

// Over-recall from Qdrant, rerank the pool, then keep the requested number of
// distinct documents. Mirrors the search_vector_knowledge 50 -> 8 pipeline so
// literature_map evidence ordering stays consistent with the search tool.
pub(super) const LITERATURE_RECALL_MULTIPLIER: usize = 6;
pub(super) const LITERATURE_MAX_RECALL: usize = 100;

pub(super) struct ScoredPoint {
    pub(super) point: Value,
    pub(super) rerank_score: Option<f64>,
    pub(super) chunk_count: usize,
}

fn point_id_string(point: &Value) -> String {
    match point.get("id").unwrap_or(&Value::Null) {
        Value::String(id) => id.clone(),
        Value::Null => String::new(),
        id => id.to_string(),
    }
}

/// Identify the source document a chunk belongs to, so multiple chunks of the
/// same paper collapse to one evidence row. Prefers the most stable id first.
fn document_key(point: &Value) -> String {
    let payload = point.get("payload").unwrap_or(&Value::Null);
    for key in ["paper_id", "document_id", "source_uri", "title"] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return format!("{key}={}", trimmed.to_lowercase());
            }
        }
    }
    format!("point={}", point_id_string(point))
}

/// Text handed to the reranker. Mirrors search_vector_knowledge: prefer the full
/// chunk text, fall back through the shorter payload fields, cap the length.
fn rerank_text_for_point(point: &Value) -> String {
    let payload = point.get("payload").unwrap_or(&Value::Null);
    for key in ["text", "snippet", "content", "title"] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.chars().take(2_000).collect();
            }
        }
    }
    String::new()
}

/// Call the shared Qwen3 reranker. Returns `[(index_into_documents, score), ...]`.
async fn rerank(
    client: &reqwest::Client,
    query: &str,
    documents: &[String],
) -> Result<Vec<(usize, f64)>, FunctionCallError> {
    if documents.is_empty() {
        return Ok(Vec::new());
    }
    let reranker_url =
        env_non_empty("CODEX_MED_RERANKER_URL").unwrap_or_else(|| RERANKER_URL.to_string());
    let reranker_model =
        env_non_empty("CODEX_MED_RERANKER_MODEL").unwrap_or_else(|| RERANKER_MODEL.to_string());
    let reranker_api_key = required_env("CODEX_MED_RERANKER_API_KEY")?;
    let response = client
        .post(reranker_url)
        .bearer_auth(reranker_api_key)
        .json(&json!({
            "model": reranker_model,
            "query": query,
            "documents": documents,
        }))
        .send()
        .await
        .map_err(http_error("reranker request failed"))?;
    let value = parse_http_json(response, "reranker").await?;
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

/// Reorder the recall pool by reranker relevance. On any reranker failure we keep
/// the raw Qdrant order so the workflow still produces an evidence map.
pub(super) async fn rerank_points(
    client: &reqwest::Client,
    query: &str,
    points: Vec<Value>,
) -> (Vec<(Value, Option<f64>)>, bool, String) {
    if points.is_empty() {
        return (Vec::new(), false, "no recall hits to rerank".to_string());
    }
    let documents: Vec<String> = points.iter().map(rerank_text_for_point).collect();
    match rerank(client, query, &documents).await {
        Ok(ranks) => {
            let mut scored: Vec<(Value, Option<f64>)> = ranks
                .into_iter()
                .filter_map(|(idx, score)| {
                    points.get(idx).map(|point| (point.clone(), Some(score)))
                })
                .collect();
            scored.sort_by(|a, b| {
                b.1.unwrap_or(f64::MIN)
                    .partial_cmp(&a.1.unwrap_or(f64::MIN))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            // Keep any recall chunk the reranker did not score, in raw order, at
            // the tail so nothing silently disappears.
            if scored.len() < points.len() {
                let present: std::collections::HashSet<String> = scored
                    .iter()
                    .map(|(point, _)| point_id_string(point))
                    .collect();
                for point in points {
                    if !present.contains(&point_id_string(&point)) {
                        scored.push((point, None));
                    }
                }
            }
            let note = format!(
                "reranked {} recall chunks with {}",
                documents.len(),
                RERANKER_MODEL
            );
            (scored, true, note)
        }
        Err(err) => {
            let scored = points.into_iter().map(|point| (point, None)).collect();
            (
                scored,
                false,
                format!("reranker failed, using Qdrant vector order: {err}"),
            )
        }
    }
}

/// Collapse chunks that share a document key, keeping the best-ranked chunk as the
/// representative and recording how many chunks backed it. Returns the deduped
/// documents (capped at `top_k`) and the total chunk count considered.
pub(super) fn dedupe_points(
    ordered: Vec<(Value, Option<f64>)>,
    top_k: usize,
) -> (Vec<ScoredPoint>, usize) {
    let total_chunks = ordered.len();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (point, _) in &ordered {
        *counts.entry(document_key(point)).or_insert(0) += 1;
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<ScoredPoint> = Vec::new();
    for (point, rerank_score) in ordered {
        let key = document_key(&point);
        if !seen.insert(key.clone()) {
            continue;
        }
        let chunk_count = counts.get(&key).copied().unwrap_or(1);
        out.push(ScoredPoint {
            point,
            rerank_score,
            chunk_count,
        });
        if out.len() >= top_k {
            break;
        }
    }
    (out, total_chunks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    #[test]
    fn document_key_prefers_stable_ids() {
        assert_eq!(
            document_key(
                &json!({"id": "c1", "payload": {"paper_id": "EP123", "source_uri": "file:///x"}})
            ),
            "paper_id=ep123"
        );
        assert_eq!(
            document_key(
                &json!({"id": "c2", "payload": {"source_uri": "file:///Y", "title": "T"}})
            ),
            "source_uri=file:///y"
        );
        assert_eq!(
            document_key(&json!({"id": "c3", "payload": {}})),
            "point=c3"
        );
    }
    #[test]
    fn dedupe_keeps_best_chunk_and_counts() {
        let ordered = vec![
            (
                json!({"id": "c1", "score": 0.9, "payload": {"paper_id": "P1"}}),
                Some(0.8),
            ),
            (
                json!({"id": "c2", "score": 0.7, "payload": {"paper_id": "P1"}}),
                Some(0.6),
            ),
            (
                json!({"id": "c3", "score": 0.6, "payload": {"paper_id": "P2"}}),
                Some(0.5),
            ),
        ];
        let (deduped, total) = dedupe_points(ordered, 10);
        assert_eq!(total, 3);
        assert_eq!(deduped.len(), 2);
        assert_eq!(point_id_string(&deduped[0].point), "c1");
        assert_eq!(deduped[0].chunk_count, 2);
        assert_eq!(deduped[1].chunk_count, 1);
    }
    #[test]
    fn dedupe_respects_top_k() {
        let ordered = vec![
            (json!({"id": "c1", "payload": {"paper_id": "P1"}}), None),
            (json!({"id": "c2", "payload": {"paper_id": "P2"}}), None),
            (json!({"id": "c3", "payload": {"paper_id": "P3"}}), None),
        ];
        let (deduped, _) = dedupe_points(ordered, 2);
        assert_eq!(deduped.len(), 2);
    }
}
