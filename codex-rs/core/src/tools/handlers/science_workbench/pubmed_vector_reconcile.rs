//! Batch reconciliation for PubMed literature vectors.

use crate::function_tool::FunctionCallError;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::Path;

use super::literature_artifacts::new_run_id;
use super::literature_registry::Literature;
use super::literature_registry::LiteratureRegistry;
use super::pretty_json;
use super::pubmed_vector_ingest::PubmedVectorConfig;
use super::pubmed_vector_ingest::PubmedVectorIngestor;
use super::relative_display;

#[derive(Debug, Deserialize)]
pub(super) struct ReconcilePubmedVectorsArgs {
    #[serde(default)]
    literature_ids: Option<Vec<String>>,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    100
}

pub(super) async fn reconcile_pubmed_vectors(
    client: &reqwest::Client,
    args: ReconcilePubmedVectorsArgs,
    cwd: &Path,
) -> Result<String, FunctionCallError> {
    let started_at = chrono::Utc::now();
    let run_id = new_run_id(&started_at, "reconcile_pubmed_vectors");
    let registry = LiteratureRegistry::open_workspace(cwd)
        .await
        .map_err(|error| {
            FunctionCallError::Fatal(format!("failed to open literature registry: {error:#}"))
        })?;
    let config = PubmedVectorConfig::from_environment().map_err(|error| {
        FunctionCallError::RespondToModel(format!("invalid PubMed vector configuration: {error:#}"))
    })?;
    let collection = config.collection_name().to_string();
    let writes_enabled = config.write_enabled();
    let registry_backup = if writes_enabled {
        let backup = cwd
            .join(".codex-med")
            .join("backups")
            .join(format!("literatures_before_{run_id}.sqlite3"));
        registry.backup_to(&backup).await.map_err(|error| {
            FunctionCallError::Fatal(format!(
                "refusing vector reconciliation because the registry backup failed: {error:#}"
            ))
        })?;
        Some(relative_display(cwd, &backup))
    } else {
        None
    };
    let literatures =
        select_pubmed_literatures(&registry, args.literature_ids.as_deref(), args.limit).await?;
    let ingestor = PubmedVectorIngestor::new(client, config);
    let mut statuses = BTreeMap::<String, usize>::new();
    let mut results = Vec::with_capacity(literatures.len());

    for literature in literatures {
        let review_case_id =
            pending_vector_review_case(&registry, &literature.literature_id, &collection)
                .await
                .map_err(|error| {
                    FunctionCallError::Fatal(format!(
                        "failed to read vector review state for {}: {error:#}",
                        literature.literature_id
                    ))
                })?;
        let result = ingestor
            .ingest(
                &registry,
                &literature,
                review_case_id.as_deref(),
                chrono::Utc::now(),
            )
            .await;
        *statuses.entry(result.status.clone()).or_default() += 1;
        results.push(json!({
            "literature_id": literature.literature_id,
            "pmid": literature.pmid,
            "status": result.status,
            "expected_points": result.expected_points,
            "verified_points": result.verified_points,
            "existing_dataset": result.existing_dataset,
            "existing_document_id": result.existing_document_id,
            "verification_method": result.verification_method,
            "review_case_id": result.review_case_id.or(review_case_id),
            "error": result.error,
        }));
    }

    let incomplete = results
        .iter()
        .filter(|result| {
            !matches!(
                result.get("status").and_then(serde_json::Value::as_str),
                Some("complete" | "already_vectorized")
            )
        })
        .count();
    pretty_json(json!({
        "run_id": run_id,
        "started_at": started_at.to_rfc3339(),
        "finished_at": chrono::Utc::now().to_rfc3339(),
        "collection": collection,
        "writes_enabled": writes_enabled,
        "registry": relative_display(cwd, registry.path()),
        "registry_backup": registry_backup,
        "selected": results.len(),
        "complete": incomplete == 0,
        "complete_count": results.len() - incomplete,
        "incomplete_count": incomplete,
        "statuses": statuses,
        "results": results,
    }))
}

