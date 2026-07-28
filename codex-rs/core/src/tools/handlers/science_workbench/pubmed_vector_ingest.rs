//! Collection-wide PubMed deduplication and guarded vector ingestion.

use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use reqwest::Method;
use reqwest::RequestBuilder;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;
use std::collections::HashSet;
use std::time::Duration;
use std::time::Instant;

use super::EMBEDDING_MODEL;
use super::EMBEDDING_URL;
use super::QDRANT_COLLECTION;
use super::QDRANT_URL;
use super::literature_registry::Literature;
use super::literature_registry::LiteratureRegistry;
use super::literature_registry::VectorLease;
use super::literature_registry::VectorLeaseOutcome;
use super::literature_registry_identity::identifiers_from_source_uri;
use super::literature_registry_identity::normalize_doi;
use super::literature_registry_identity::normalize_paper_id;
use super::literature_registry_identity::normalize_title;
use super::literature_registry_identity::normalized_first_author;
use super::literature_registry_identity::paper_id_from_source_uri;
use super::literature_registry_identity::publication_year;
use super::literature_registry_identity::title_jaccard;
use super::pubmed_chunks::PUBMED_EMBEDDING_PROFILE;
use super::pubmed_chunks::PubmedChunk;
use super::pubmed_chunks::build_pubmed_chunks;
use super::pubmed_chunks::pubmed_payload;

const WRITE_ENABLE_ENV: &str = "CODEX_MED_PUBMED_VECTOR_WRITES";
const BATCH_SIZE: usize = 32;

#[derive(Debug, Clone)]
pub(super) struct PubmedVectorConfig {
    qdrant_url: String,
    collection: String,
    qdrant_api_key: String,
    embedding_url: String,
    embedding_model: String,
    embedding_api_key: String,
    write_enabled: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PubmedVectorResult {
    pub(super) status: String,
    pub(super) expected_points: usize,
    pub(super) verified_points: usize,
    pub(super) existing_dataset: Option<String>,
    pub(super) existing_document_id: Option<String>,
    pub(super) verification_method: String,
    pub(super) review_case_id: Option<String>,
    pub(super) error: Option<String>,
}

pub(super) struct PubmedVectorIngestor<'a> {
    client: &'a reqwest::Client,
    config: PubmedVectorConfig,
    collection_points: tokio::sync::OnceCell<Vec<Value>>,
}

#[derive(Debug)]
struct ExistingVector {
    dataset: Option<String>,
    document_id: Option<String>,
    point_count: usize,
}

impl PubmedVectorConfig {
    pub(super) fn from_environment() -> Result<Self> {
        let write_enabled = std::env::var(WRITE_ENABLE_ENV).is_ok_and(|value| value == "1");
        let configured_collection = env_non_empty("CODEX_MED_VECTOR_COLLECTION");
        ensure!(
            !write_enabled || configured_collection.is_some(),
            "CODEX_MED_VECTOR_COLLECTION must be set explicitly when PubMed vector writes are enabled"
        );
        let collection = configured_collection.unwrap_or_else(|| QDRANT_COLLECTION.into());
        ensure!(
            collection
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.')),
            "invalid Qdrant collection name"
        );
        let qdrant_url = env_non_empty("CODEX_MED_VECTOR_QDRANT_URL")
            .unwrap_or_else(|| QDRANT_URL.into())
            .trim_end_matches('/')
            .to_string();
        let qdrant_api_key = env_non_empty("CODEX_MED_VECTOR_QDRANT_API_KEY")
            .or_else(|| env_non_empty("QDRANT_API_KEY"))
            .unwrap_or_default();
        let embedding_url =
            env_non_empty("CODEX_MED_EMBEDDING_URL").unwrap_or_else(|| EMBEDDING_URL.into());
        let embedding_api_key = env_non_empty("CODEX_MED_EMBEDDING_API_KEY")
            .or_else(|| env_non_empty("EMBEDDING_API_KEY"))
            .unwrap_or_default();
        ensure!(
            !write_enabled || qdrant_url != QDRANT_URL || !qdrant_api_key.is_empty(),
            "CODEX_MED_VECTOR_QDRANT_API_KEY must be set for writes to the default Qdrant service"
        );
        ensure!(
            !write_enabled || embedding_url != EMBEDDING_URL || !embedding_api_key.is_empty(),
            "CODEX_MED_EMBEDDING_API_KEY must be set for writes using the default embedding service"
        );
        Ok(Self {
            qdrant_url,
            collection,
            qdrant_api_key,
            embedding_url,
            embedding_model: env_non_empty("CODEX_MED_EMBEDDING_MODEL")
                .unwrap_or_else(|| EMBEDDING_MODEL.into()),
            embedding_api_key,
            write_enabled,
        })
    }

    pub(super) fn collection_name(&self) -> &str {
        &self.collection
    }

    pub(super) fn write_enabled(&self) -> bool {
        self.write_enabled
    }

    pub(super) fn embedding_model(&self) -> &str {
        &self.embedding_model
    }

    #[cfg(test)]
    fn test(qdrant_url: &str, embedding_url: &str, collection: &str, write_enabled: bool) -> Self {
        Self {
            qdrant_url: qdrant_url.trim_end_matches('/').to_string(),
            collection: collection.to_string(),
            qdrant_api_key: String::new(),
            embedding_url: embedding_url.to_string(),
            embedding_model: "test-embedding".to_string(),
            embedding_api_key: String::new(),
            write_enabled,
        }
    }

    #[cfg(test)]
    fn test_with_qdrant_api_key(
        qdrant_url: &str,
        embedding_url: &str,
        collection: &str,
        qdrant_api_key: &str,
        write_enabled: bool,
    ) -> Self {
        let mut config = Self::test(qdrant_url, embedding_url, collection, write_enabled);
        config.qdrant_api_key = qdrant_api_key.to_string();
        config
    }
}

impl<'a> PubmedVectorIngestor<'a> {
    pub(super) fn new(client: &'a reqwest::Client, config: PubmedVectorConfig) -> Self {
        Self {
            client,
            config,
            collection_points: tokio::sync::OnceCell::new(),
        }
    }

