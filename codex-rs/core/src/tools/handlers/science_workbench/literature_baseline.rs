//! Explicit, read-only Qdrant baseline scan into the local literature registry.

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use reqwest::Method;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use uuid::Uuid;

use super::QDRANT_COLLECTION;
use super::literature_registry::LiteratureInput;
use super::literature_registry::LiteratureRegistry;
use super::literature_registry::RegistrationOutcome;
use super::literature_registry::SourceRecord;
use super::literature_registry_identity::identifiers_from_source_uri;
use super::literature_registry_identity::normalize_doi;
use super::literature_registry_identity::paper_id_from_source_uri;

const EXPECTED_PRODUCTION_POINTS: usize = 50_760;
const EXPECTED_PRODUCTION_DOCUMENTS: usize = 316;
const EXPECTED_PRODUCTION_CANONICAL: usize = 288;
const EXPECTED_PRODUCTION_DUPLICATES: usize = 28;

#[derive(Default)]
struct BaselineDocument {
    document_id: String,
    paper_id: Option<String>,
    source_uri: Option<String>,
    title: Option<String>,
    point_count: usize,
}

pub(super) async fn initialize_qdrant_baseline(
    client: &reqwest::Client,
    registry: &LiteratureRegistry,
    workspace_root: &Path,
    qdrant_url: &str,
    collection: &str,
    api_key: &str,
    now: &str,
) -> Result<Value> {
    let source_system = format!("qdrant:{collection}");
    let production_baseline = collection == QDRANT_COLLECTION;
    if let Some(details) = sqlx::query_scalar::<_, String>(
        "SELECT details_json FROM literature_registry_initializations WHERE source_system = ?",
    )
    .bind(&source_system)
    .fetch_optional(&registry.pool)
    .await?
    {
        let details = serde_json::from_str(&details).unwrap_or_else(|_| json!({}));
        if !production_baseline
            || details
                .pointer("/validation/matches_expected_baseline")
                .and_then(Value::as_bool)
                == Some(true)
        {
            return Ok(json!({"status": "already_initialized", "details": details}));
        }
        sqlx::query("DELETE FROM literature_registry_initializations WHERE source_system = ?")
            .bind(&source_system)
            .execute(&registry.pool)
            .await?;
    }
    let point_count_before = if production_baseline {
        Some(qdrant_collection_point_count(client, qdrant_url, collection, api_key).await?)
    } else {
        None
    };

    let mut documents = BTreeMap::<String, BaselineDocument>::new();
    let mut offset: Option<Value> = None;
    let mut scanned_points = 0usize;
    let mut seen_offsets = BTreeSet::new();
    loop {
        let mut body = json!({
            "limit": 256,
            "with_payload": true,
            "with_vector": false,
        });
        if let Some(offset) = offset.as_ref()
            && let Value::Object(body) = &mut body
        {
            body.insert("offset".to_string(), offset.clone());
        }
        let response = qdrant_json(
            client,
            Method::POST,
            &format!(
                "{}/collections/{collection}/points/scroll",
                qdrant_url.trim_end_matches('/')
            ),
            api_key,
            Some(body),
        )
        .await?;
        let points = response
            .pointer("/result/points")
            .and_then(Value::as_array)
            .context("Qdrant baseline scroll response is missing points")?;
        scanned_points += points.len();
        for point in points {
            let payload = point.get("payload").unwrap_or(&Value::Null);
            let document_id = payload
                .get("document_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .unwrap_or_else(|| {
                    format!(
                        "point:{}",
                        point
                            .get("id")
                            .map(Value::to_string)
                            .unwrap_or_else(|| "unknown".to_string())
                    )
                });
            let document =
                documents
                    .entry(document_id.clone())
                    .or_insert_with(|| BaselineDocument {
                        document_id,
                        ..Default::default()
                    });
            document.point_count += 1;
            fill_if_empty(&mut document.paper_id, payload, "paper_id");
            fill_if_empty(&mut document.source_uri, payload, "source_uri");
            fill_if_empty(&mut document.title, payload, "title");
        }
        let next_offset = response.pointer("/result/next_page_offset").cloned();
        let Some(next_offset) = next_offset.filter(|value| !value.is_null()) else {
            break;
        };
        let encoded = serde_json::to_string(&next_offset)?;
        ensure!(
            seen_offsets.insert(encoded),
            "Qdrant baseline scroll repeated an offset"
        );
        offset = Some(next_offset);
    }

    let mut canonical_ids = BTreeSet::new();
    let mut created = 0usize;
    let mut reused = 0usize;
    let mut possible_duplicates = Vec::new();
    let mut conflicts = Vec::new();
    for document in documents.values() {
        let source_uri = document.source_uri.as_deref().unwrap_or_default();
        let paper_id = document.paper_id.as_deref().unwrap_or_default();
        let (uri_pmid, uri_doi) = identifiers_from_source_uri(source_uri);
        let doi = uri_doi.or_else(|| normalize_doi(paper_id));
        let normalized_paper_id = document
            .paper_id
            .clone()
            .or_else(|| paper_id_from_source_uri(source_uri));
        let registration = registry
            .register(
                LiteratureInput {
                    pmid: uri_pmid,
                    doi,
                    paper_id: normalized_paper_id,
                    title: document.title.clone(),
                    metadata: json!({
                        "raw_identifiers": {
                            "paper_id": document.paper_id,
                            "source_uri": document.source_uri,
                        },
                        "field_sources": {
                            "title": {
                                "source": source_system,
                                "observed_at": now,
                            }
                        }
                    }),
                    ..Default::default()
                },
                Some(SourceRecord {
                    system: source_system.clone(),
                    key: document.document_id.clone(),
                    match_method: "qdrant_baseline".to_string(),
                }),
                now,
            )
            .await?;
        match registration {
            RegistrationOutcome::Registered(registered) => {
                canonical_ids.insert(registered.literature_id.clone());
                if registered.created {
                    created += 1;
                } else {
                    reused += 1;
                }
                if let Some(review_case_id) = registered.review_case_id {
                    possible_duplicates.push(json!({
                        "document_id": document.document_id,
                        "review_case_id": review_case_id,
                    }));
                }
            }
            RegistrationOutcome::Conflict {
                review_case_id,
                matched_literature_ids,
            } => conflicts.push(json!({
                "document_id": document.document_id,
                "review_case_id": review_case_id,
                "matched_literature_ids": matched_literature_ids,
            })),
        }
    }

    let duplicate_source_documents = documents.len().saturating_sub(canonical_ids.len());
    let matches_expected_baseline = !production_baseline
        || (scanned_points == EXPECTED_PRODUCTION_POINTS
            && point_count_before == Some(EXPECTED_PRODUCTION_POINTS)
            && documents.len() == EXPECTED_PRODUCTION_DOCUMENTS
            && canonical_ids.len() == EXPECTED_PRODUCTION_CANONICAL
            && duplicate_source_documents == EXPECTED_PRODUCTION_DUPLICATES
            && possible_duplicates.is_empty()
            && conflicts.is_empty());
    let point_count_after = if production_baseline {
        Some(qdrant_collection_point_count(client, qdrant_url, collection, api_key).await?)
    } else {
        None
    };
    let collection_unchanged = point_count_before == point_count_after;
    let validation_passed = matches_expected_baseline && collection_unchanged;
    let details = json!({
        "source_system": source_system,
        "collection": collection,
        "scanned_points": scanned_points,
        "source_documents": documents.len(),
        "canonical_literatures": canonical_ids.len(),
        "duplicate_source_documents": duplicate_source_documents,
        "created": created,
        "reused": reused,
        "possible_duplicates": possible_duplicates,
        "conflicts": conflicts,
        "validation": {
            "production_baseline": production_baseline,
            "expected": production_baseline.then(|| json!({
                "points": EXPECTED_PRODUCTION_POINTS,
                "source_documents": EXPECTED_PRODUCTION_DOCUMENTS,
                "canonical_literatures": EXPECTED_PRODUCTION_CANONICAL,
                "duplicate_source_documents": EXPECTED_PRODUCTION_DUPLICATES,
            })),
            "point_count_before": point_count_before,
            "point_count_after": point_count_after,
            "collection_unchanged": collection_unchanged,
            "matches_expected_baseline": matches_expected_baseline,
            "passed": validation_passed,
        },
        "completed_at": now,
    });
    let report_dir = workspace_root.join(".codex-med").join("migrations");
    fs::create_dir_all(&report_dir)
        .with_context(|| format!("failed to create migration report {}", report_dir.display()))?;
    let report_path = report_dir.join(format!(
        "qdrant_baseline_{}_{}.json",
        chrono::Utc::now().format("%Y%m%dT%H%M%S%.6fZ"),
        &Uuid::new_v4().simple().to_string()[..8]
    ));
    let temporary = report_path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&details)?)
        .with_context(|| format!("failed to write migration report {}", temporary.display()))?;
    fs::rename(&temporary, &report_path).with_context(|| {
        format!(
            "failed to commit migration report {}",
            report_path.display()
        )
    })?;
    if !validation_passed {
        return Ok(json!({
            "status": "requires_human_review",
            "report": report_path
                .strip_prefix(workspace_root)
                .unwrap_or(&report_path)
                .to_string_lossy(),
            "details": details,
        }));
    }
    sqlx::query(
        r#"
        INSERT OR IGNORE INTO literature_registry_initializations(
            source_system, status, details_json, completed_at
        ) VALUES(?, 'complete', ?, ?)
        "#,
    )
    .bind(&source_system)
    .bind(serde_json::to_string(&details)?)
    .bind(now)
    .execute(&registry.pool)
    .await?;

    Ok(json!({
        "status": "initialized",
        "report": report_path
            .strip_prefix(workspace_root)
            .unwrap_or(&report_path)
            .to_string_lossy(),
        "details": details,
    }))
}