async fn select_pubmed_literatures(
    registry: &LiteratureRegistry,
    requested_ids: Option<&[String]>,
    limit: usize,
) -> Result<Vec<Literature>, FunctionCallError> {
    let limit = limit.clamp(1, 500);
    let ids = if let Some(requested_ids) = requested_ids {
        requested_ids
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .take(limit)
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    } else {
        sqlx::query_scalar::<_, String>(
            "SELECT literature_id FROM literatures WHERE pmid IS NOT NULL ORDER BY updated_at, literature_id LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&registry.pool)
        .await
        .map_err(|error| {
            FunctionCallError::Fatal(format!(
                "failed to select PubMed literature for reconciliation: {error:#}"
            ))
        })?
    };

    let mut seen = HashSet::new();
    let mut literatures = Vec::with_capacity(ids.len());
    for id in ids {
        let literature = registry
            .get(&id)
            .await
            .map_err(|error| {
                FunctionCallError::Fatal(format!(
                    "failed to read literature {id} for reconciliation: {error:#}"
                ))
            })?
            .ok_or_else(|| {
                FunctionCallError::RespondToModel(format!(
                    "unknown literature_id requested for reconciliation: {id}"
                ))
            })?;
        if literature.pmid.is_none() {
            return Err(FunctionCallError::RespondToModel(format!(
                "literature_id {} is not a PubMed record",
                literature.literature_id
            )));
        }
        if seen.insert(literature.literature_id.clone()) {
            literatures.push(literature);
        }
    }
    Ok(literatures)
}

async fn pending_vector_review_case(
    registry: &LiteratureRegistry,
    literature_id: &str,
    collection: &str,
) -> anyhow::Result<Option<String>> {
    let row = sqlx::query(
        r#"
        SELECT review_case_id
        FROM literature_vector_jobs
        WHERE literature_id = ?
          AND collection_name = ?
          AND status IN ('possible_duplicate', 'blocked_conflict')
          AND review_case_id IS NOT NULL
        "#,
    )
    .bind(literature_id)
    .bind(collection)
    .fetch_optional(&registry.pool)
    .await?;
    Ok(row.map(|row| row.try_get("review_case_id")).transpose()?)
}

#[cfg(test)]
mod tests {
    use super::super::literature_registry::LiteratureInput;
    use super::super::literature_registry::RegistrationOutcome;
    use super::super::pubmed_chunks::build_pubmed_chunks;
    use super::super::qdrant_count;
    use super::*;
    use crate::tools::handlers::qdrant_config::QdrantRuntimeConfig;
    use serde_json::json;

    #[tokio::test]
    #[ignore = "requires live embedding and a disposable Qdrant collection"]
    async fn real_pubmed_vector_reconciliation_sandbox() {
        let collection =
            std::env::var("CODEX_MED_VECTOR_COLLECTION").expect("sandbox collection must be set");
        assert!(
            collection.starts_with("codex_med_it_"),
            "refusing to run against a non-sandbox collection"
        );
        assert_eq!(
            std::env::var("CODEX_MED_PUBMED_VECTOR_WRITES").as_deref(),
            Ok("1"),
            "sandbox vector writes must be enabled explicitly"
        );
        assert!(
            std::env::var("CODEX_MED_VECTOR_QDRANT_API_KEY")
                .is_ok_and(|value| !value.trim().is_empty()),
            "Qdrant API key must be set"
        );
        assert!(
            std::env::var("CODEX_MED_EMBEDDING_API_KEY")
                .is_ok_and(|value| !value.trim().is_empty()),
            "embedding API key must be set"
        );

        let temp = tempfile::tempdir().expect("temporary workspace");
        let registry = LiteratureRegistry::open_workspace(temp.path())
            .await
            .expect("literature registry");
        let RegistrationOutcome::Registered(registered) = registry
            .register(
                LiteratureInput {
                    pmid: Some("99999991".to_string()),
                    doi: Some("10.0000/codex-med-real-reconcile".to_string()),
                    paper_id: Some("PMID:99999991".to_string()),
                    title: Some("Codex Med real vector reconciliation sandbox".to_string()),
                    abstract_text: Some(format!("{}。{}", "a".repeat(1_900), "b".repeat(1_900))),
                    authors: vec!["Codex Med Integration Test".to_string()],
                    journal: Some("Sandbox Journal".to_string()),
                    publication_date: Some("2026".to_string()),
                    metadata: json!({"source": "real_reconciliation_sandbox"}),
                },
                None,
                "2026-07-28T00:00:00Z",
            )
            .await
            .expect("register sandbox PubMed record")
        else {
            panic!("expected sandbox PubMed registration");
        };
        let literature = registry
            .get(&registered.literature_id)
            .await
            .expect("read sandbox literature")
            .expect("sandbox literature exists");
        let chunks = build_pubmed_chunks(&literature);
        assert!(
            chunks.len() > 1,
            "sandbox literature must produce multiple chunks"
        );

        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(90))
            .build()
            .expect("HTTP client");
        let qdrant = QdrantRuntimeConfig::from_environment(None, None)
            .expect("valid sandbox Qdrant configuration");
        assert_eq!(
            qdrant_count(&client, &qdrant, None)
                .await
                .expect("initial sandbox count"),
            0,
            "sandbox collection must start empty"
        );
        let args = || ReconcilePubmedVectorsArgs {
            literature_ids: Some(vec![registered.literature_id.clone()]),
            limit: 1,
        };

        let first: serde_json::Value = serde_json::from_str(
            &reconcile_pubmed_vectors(&client, args(), temp.path())
                .await
                .expect("first real reconciliation"),
        )
        .expect("first reconciliation JSON");
        assert_eq!(first["complete"], true);
        assert_eq!(first["selected"], 1);
        assert_eq!(first["statuses"]["complete"], 1);
        assert_eq!(first["results"][0]["verified_points"], chunks.len());
        assert_eq!(
            qdrant_count(&client, &qdrant, None)
                .await
                .expect("count after first reconciliation"),
            chunks.len() as u64
        );

        let delete_response = qdrant
            .authenticate(client.post(format!(
                "{}?wait=true",
                qdrant.collection_url("/points/delete")
            )))
            .json(&json!({"points": [&chunks[0].point_id]}))
            .send()
            .await
            .expect("delete one sandbox point");
        assert!(
            delete_response.status().is_success(),
            "sandbox point deletion failed: {}",
            delete_response.status()
        );
        assert_eq!(
            qdrant_count(&client, &qdrant, None)
                .await
                .expect("count after simulated drift"),
            chunks.len() as u64 - 1
        );

        let repaired: serde_json::Value = serde_json::from_str(
            &reconcile_pubmed_vectors(&client, args(), temp.path())
                .await
                .expect("drift repair reconciliation"),
        )
        .expect("repair reconciliation JSON");
        assert_eq!(repaired["complete"], true);
        assert_eq!(repaired["statuses"]["complete"], 1);
        assert_eq!(repaired["results"][0]["verified_points"], chunks.len());
        assert_eq!(
            qdrant_count(&client, &qdrant, None)
                .await
                .expect("count after drift repair"),
            chunks.len() as u64,
            "reconciliation did not restore the missing vector"
        );

        let repeated: serde_json::Value = serde_json::from_str(
            &reconcile_pubmed_vectors(&client, args(), temp.path())
                .await
                .expect("repeated reconciliation"),
        )
        .expect("repeated reconciliation JSON");
        assert_eq!(repeated["complete"], true);
        assert_eq!(repeated["statuses"]["already_vectorized"], 1);
        assert_eq!(
            qdrant_count(&client, &qdrant, None)
                .await
                .expect("count after repeated reconciliation"),
            chunks.len() as u64,
            "repeated reconciliation duplicated vectors"
        );
    }

    #[tokio::test]
    async fn selects_all_pubmed_records_and_deduplicates_requested_ids() {
        let temp = tempfile::tempdir().expect("temp dir");
        let registry = LiteratureRegistry::open(temp.path().join("literatures.sqlite3"))
            .await
            .expect("registry");
        let RegistrationOutcome::Registered(pubmed) = registry
            .register(
                LiteratureInput {
                    pmid: Some("12345678".to_string()),
                    title: Some("PubMed record".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register PubMed")
        else {
            panic!("expected PubMed registration");
        };
        registry
            .register(
                LiteratureInput {
                    title: Some("Non-PubMed record".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register non-PubMed");

        let selected = select_pubmed_literatures(&registry, None, 100)
            .await
            .expect("select all");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].literature_id, pubmed.literature_id);

        let requested = vec![pubmed.literature_id.clone(), pubmed.literature_id.clone()];
        let selected = select_pubmed_literatures(&registry, Some(&requested), 100)
            .await
            .expect("select requested");
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].literature_id, pubmed.literature_id);
    }
}