    pub(super) async fn ingest(
        &self,
        registry: &LiteratureRegistry,
        literature: &Literature,
        review_case_id: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> PubmedVectorResult {
        if let Some(review_case_id) = review_case_id {
            let _ = registry
                .record_vector_status(
                    &literature.literature_id,
                    &self.config.collection,
                    PUBMED_EMBEDDING_PROFILE,
                    "possible_duplicate",
                    None,
                    Some(review_case_id),
                    None,
                    &now.to_rfc3339(),
                )
                .await;
            return result("possible_duplicate", 0, 0, None, None);
        }

        let chunks = build_pubmed_chunks(literature);
        if chunks.is_empty() {
            let error = "PubMed record has no PMID or embeddable metadata".to_string();
            let _ = registry
                .record_vector_status(
                    &literature.literature_id,
                    &self.config.collection,
                    PUBMED_EMBEDDING_PROFILE,
                    "failed",
                    None,
                    None,
                    Some(&error),
                    &now.to_rfc3339(),
                )
                .await;
            return result("failed", 0, 0, None, Some(error));
        }

        let existing = match self.find_existing(literature).await {
            Ok(existing) => existing,
            Err(error) => {
                return self
                    .record_failure(registry, literature, chunks.len(), error, now)
                    .await;
            }
        };
        let mut own_pubmed_document = false;
        if let Some(existing) = existing {
            let expected_document = format!(
                "bio_literature:pubmed:PMID-{}",
                literature.pmid.as_deref().unwrap_or("")
            );
            let is_own_pubmed_document = existing.dataset.as_deref() == Some("pubmed")
                && existing.document_id.as_deref() == Some(expected_document.as_str());
            own_pubmed_document = is_own_pubmed_document;
            if !is_own_pubmed_document {
                let _ = registry
                    .record_vector_status(
                        &literature.literature_id,
                        &self.config.collection,
                        PUBMED_EMBEDDING_PROFILE,
                        "already_vectorized",
                        existing.dataset.as_deref(),
                        None,
                        None,
                        &now.to_rfc3339(),
                    )
                    .await;
                return existing_result(existing, "strong_identifier_document");
            }
        }
        if !own_pubmed_document {
            match self.find_possible_duplicates(literature).await {
                Ok(candidates) => {
                    for candidate in candidates {
                        if let Some(document_id) =
                            candidate.get("document_id").and_then(Value::as_str)
                        {
                            let source_system = format!("qdrant:{}", self.config.collection);
                            match registry
                                .source_record_literature_id(&source_system, document_id)
                                .await
                            {
                                Ok(Some(candidate_id))
                                    if candidate_id == literature.literature_id =>
                                {
                                    let dataset = candidate
                                        .get("project_id")
                                        .and_then(Value::as_str)
                                        .map(ToString::to_string);
                                    let document_id =
                                        candidate.get("document_id").and_then(Value::as_str);
                                    let existing = self.existing_document(dataset, document_id);
                                    let _ = registry
                                        .record_vector_status(
                                            &literature.literature_id,
                                            &self.config.collection,
                                            PUBMED_EMBEDDING_PROFILE,
                                            "already_vectorized",
                                            existing.dataset.as_deref(),
                                            None,
                                            None,
                                            &now.to_rfc3339(),
                                        )
                                        .await;
                                    return existing_result(existing, "reviewed_source_document");
                                }
                                Ok(_) => {}
                                Err(error) => {
                                    return self
                                        .record_failure(
                                            registry,
                                            literature,
                                            chunks.len(),
                                            error,
                                            now,
                                        )
                                        .await;
                                }
                            }
                        }
                        match registry
                            .qdrant_candidate_was_confirmed_different(
                                &literature.literature_id,
                                &self.config.collection,
                                &candidate,
                            )
                            .await
                        {
                            Ok(true) => continue,
                            Ok(false) => {}
                            Err(error) => {
                                return self
                                    .record_failure(registry, literature, chunks.len(), error, now)
                                    .await;
                            }
                        }
                        let review_case_id = match registry
                            .create_qdrant_possible_duplicate_case(
                                literature,
                                &self.config.collection,
                                candidate,
                                &now.to_rfc3339(),
                            )
                            .await
                        {
                            Ok(review_case_id) => review_case_id,
                            Err(error) => {
                                return self
                                    .record_failure(registry, literature, chunks.len(), error, now)
                                    .await;
                            }
                        };
                        let _ = registry
                            .record_vector_status(
                                &literature.literature_id,
                                &self.config.collection,
                                PUBMED_EMBEDDING_PROFILE,
                                "possible_duplicate",
                                None,
                                Some(&review_case_id),
                                None,
                                &now.to_rfc3339(),
                            )
                            .await;
                        let mut duplicate =
                            result("possible_duplicate", chunks.len(), 0, None, None);
                        duplicate.review_case_id = Some(review_case_id);
                        return duplicate;
                    }
                }
                Err(error) => {
                    return self
                        .record_failure(registry, literature, chunks.len(), error, now)
                        .await;
                }
            }
        }

        let deterministic_ids = match self.existing_point_ids(&chunks).await {
            Ok(ids) => ids,
            Err(error) => {
                return self
                    .record_failure(registry, literature, chunks.len(), error, now)
                    .await;
            }
        };
        if deterministic_ids.len() == chunks.len() {
            let _ = registry
                .record_vector_status(
                    &literature.literature_id,
                    &self.config.collection,
                    PUBMED_EMBEDDING_PROFILE,
                    "already_vectorized",
                    Some("pubmed"),
                    None,
                    None,
                    &now.to_rfc3339(),
                )
                .await;
            return result(
                "already_vectorized",
                chunks.len(),
                deterministic_ids.len(),
                Some("pubmed".to_string()),
                None,
            );
        }

        if !self.config.write_enabled {
            let _ = registry
                .record_vector_status(
                    &literature.literature_id,
                    &self.config.collection,
                    PUBMED_EMBEDDING_PROFILE,
                    "pending",
                    None,
                    None,
                    None,
                    &now.to_rfc3339(),
                )
                .await;
            return result(
                "vector_write_disabled",
                chunks.len(),
                deterministic_ids.len(),
                None,
                None,
            );
        }
        if self.config.collection == QDRANT_COLLECTION {
            let initialized = registry
                .has_validated_qdrant_initialization(&self.config.collection)
                .await
                .unwrap_or(false);
            if !initialized {
                let error = "production Qdrant baseline has not been validated".to_string();
                return self
                    .record_failure(
                        registry,
                        literature,
                        chunks.len(),
                        anyhow::anyhow!(error),
                        now,
                    )
                    .await;
            }
            if let Err(error) = self.ensure_recoverable_snapshot().await {
                return self
                    .record_failure(registry, literature, chunks.len(), error, now)
                    .await;
            }
        }
        if let Err(error) = registry
            .reopen_terminal_vector_job_if_incomplete(
                &literature.literature_id,
                &self.config.collection,
                PUBMED_EMBEDDING_PROFILE,
                chunks.len(),
                deterministic_ids.len(),
                chrono::Utc::now(),
            )
            .await
        {
            return self
                .record_failure(registry, literature, chunks.len(), error, now)
                .await;
        }

        let lease = match registry
            .claim_vector_job(
                &literature.literature_id,
                &self.config.collection,
                PUBMED_EMBEDDING_PROFILE,
                now,
            )
            .await
        {
            Ok(VectorLeaseOutcome::Acquired(lease)) => lease,
            Ok(VectorLeaseOutcome::NotAcquired { status }) => {
                return result(&status, chunks.len(), 0, None, None);
            }
            Err(error) => {
                return result("failed", chunks.len(), 0, None, Some(format!("{error:#}")));
            }
        };
        match self
            .ingest_claimed(registry, literature, &chunks, &lease, now)
            .await
        {
            Ok(verified) => result("complete", chunks.len(), verified, None, None),
            Err(error) => {
                let error = format!("{error:#}");
                let _ = registry
                    .update_claimed_vector_job(
                        &lease,
                        "failed",
                        Some(chunks.len()),
                        None,
                        Some(&error),
                        chrono::Utc::now(),
                    )
                    .await;
                result("failed", chunks.len(), 0, None, Some(error))
            }
        }
    }

    async fn ingest_claimed(
        &self,
        registry: &LiteratureRegistry,
        literature: &Literature,
        chunks: &[PubmedChunk],
        lease: &VectorLease,
        imported_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<usize> {
        let existing_ids = self.existing_point_ids(chunks).await?;
        let missing = chunks
            .iter()
            .filter(|chunk| !existing_ids.contains(&chunk.point_id))
            .collect::<Vec<_>>();
        if missing.is_empty() {
            registry
                .update_claimed_vector_job(
                    lease,
                    "complete",
                    Some(chunks.len()),
                    Some(chunks.len()),
                    None,
                    chrono::Utc::now(),
                )
                .await?;
            return Ok(chunks.len());
        }

        let (expected_dimension, distance) = self.collection_vector_config().await?;
        ensure!(
            distance.eq_ignore_ascii_case("cosine"),
            "collection distance {distance} is incompatible with pubmed-v1 (expected Cosine)"
        );
        let mut points = Vec::with_capacity(missing.len());
        let mut heartbeat = Instant::now();
        for chunk in missing {
            let vector = self.embed(&chunk.text).await?;
            ensure!(
                vector.len() == expected_dimension,
                "embedding dimension {} does not match collection dimension {expected_dimension}",
                vector.len()
            );
            points.push(json!({
                "id": chunk.point_id,
                "vector": vector,
                "payload": pubmed_payload(literature, chunk, &imported_at.to_rfc3339()),
            }));
            if heartbeat.elapsed() >= Duration::from_secs(50) {
                registry
                    .update_claimed_vector_job(
                        lease,
                        "embedding",
                        Some(chunks.len()),
                        Some(existing_ids.len()),
                        None,
                        chrono::Utc::now(),
                    )
                    .await?;
                heartbeat = Instant::now();
            }
        }

        registry
            .update_claimed_vector_job(
                lease,
                "upserting",
                Some(chunks.len()),
                Some(existing_ids.len()),
                None,
                chrono::Utc::now(),
            )
            .await?;
        for batch in points.chunks(BATCH_SIZE) {
            self.upsert(batch).await?;
            registry
                .update_claimed_vector_job(
                    lease,
                    "upserting",
                    Some(chunks.len()),
                    None,
                    None,
                    chrono::Utc::now(),
                )
                .await?;
        }
        registry
            .update_claimed_vector_job(
                lease,
                "verifying",
                Some(chunks.len()),
                None,
                None,
                chrono::Utc::now(),
            )
            .await?;
        let verified = self.existing_point_ids(chunks).await?.len();
        ensure!(
            verified == chunks.len(),
            "Qdrant verification found {verified} of {} expected PubMed points",
            chunks.len()
        );
        registry
            .update_claimed_vector_job(
                lease,
                "complete",
                Some(chunks.len()),
                Some(verified),
                None,
                chrono::Utc::now(),
            )
            .await?;
        Ok(verified)
    }

    async fn find_existing(&self, literature: &Literature) -> Result<Option<ExistingVector>> {
        let literature_keys = strong_identifier_keys(literature);
        let mut documents =
            std::collections::BTreeMap::<(Option<String>, Option<String>), usize>::new();
        for point in self.collection_points().await? {
            let payload = point.get("payload").unwrap_or(&Value::Null);
            if literature_keys.is_disjoint(&payload_strong_identifier_keys(payload)) {
                continue;
            }
            let key = (
                payload
                    .get("project_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
                payload
                    .get("document_id")
                    .and_then(Value::as_str)
                    .map(ToString::to_string),
            );
            *documents.entry(key).or_default() += 1;
        }
        Ok(documents
            .into_iter()
            .max_by(|(left_key, left_count), (right_key, right_count)| {
                left_count
                    .cmp(right_count)
                    .then_with(|| right_key.cmp(left_key))
            })
            .map(|((dataset, document_id), point_count)| ExistingVector {
                dataset,
                document_id,
                point_count,
            }))
    }

    fn existing_document(
        &self,
        dataset: Option<String>,
        document_id: Option<&str>,
    ) -> ExistingVector {
        let point_count = self
            .collection_points
            .get()
            .into_iter()
            .flatten()
            .filter(|point| {
                let payload = point.get("payload").unwrap_or(&Value::Null);
                payload.get("project_id").and_then(Value::as_str) == dataset.as_deref()
                    && payload.get("document_id").and_then(Value::as_str) == document_id
            })
            .count();
        ExistingVector {
            dataset,
            document_id: document_id.map(ToString::to_string),
            point_count,
        }
    }

    async fn find_possible_duplicates(&self, literature: &Literature) -> Result<Vec<Value>> {
        let Some(title) = literature.title.as_deref() else {
            return Ok(Vec::new());
        };
        let normalized_title = normalize_title(title);
        if normalized_title.is_empty() {
            return Ok(Vec::new());
        }
        let incoming_author = normalized_first_author(&literature.authors);
        let incoming_year = publication_year(literature.publication_date.as_deref());
        let mut candidates = Vec::new();
        for point in self.collection_points().await? {
            let payload = point.get("payload").unwrap_or(&Value::Null);
            let Some(candidate_title) = payload
                .get("title")
                .and_then(Value::as_str)
                .filter(|title| !title.trim().is_empty())
            else {
                continue;
            };
            let candidate_authors = payload_authors(payload);
            let candidate_author = normalized_first_author(&candidate_authors);
            let candidate_year = payload_publication_year(payload);
            let jaccard = title_jaccard(title, candidate_title);
            let author_conflict = incoming_author.is_some()
                && candidate_author.is_some()
                && incoming_author != candidate_author;
            let year_conflict = incoming_year
                .zip(candidate_year)
                .is_some_and(|(left, right)| (left - right).abs() > 1);
            let exact_rule = normalize_title(candidate_title) == normalized_title
                && !author_conflict
                && !year_conflict;
            let fuzzy_rule = jaccard >= 0.90
                && incoming_author.is_some()
                && incoming_author == candidate_author
                && incoming_year
                    .zip(candidate_year)
                    .is_some_and(|(left, right)| (left - right).abs() <= 1);
            if exact_rule || fuzzy_rule {
                candidates.push(json!({
                    "point_id": point.get("id"),
                    "document_id": payload.get("document_id"),
                    "project_id": payload.get("project_id"),
                    "title": candidate_title,
                    "normalized_title": normalize_title(candidate_title),
                    "title_jaccard": jaccard,
                    "first_author": candidate_author,
                    "publication_year": candidate_year,
                    "matched_rule": if exact_rule {
                        "exact_title_no_conflict"
                    } else {
                        "jaccard_author_year"
                    },
                }));
            }
        }
        Ok(candidates)
    }

    async fn collection_points(&self) -> Result<&Vec<Value>> {
        self.collection_points
            .get_or_try_init(|| async {
                let mut points = Vec::new();
                let mut offset: Option<Value> = None;
                let mut seen_offsets = HashSet::new();
                loop {
                    let mut body = json!({
                        "limit": 256,
                        "with_payload": [
                            "document_id",
                            "paper_id",
                            "source_uri",
                            "project_id",
                            "title",
                            "authors",
                            "author",
                            "publication_date",
                            "publication_year",
                            "year"
                        ],
                        "with_vector": false,
                    });
                    if let Some(offset) = offset.as_ref()
                        && let Some(object) = body.as_object_mut()
                    {
                        object.insert("offset".to_string(), offset.clone());
                    }
                    let value = self
                        .qdrant_json(
                            Method::POST,
                            &format!(
                                "{}/collections/{}/points/scroll",
                                self.config.qdrant_url, self.config.collection
                            ),
                            Some(body),
                            "Qdrant collection-wide literature scan",
                        )
                        .await?;
                    let page = value
                        .pointer("/result/points")
                        .and_then(Value::as_array)
                        .context("Qdrant scroll response is missing points")?;
                    points.extend(page.iter().map(projected_collection_point));
                    let Some(next_offset) = value
                        .pointer("/result/next_page_offset")
                        .cloned()
                        .filter(|offset| !offset.is_null())
                    else {
                        break;
                    };
                    ensure!(
                        seen_offsets.insert(next_offset.to_string()),
                        "Qdrant collection-wide scan repeated an offset"
                    );
                    offset = Some(next_offset);
                }
                Ok(points)
            })
            .await
    }

    async fn existing_point_ids(&self, chunks: &[PubmedChunk]) -> Result<HashSet<String>> {
        let mut existing = HashSet::new();
        for batch in chunks.chunks(256) {
            let value = self
                .qdrant_json(
                    Method::POST,
                    &format!(
                        "{}/collections/{}/points",
                        self.config.qdrant_url, self.config.collection
                    ),
                    Some(json!({
                        "ids": batch.iter().map(|chunk| &chunk.point_id).collect::<Vec<_>>(),
                        "with_payload": false,
                        "with_vector": false,
                    })),
                    "Qdrant point completeness check",
                )
                .await?;
            for point in value
                .get("result")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(id) = point.get("id").and_then(Value::as_str) {
                    existing.insert(id.to_string());
                }
            }
        }
        Ok(existing)
    }

    async fn collection_vector_config(&self) -> Result<(usize, String)> {
        let value = self
            .qdrant_json(
                Method::GET,
                &format!(
                    "{}/collections/{}",
                    self.config.qdrant_url, self.config.collection
                ),
                None,
                "Qdrant collection configuration",
            )
            .await?;
        let size = value
            .pointer("/result/config/params/vectors/size")
            .and_then(Value::as_u64)
            .map(|size| size as usize)
            .context("Qdrant collection response is missing vector size")?;
        let distance = value
            .pointer("/result/config/params/vectors/distance")
            .and_then(Value::as_str)
            .context("Qdrant collection response is missing vector distance")?;
        Ok((size, distance.to_string()))
    }

    async fn ensure_recoverable_snapshot(&self) -> Result<()> {
        let value = self
            .qdrant_json(
                Method::GET,
                &format!(
                    "{}/collections/{}/snapshots",
                    self.config.qdrant_url, self.config.collection
                ),
                None,
                "Qdrant snapshot precondition",
            )
            .await?;
        let snapshots = value
            .get("result")
            .and_then(Value::as_array)
            .context("Qdrant snapshot response is missing result")?;
        ensure!(
            !snapshots.is_empty(),
            "production Qdrant collection has no recoverable snapshot"
        );
        Ok(())
    }

    async fn embed(&self, text: &str) -> Result<Vec<f64>> {
        let mut request = self.client.post(&self.config.embedding_url).json(&json!({
            "model": self.config.embedding_model,
            "input": text,
        }));
        if !self.config.embedding_api_key.is_empty() {
            request = request.bearer_auth(&self.config.embedding_api_key);
        }
        let value = response_json(request, "PubMed embedding").await?;
        let raw = value
            .get("embedding")
            .and_then(Value::as_array)
            .or_else(|| value.pointer("/data/0/embedding").and_then(Value::as_array))
            .or_else(|| value.as_array())
            .context("embedding response is missing a vector")?;
        raw.iter()
            .map(|item| {
                item.as_f64()
                    .context("embedding vector contains a non-number")
            })
            .collect()
    }

    async fn upsert(&self, points: &[Value]) -> Result<()> {
        ensure!(
            self.config.write_enabled,
            "Qdrant PubMed vector writes are disabled"
        );
        self.qdrant_json(
            Method::PUT,
            &format!(
                "{}/collections/{}/points?wait=true",
                self.config.qdrant_url, self.config.collection
            ),
            Some(json!({"points": points})),
            "Qdrant PubMed upsert",
        )
        .await?;
        Ok(())
    }

    async fn qdrant_json(
        &self,
        method: Method,
        url: &str,
        body: Option<Value>,
        label: &str,
    ) -> Result<Value> {
        let mut request = self.client.request(method, url);
        if !self.config.qdrant_api_key.is_empty() {
            request = request.header("api-key", &self.config.qdrant_api_key);
        }
        if let Some(body) = body {
            request = request.json(&body);
        }
        response_json(request, label).await
    }

    async fn record_failure(
        &self,
        registry: &LiteratureRegistry,
        literature: &Literature,
        expected_points: usize,
        error: anyhow::Error,
        now: chrono::DateTime<chrono::Utc>,
    ) -> PubmedVectorResult {
        let error = format!("{error:#}");
        let _ = registry
            .record_vector_status(
                &literature.literature_id,
                &self.config.collection,
                PUBMED_EMBEDDING_PROFILE,
                "failed",
                None,
                None,
                Some(&error),
                &now.to_rfc3339(),
            )
            .await;
        result("failed", expected_points, 0, None, Some(error))
    }
}

fn strong_identifier_keys(literature: &Literature) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    if let Some(pmid) = literature.pmid.as_deref() {
        keys.insert(format!("pmid:{pmid}"));
    }
    if let Some(doi) = literature.doi.as_deref().and_then(normalize_doi) {
        keys.insert(format!("doi:{doi}"));
    }
    if let Some(paper_id) = literature.paper_id.as_deref() {
        insert_paper_identifier_keys(&mut keys, paper_id);
    }
    keys
}

fn payload_strong_identifier_keys(payload: &Value) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    if let Some(paper_id) = payload.get("paper_id").and_then(Value::as_str) {
        insert_paper_identifier_keys(&mut keys, paper_id);
    }
    if let Some(source_uri) = payload.get("source_uri").and_then(Value::as_str) {
        let (pmid, doi) = identifiers_from_source_uri(source_uri);
        if let Some(pmid) = pmid {
            keys.insert(format!("pmid:{pmid}"));
        }
        if let Some(doi) = doi {
            keys.insert(format!("doi:{doi}"));
        }
        if let Some(paper_id) = paper_id_from_source_uri(source_uri) {
            keys.insert(format!("paper:{paper_id}"));
        }
    }
    keys
}