async fn qdrant_collection_point_count(
    client: &reqwest::Client,
    qdrant_url: &str,
    collection: &str,
    api_key: &str,
) -> Result<usize> {
    let response = qdrant_json(
        client,
        Method::GET,
        &format!(
            "{}/collections/{collection}",
            qdrant_url.trim_end_matches('/')
        ),
        api_key,
        None,
    )
    .await?;
    response
        .pointer("/result/points_count")
        .and_then(Value::as_u64)
        .map(|count| count as usize)
        .context("Qdrant collection metadata is missing points_count")
}

fn fill_if_empty(target: &mut Option<String>, payload: &Value, key: &str) {
    if target.is_none() {
        *target = payload
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
    }
}

async fn qdrant_json(
    client: &reqwest::Client,
    method: Method,
    url: &str,
    api_key: &str,
    body: Option<Value>,
) -> Result<Value> {
    let mut request = client.request(method, url);
    if !api_key.is_empty() {
        request = request.header("api-key", api_key);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .context("Qdrant baseline request failed")?;
    let status = response.status();
    let text = response
        .text()
        .await
        .context("failed to read Qdrant baseline response")?;
    ensure!(
        status.is_success(),
        "Qdrant baseline request returned HTTP {status}: {}",
        text.chars().take(500).collect::<String>()
    );
    serde_json::from_str(&text).context("Qdrant baseline returned invalid JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    #[tokio::test]
    async fn scan_groups_chunks_and_reuses_strong_identifier_across_documents() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/collections/baseline/points/scroll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {
                    "points": [
                        {
                            "id": "p1",
                            "payload": {
                                "document_id": "doc-1",
                                "paper_id": "EP0739981A1",
                                "title": "Example"
                            }
                        },
                        {
                            "id": "p2",
                            "payload": {
                                "document_id": "doc-1",
                                "paper_id": "EP0739981A1",
                                "title": "Example"
                            }
                        },
                        {
                            "id": "p3",
                            "payload": {
                                "document_id": "doc-2",
                                "source_uri": "file:///data/EP0739981A1/ocr_repaired.md",
                                "title": "Example duplicate import"
                            }
                        }
                    ],
                    "next_page_offset": null
                }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let temp = tempfile::tempdir().expect("temp dir");
        let registry = LiteratureRegistry::open(temp.path().join("literatures.sqlite3"))
            .await
            .expect("registry");
        let result = initialize_qdrant_baseline(
            &reqwest::Client::new(),
            &registry,
            temp.path(),
            &server.uri(),
            "baseline",
            "",
            "2026-01-01T00:00:00Z",
        )
        .await
        .expect("initialization");
        let rerun = initialize_qdrant_baseline(
            &reqwest::Client::new(),
            &registry,
            temp.path(),
            &server.uri(),
            "baseline",
            "",
            "2026-01-01T00:00:00Z",
        )
        .await
        .expect("rerun");

        assert_eq!(result["status"], "initialized");
        assert_eq!(result["details"]["scanned_points"], 3);
        assert_eq!(result["details"]["source_documents"], 2);
        assert_eq!(result["details"]["canonical_literatures"], 1);
        assert_eq!(result["details"]["duplicate_source_documents"], 1);
        assert_eq!(rerun["status"], "already_initialized");
        assert!(
            temp.path()
                .join(result["report"].as_str().expect("report path"))
                .exists()
        );
    }
}