fn insert_paper_identifier_keys(keys: &mut BTreeSet<String>, value: &str) {
    if let Some(paper_id) = normalize_paper_id(value) {
        if let Some(pmid) = paper_id.strip_prefix("PMID:") {
            keys.insert(format!("pmid:{pmid}"));
        }
        keys.insert(format!("paper:{paper_id}"));
    }
    if let Some(doi) = normalize_doi(value) {
        keys.insert(format!("doi:{doi}"));
    }
}

fn payload_authors(payload: &Value) -> Vec<String> {
    for key in ["authors", "author"] {
        match payload.get(key) {
            Some(Value::Array(authors)) => {
                let parsed = authors
                    .iter()
                    .filter_map(|author| {
                        author
                            .as_str()
                            .or_else(|| author.get("name").and_then(Value::as_str))
                    })
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                if !parsed.is_empty() {
                    return parsed;
                }
            }
            Some(Value::String(authors)) if !authors.trim().is_empty() => {
                return authors
                    .split(';')
                    .map(str::trim)
                    .filter(|author| !author.is_empty())
                    .map(ToString::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    Vec::new()
}

fn payload_publication_year(payload: &Value) -> Option<i32> {
    for key in ["publication_date", "publication_year", "year"] {
        let Some(value) = payload.get(key) else {
            continue;
        };
        let text = value
            .as_str()
            .map(ToString::to_string)
            .or_else(|| value.as_i64().map(|value| value.to_string()));
        if let Some(year) = publication_year(text.as_deref()) {
            return Some(year);
        }
    }
    None
}

fn projected_collection_point(point: &Value) -> Value {
    let payload = point.get("payload").unwrap_or(&Value::Null);
    let mut projected_payload = serde_json::Map::new();
    for key in [
        "document_id",
        "paper_id",
        "source_uri",
        "project_id",
        "title",
        "authors",
        "author",
        "publication_date",
        "publication_year",
        "year",
    ] {
        if let Some(value) = payload.get(key) {
            projected_payload.insert(key.to_string(), value.clone());
        }
    }
    json!({
        "id": point.get("id").cloned().unwrap_or(Value::Null),
        "payload": projected_payload,
    })
}

async fn response_json(request: RequestBuilder, label: &str) -> Result<Value> {
    let response = request
        .send()
        .await
        .with_context(|| format!("{label} request failed"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .with_context(|| format!("failed to read {label} response"))?;
    ensure!(
        status.is_success(),
        "{label} returned HTTP {status}: {}",
        text.chars().take(500).collect::<String>()
    );
    serde_json::from_str(&text).with_context(|| format!("{label} returned invalid JSON"))
}

fn result(
    status: &str,
    expected_points: usize,
    verified_points: usize,
    existing_dataset: Option<String>,
    error: Option<String>,
) -> PubmedVectorResult {
    PubmedVectorResult {
        status: status.to_string(),
        expected_points,
        verified_points,
        existing_dataset,
        existing_document_id: None,
        verification_method: if matches!(status, "complete" | "already_vectorized") {
            "deterministic_point_ids".to_string()
        } else {
            "not_complete".to_string()
        },
        review_case_id: None,
        error,
    }
}

fn existing_result(existing: ExistingVector, verification_method: &str) -> PubmedVectorResult {
    PubmedVectorResult {
        status: "already_vectorized".to_string(),
        expected_points: existing.point_count,
        verified_points: existing.point_count,
        existing_dataset: existing.dataset,
        existing_document_id: existing.document_id,
        verification_method: verification_method.to_string(),
        review_case_id: None,
        error: None,
    }
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::Request;
    use wiremock::Respond;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    struct JsonSequence {
        calls: AtomicUsize,
        bodies: Vec<Value>,
    }

    impl Respond for JsonSequence {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(
                self.bodies
                    .get(call)
                    .unwrap_or_else(|| panic!("unexpected request {call}")),
            )
        }
    }

    struct TemplateSequence {
        calls: AtomicUsize,
        templates: Vec<ResponseTemplate>,
    }

    impl Respond for TemplateSequence {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            self.templates
                .get(call)
                .unwrap_or_else(|| panic!("unexpected request {call}"))
                .clone()
        }
    }

    async fn registry_literature_with_abstract(
        abstract_text: String,
    ) -> (tempfile::TempDir, LiteratureRegistry, Literature) {
        registry_literature_for("12345678", abstract_text).await
    }

    async fn registry_literature_for(
        pmid: &str,
        abstract_text: String,
    ) -> (tempfile::TempDir, LiteratureRegistry, Literature) {
        let temp = tempfile::tempdir().expect("temp dir");
        let registry = LiteratureRegistry::open(temp.path().join("literatures.sqlite3"))
            .await
            .expect("registry");
        let registered = registry
            .register(
                super::super::literature_registry::LiteratureInput {
                    pmid: Some(pmid.to_string()),
                    paper_id: Some(format!("PMID:{pmid}")),
                    title: Some(format!("Example title {pmid}")),
                    abstract_text: Some(abstract_text),
                    metadata: json!({}),
                    ..Default::default()
                },
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register");
        let super::super::literature_registry::RegistrationOutcome::Registered(registered) =
            registered
        else {
            panic!("expected registration");
        };
        let literature = registry
            .get(&registered.literature_id)
            .await
            .expect("get")
            .expect("literature");
        (temp, registry, literature)
    }

    async fn sandbox_qdrant_json(request: RequestBuilder, api_key: &str) -> Value {
        let request = if api_key.is_empty() {
            request
        } else {
            request.header("api-key", api_key)
        };
        response_json(request, "sandbox Qdrant")
            .await
            .expect("sandbox Qdrant response")
    }

    async fn create_sandbox_collection(
        client: &reqwest::Client,
        qdrant_url: &str,
        collection: &str,
        api_key: &str,
    ) {
        sandbox_qdrant_json(
            client
                .put(format!("{qdrant_url}/collections/{collection}"))
                .json(&json!({
                    "vectors": {
                        "size": 3,
                        "distance": "Cosine",
                    }
                })),
            api_key,
        )
        .await;
    }

    async fn sandbox_point_count(
        client: &reqwest::Client,
        qdrant_url: &str,
        collection: &str,
        api_key: &str,
    ) -> usize {
        sandbox_qdrant_json(
            client.get(format!("{qdrant_url}/collections/{collection}")),
            api_key,
        )
        .await
        .pointer("/result/points_count")
        .and_then(Value::as_u64)
        .expect("sandbox point count") as usize
    }

    async fn seed_sandbox_point(
        client: &reqwest::Client,
        qdrant_url: &str,
        collection: &str,
        point_id: &str,
        payload: Value,
        api_key: &str,
    ) {
        sandbox_qdrant_json(
            client
                .put(format!(
                    "{qdrant_url}/collections/{collection}/points?wait=true"
                ))
                .json(&json!({
                    "points": [{
                        "id": point_id,
                        "vector": [0.1, 0.2, 0.3],
                        "payload": payload,
                    }]
                })),
            api_key,
        )
        .await;
    }

    async fn registry_literature() -> (tempfile::TempDir, LiteratureRegistry, Literature) {
        registry_literature_with_abstract("Example abstract.".to_string()).await
    }

    fn test_now() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn test_config_never_enables_writes_implicitly() {
        let config = PubmedVectorConfig::test(
            "http://127.0.0.1:1",
            "http://127.0.0.1:2/embed",
            "temporary",
            false,
        );
        assert!(!config.write_enabled);
        assert_eq!(config.collection, "temporary");
    }

    #[tokio::test]
    async fn disabled_writes_stop_before_embedding_or_upsert() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points/scroll"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"result": {"points": []}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": []})))
            .expect(1)
            .mount(&server)
            .await;
        let (_temp, registry, literature) = registry_literature().await;
        let client = reqwest::Client::new();
        let config = PubmedVectorConfig::test(
            &server.uri(),
            &format!("{}/embed", server.uri()),
            "temporary",
            false,
        );
        let result = PubmedVectorIngestor::new(&client, config)
            .ingest(&registry, &literature, None, test_now())
            .await;

        assert_eq!(result.status, "vector_write_disabled");
        assert_eq!(result.expected_points, 1);
        assert_eq!(result.verified_points, 0);
    }

    #[tokio::test]
    async fn strong_identifier_match_in_local_dataset_never_writes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points/scroll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {"points": [
                    {
                        "id": "local-point-1",
                        "payload": {
                            "project_id": "data-extract-new",
                            "document_id": "local-full-text",
                            "paper_id": "PMID:12345678"
                        }
                    },
                    {
                        "id": "local-point-2",
                        "payload": {
                            "project_id": "data-extract-new",
                            "document_id": "local-full-text",
                            "paper_id": "PMID:12345678"
                        }
                    }
                ]}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let (_temp, registry, literature) = registry_literature().await;
        let client = reqwest::Client::new();
        let config = PubmedVectorConfig::test(
            &server.uri(),
            &format!("{}/embed", server.uri()),
            "temporary",
            true,
        );
        let result = PubmedVectorIngestor::new(&client, config)
            .ingest(&registry, &literature, None, test_now())
            .await;

        assert_eq!(result.status, "already_vectorized");
        assert_eq!(result.existing_dataset.as_deref(), Some("data-extract-new"));
        assert_eq!(
            result.existing_document_id.as_deref(),
            Some("local-full-text")
        );
        assert_eq!(result.expected_points, 2);
        assert_eq!(result.verified_points, 2);
        assert_eq!(result.verification_method, "strong_identifier_document");
    }

    #[tokio::test]
    async fn human_merged_weak_qdrant_match_is_reused_without_another_review() {
        use super::super::literature_registry::LiteratureInput;
        use super::super::literature_registry::RegistrationOutcome;
        use super::super::literature_registry::SourceRecord;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points/scroll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {"points": [{
                    "id": "local-point",
                    "payload": {
                        "project_id": "data-extract-new",
                        "document_id": "local-full-text",
                        "title": "Shared weak title",
                        "authors": ["Doe, Jane"],
                        "publication_date": "2025"
                    }
                }]}
            })))
            .expect(1)
            .mount(&server)
            .await;
        let temp = tempfile::tempdir().expect("temp dir");
        let registry = LiteratureRegistry::open(temp.path().join("literatures.sqlite3"))
            .await
            .expect("registry");
        let RegistrationOutcome::Registered(local) = registry
            .register(
                LiteratureInput {
                    title: Some("Shared weak title".to_string()),
                    authors: vec!["Doe, Jane".to_string()],
                    publication_date: Some("2025".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(SourceRecord {
                    system: "qdrant:temporary".to_string(),
                    key: "local-full-text".to_string(),
                    match_method: "qdrant_document".to_string(),
                }),
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register local")
        else {
            panic!("expected local registration");
        };
        let RegistrationOutcome::Registered(pubmed) = registry
            .register(
                LiteratureInput {
                    pmid: Some("12345678".to_string()),
                    paper_id: Some("PMID:12345678".to_string()),
                    title: Some("Shared weak title".to_string()),
                    abstract_text: Some("PubMed abstract.".to_string()),
                    authors: vec!["Doe, Jane".to_string()],
                    publication_date: Some("2025".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(SourceRecord {
                    system: "pubmed".to_string(),
                    key: "12345678".to_string(),
                    match_method: "pmid".to_string(),
                }),
                "2026-01-02T00:00:00Z",
            )
            .await
            .expect("register PubMed")
        else {
            panic!("expected PubMed registration");
        };
        let review_case_id = pubmed.review_case_id.expect("weak duplicate review");
        registry
            .merge_literatures(
                &local.literature_id,
                &pubmed.literature_id,
                "human confirmed same literature",
                Some(&review_case_id),
                "2026-01-03T00:00:00Z",
            )
            .await
            .expect("merge reviewed duplicate");
        let literature = registry
            .get(&local.literature_id)
            .await
            .expect("get")
            .expect("merged literature");
        let client = reqwest::Client::new();
        let config = PubmedVectorConfig::test(
            &server.uri(),
            &format!("{}/embed", server.uri()),
            "temporary",
            true,
        );

        let result = PubmedVectorIngestor::new(&client, config)
            .ingest(&registry, &literature, None, test_now())
            .await;

        assert_eq!(result.status, "already_vectorized");
        assert_eq!(result.existing_dataset.as_deref(), Some("data-extract-new"));
        assert_eq!(result.review_case_id, None);
    }

    #[tokio::test]
    async fn first_import_writes_once_and_second_run_reuses_points() {
        let server = MockServer::start().await;
        let (_temp, registry, literature) = registry_literature().await;
        let chunk = build_pubmed_chunks(&literature)
            .into_iter()
            .next()
            .expect("chunk");
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points/scroll"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"result": {"points": []}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points"))
            .respond_with(JsonSequence {
                calls: AtomicUsize::new(0),
                bodies: vec![
                    json!({"result": []}),
                    json!({"result": []}),
                    json!({"result": [{"id": chunk.point_id}]}),
                    json!({"result": [{"id": chunk.point_id}]}),
                ],
            })
            .expect(4)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/collections/temporary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {"config": {"params": {"vectors": {
                    "size": 3,
                    "distance": "Cosine"
                }}}}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/embed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{"embedding": [0.1, 0.2, 0.3]}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/collections/temporary/points"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"result": {"status": "completed"}})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let config = PubmedVectorConfig::test(
            &server.uri(),
            &format!("{}/embed", server.uri()),
            "temporary",
            true,
        );
        let ingestor = PubmedVectorIngestor::new(&client, config);
        let first = ingestor
            .ingest(&registry, &literature, None, test_now())
            .await;
        let second = ingestor
            .ingest(&registry, &literature, None, test_now())
            .await;

        assert_eq!(first.status, "complete");
        assert_eq!(first.verified_points, 1);
        assert_eq!(second.status, "already_vectorized");
        assert_eq!(second.verified_points, 1);
    }

    #[tokio::test]
    async fn terminal_job_with_qdrant_drift_upserts_only_missing_chunks() {
        let server = MockServer::start().await;
        let abstract_text = format!("{}。{}", "a".repeat(1_100), "b".repeat(1_100));
        let (_temp, registry, literature) = registry_literature_with_abstract(abstract_text).await;
        let chunks = build_pubmed_chunks(&literature);
        assert_eq!(chunks.len(), 2);
        registry
            .record_vector_status(
                &literature.literature_id,
                "temporary",
                PUBMED_EMBEDDING_PROFILE,
                "complete",
                None,
                None,
                None,
                &test_now().to_rfc3339(),
            )
            .await
            .expect("record stale terminal status");
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points/scroll"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {"points": [{
                    "id": chunks[0].point_id,
                    "payload": {
                        "project_id": "pubmed",
                        "document_id": "bio_literature:pubmed:PMID-12345678"
                    }
                }]}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points"))
            .respond_with(JsonSequence {
                calls: AtomicUsize::new(0),
                bodies: vec![
                    json!({"result": [{"id": chunks[0].point_id}]}),
                    json!({"result": [{"id": chunks[0].point_id}]}),
                    json!({"result": [
                        {"id": chunks[0].point_id},
                        {"id": chunks[1].point_id}
                    ]}),
                ],
            })
            .expect(3)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/collections/temporary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {"config": {"params": {"vectors": {
                    "size": 3,
                    "distance": "Cosine"
                }}}}
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/embed"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "embedding": [0.1, 0.2, 0.3]
            })))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/collections/temporary/points"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {}})))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let config = PubmedVectorConfig::test(
            &server.uri(),
            &format!("{}/embed", server.uri()),
            "temporary",
            true,
        );
        let result = PubmedVectorIngestor::new(&client, config)
            .ingest(&registry, &literature, None, test_now())
            .await;

        assert_eq!(result.status, "complete");
        assert_eq!(result.expected_points, 2);
        assert_eq!(result.verified_points, 2);
    }

    #[tokio::test]
    async fn embedding_failure_retries_without_changing_identity_or_duplicate_upsert() {
        let server = MockServer::start().await;
        let (_temp, registry, literature) = registry_literature().await;
        let chunk = build_pubmed_chunks(&literature)
            .into_iter()
            .next()
            .expect("chunk");
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points/scroll"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"result": {"points": []}})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/collections/temporary/points"))
            .respond_with(JsonSequence {
                calls: AtomicUsize::new(0),
                bodies: vec![
                    json!({"result": []}),
                    json!({"result": []}),
                    json!({"result": []}),
                    json!({"result": []}),
                    json!({"result": [{"id": chunk.point_id}]}),
                ],
            })
            .expect(5)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/collections/temporary"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "result": {"config": {"params": {"vectors": {
                    "size": 3,
                    "distance": "Cosine"
                }}}}
            })))
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/embed"))
            .respond_with(TemplateSequence {
                calls: AtomicUsize::new(0),
                templates: vec![
                    ResponseTemplate::new(500).set_body_string("temporary embedding failure"),
                    ResponseTemplate::new(200).set_body_json(json!({"embedding": [0.1, 0.2, 0.3]})),
                ],
            })
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/collections/temporary/points"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": {}})))
            .expect(1)
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let config = PubmedVectorConfig::test(
            &server.uri(),
            &format!("{}/embed", server.uri()),
            "temporary",
            true,
        );
        let ingestor = PubmedVectorIngestor::new(&client, config);
        let literature_id = literature.literature_id.clone();
        let first = ingestor
            .ingest(&registry, &literature, None, test_now())
            .await;
        let second = ingestor
            .ingest(&registry, &literature, None, test_now())
            .await;

        assert_eq!(first.status, "failed");
        assert_eq!(second.status, "complete");
        assert_eq!(literature.literature_id, literature_id);
        assert_eq!(second.verified_points, 1);
    }

    #[tokio::test]
    #[ignore = "requires CODEX_MED_QDRANT_IT_URL pointing to a disposable Qdrant"]
    async fn real_qdrant_sandbox_matrix() {
        let qdrant_url = std::env::var("CODEX_MED_QDRANT_IT_URL")
            .expect("CODEX_MED_QDRANT_IT_URL must be set")
            .trim_end_matches('/')
            .to_string();
        let qdrant_api_key = std::env::var("CODEX_MED_QDRANT_IT_API_KEY").unwrap_or_default();
        let collection = std::env::var("CODEX_MED_QDRANT_IT_COLLECTION")
            .unwrap_or_else(|_| format!("codex_med_it_{}", uuid::Uuid::new_v4().simple()));
        let client = reqwest::Client::new();
        create_sandbox_collection(&client, &qdrant_url, &collection, &qdrant_api_key).await;

        let embedding_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embed"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"embedding": [0.1, 0.2, 0.3]})),
            )
            .mount(&embedding_server)
            .await;

        let (_temp, registry, literature) = registry_literature().await;
        let disabled = PubmedVectorConfig::test_with_qdrant_api_key(
            &qdrant_url,
            &format!("{}/embed", embedding_server.uri()),
            &collection,
            &qdrant_api_key,
            false,
        );
        let disabled_result = PubmedVectorIngestor::new(&client, disabled)
            .ingest(&registry, &literature, None, test_now())
            .await;
        assert_eq!(
            disabled_result,
            PubmedVectorResult {
                status: "vector_write_disabled".to_string(),
                expected_points: 1,
                verified_points: 0,
                existing_dataset: None,
                existing_document_id: None,
                verification_method: "not_complete".to_string(),
                review_case_id: None,
                error: None,
            }
        );
        assert_eq!(
            sandbox_point_count(&client, &qdrant_url, &collection, &qdrant_api_key).await,
            0
        );

        let enabled = PubmedVectorConfig::test_with_qdrant_api_key(
            &qdrant_url,
            &format!("{}/embed", embedding_server.uri()),
            &collection,
            &qdrant_api_key,
            true,
        );
        let ingestor = PubmedVectorIngestor::new(&client, enabled);
        let first = ingestor
            .ingest(&registry, &literature, None, test_now())
            .await;
        let second = ingestor
            .ingest(&registry, &literature, None, test_now())
            .await;
        assert_eq!(
            (first.status.as_str(), second.status.as_str()),
            ("complete", "already_vectorized")
        );
        assert_eq!(
            sandbox_point_count(&client, &qdrant_url, &collection, &qdrant_api_key).await,
            1
        );
        let first_chunk = build_pubmed_chunks(&literature)
            .into_iter()
            .next()
            .expect("first chunk");
        let retrieved = sandbox_qdrant_json(
            client
                .post(format!("{qdrant_url}/collections/{collection}/points"))
                .json(&json!({
                    "ids": [&first_chunk.point_id],
                    "with_payload": true,
                    "with_vector": false,
                })),
            &qdrant_api_key,
        )
        .await;
        assert_eq!(
            retrieved.pointer("/result/0/payload"),
            Some(&pubmed_payload(
                &literature,
                &first_chunk,
                &test_now().to_rfc3339()
            ))
        );

        let partial_abstract = format!("{}。{}", "a".repeat(1_100), "b".repeat(1_100));
        let (_partial_temp, partial_registry, partial_literature) =
            registry_literature_for("22345678", partial_abstract).await;
        let partial_chunks = build_pubmed_chunks(&partial_literature);
        assert_eq!(partial_chunks.len(), 2);
        seed_sandbox_point(
            &client,
            &qdrant_url,
            &collection,
            &partial_chunks[0].point_id,
            pubmed_payload(
                &partial_literature,
                &partial_chunks[0],
                &test_now().to_rfc3339(),
            ),
            &qdrant_api_key,
        )
        .await;
        let partial_config = PubmedVectorConfig::test_with_qdrant_api_key(
            &qdrant_url,
            &format!("{}/embed", embedding_server.uri()),
            &collection,
            &qdrant_api_key,
            true,
        );
        let partial_result = PubmedVectorIngestor::new(&client, partial_config)
            .ingest(&partial_registry, &partial_literature, None, test_now())
            .await;
        assert_eq!(
            partial_result,
            PubmedVectorResult {
                status: "complete".to_string(),
                expected_points: 2,
                verified_points: 2,
                existing_dataset: None,
                existing_document_id: None,
                verification_method: "deterministic_point_ids".to_string(),
                review_case_id: None,
                error: None,
            }
        );
        assert_eq!(
            sandbox_point_count(&client, &qdrant_url, &collection, &qdrant_api_key).await,
            3
        );

        let failure_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embed"))
            .respond_with(TemplateSequence {
                calls: AtomicUsize::new(0),
                templates: vec![
                    ResponseTemplate::new(500).set_body_string("sandbox embedding failure"),
                    ResponseTemplate::new(200).set_body_json(json!({"embedding": [0.1, 0.2, 0.3]})),
                ],
            })
            .expect(2)
            .mount(&failure_server)
            .await;
        let (_failure_temp, failure_registry, failure_literature) =
            registry_literature_for("32345678", "Failure recovery abstract.".to_string()).await;
        let failure_config = PubmedVectorConfig::test_with_qdrant_api_key(
            &qdrant_url,
            &format!("{}/embed", failure_server.uri()),
            &collection,
            &qdrant_api_key,
            true,
        );
        let failure_ingestor = PubmedVectorIngestor::new(&client, failure_config);
        let literature_id = failure_literature.literature_id.clone();
        let failed = failure_ingestor
            .ingest(&failure_registry, &failure_literature, None, test_now())
            .await;
        let recovered = failure_ingestor
            .ingest(&failure_registry, &failure_literature, None, test_now())
            .await;
        assert_eq!(
            (failed.status.as_str(), recovered.status.as_str()),
            ("failed", "complete")
        );
        assert_eq!(failure_literature.literature_id, literature_id);
        assert_eq!(
            sandbox_point_count(&client, &qdrant_url, &collection, &qdrant_api_key).await,
            4
        );

        let concurrency_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/embed"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"embedding": [0.1, 0.2, 0.3]})),
            )
            .expect(1)
            .mount(&concurrency_server)
            .await;
        let (concurrency_temp, first_registry, concurrent_literature) =
            registry_literature_for("42345678", "Concurrent abstract.".to_string()).await;
        let second_registry =
            LiteratureRegistry::open(concurrency_temp.path().join("literatures.sqlite3"))
                .await
                .expect("second registry pool");
        let concurrency_config = PubmedVectorConfig::test_with_qdrant_api_key(
            &qdrant_url,
            &format!("{}/embed", concurrency_server.uri()),
            &collection,
            &qdrant_api_key,
            true,
        );
        let first_ingestor = PubmedVectorIngestor::new(&client, concurrency_config.clone());
        let second_ingestor = PubmedVectorIngestor::new(&client, concurrency_config);
        let second_literature = concurrent_literature.clone();
        let (first_concurrent, second_concurrent) = tokio::join!(
            first_ingestor.ingest(&first_registry, &concurrent_literature, None, test_now()),
            second_ingestor.ingest(&second_registry, &second_literature, None, test_now()),
        );
        assert!(first_concurrent.status == "complete" || second_concurrent.status == "complete");
        assert_eq!(
            sandbox_point_count(&client, &qdrant_url, &collection, &qdrant_api_key).await,
            5
        );

        let (_local_temp, local_registry, local_literature) =
            registry_literature_for("52345678", "Local duplicate abstract.".to_string()).await;
        let local_point_id = uuid::Uuid::new_v4().to_string();
        seed_sandbox_point(
            &client,
            &qdrant_url,
            &collection,
            &local_point_id,
            json!({
                "document_id": "bio_literature:local:PMID-52345678",
                "paper_id": "PMID:52345678",
                "project_id": "data-extract-new",
                "source_uri": "https://pubmed.ncbi.nlm.nih.gov/52345678/",
            }),
            &qdrant_api_key,
        )
        .await;
        let local_duplicate_config = PubmedVectorConfig::test_with_qdrant_api_key(
            &qdrant_url,
            &format!("{}/embed", embedding_server.uri()),
            &collection,
            &qdrant_api_key,
            true,
        );
        let before_local_duplicate =
            sandbox_point_count(&client, &qdrant_url, &collection, &qdrant_api_key).await;
        let local_duplicate = PubmedVectorIngestor::new(&client, local_duplicate_config)
            .ingest(&local_registry, &local_literature, None, test_now())
            .await;
        assert_eq!(local_duplicate.status, "already_vectorized");
        assert_eq!(
            sandbox_point_count(&client, &qdrant_url, &collection, &qdrant_api_key).await,
            before_local_duplicate
        );

        sandbox_qdrant_json(
            client.delete(format!("{qdrant_url}/collections/{collection}")),
            &qdrant_api_key,
        )
        .await;
    }
}
