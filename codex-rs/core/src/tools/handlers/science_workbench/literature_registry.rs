//! Workspace-global literature identity and metadata registry.
//!
//! The registry deliberately lives outside individual research projects. Project
//! artifacts only retain `literature_id` values; this module owns normalization,
//! durable identity reuse, review cases, source-record bindings, and vector-job
//! coordination.

use anyhow::Context;
use anyhow::Result;
use serde_json::Value;
use serde_json::json;
use sqlx::Row;
use sqlx::Sqlite;
use sqlx::SqlitePool;
use sqlx::Transaction;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqliteJournalMode;
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

use super::literature_registry_identity::merge_json_missing;
use super::literature_registry_identity::metadata_with_defaults;
use super::literature_registry_identity::normalize_input;
use super::literature_registry_identity::normalize_title;
use super::literature_registry_identity::normalized_first_author;
use super::literature_registry_identity::publication_year;
use super::literature_registry_identity::should_replace_field;
use super::literature_registry_identity::title_jaccard;
use super::literature_registry_identity::update_field_source;
use super::literature_registry_schema::migrate_registry;

const REGISTRY_ENV: &str = "CODEX_MED_LITERATURE_DB";

#[derive(Clone)]
pub(crate) struct LiteratureRegistry {
    pub(super) pool: SqlitePool,
    path: PathBuf,
    pub(super) write_gate: Arc<tokio::sync::Semaphore>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct LiteratureInput {
    pub(super) pmid: Option<String>,
    pub(super) doi: Option<String>,
    pub(super) paper_id: Option<String>,
    pub(super) title: Option<String>,
    pub(super) abstract_text: Option<String>,
    pub(super) authors: Vec<String>,
    pub(super) journal: Option<String>,
    pub(super) publication_date: Option<String>,
    pub(super) metadata: Value,
}

#[derive(Debug, Clone)]
pub(super) struct SourceRecord {
    pub(super) system: String,
    pub(super) key: String,
    pub(super) match_method: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct RegisteredLiterature {
    pub(super) literature_id: String,
    pub(super) created: bool,
    pub(super) review_case_id: Option<String>,
    pub(super) match_method: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum RegistrationOutcome {
    Registered(RegisteredLiterature),
    Conflict {
        review_case_id: String,
        matched_literature_ids: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Literature {
    pub(super) literature_id: String,
    pub(super) pmid: Option<String>,
    pub(super) doi: Option<String>,
    pub(super) paper_id: Option<String>,
    pub(super) title: Option<String>,
    pub(super) abstract_text: Option<String>,
    pub(super) authors: Vec<String>,
    pub(super) journal: Option<String>,
    pub(super) publication_date: Option<String>,
    pub(super) metadata: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct VectorLease {
    pub(super) job_id: String,
    pub(super) owner_token: String,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum VectorLeaseOutcome {
    Acquired(VectorLease),
    NotAcquired { status: String },
}

impl LiteratureRegistry {
    pub(super) fn configured_path(workspace_root: &Path) -> PathBuf {
        std::env::var_os(REGISTRY_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                workspace_root
                    .join(".codex-med")
                    .join("literatures.sqlite3")
            })
    }

    pub(super) async fn open_workspace(workspace_root: &Path) -> Result<Self> {
        Self::open(Self::configured_path(workspace_root)).await
    }

    pub(super) async fn open(path: PathBuf) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create registry directory {}", parent.display())
            })?;
        }
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_millis(5_000));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .with_context(|| format!("failed to open literature registry {}", path.display()))?;
        let registry = Self {
            pool,
            path,
            write_gate: Arc::new(tokio::sync::Semaphore::new(1)),
        };
        migrate_registry(&registry.pool).await?;
        Ok(registry)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) async fn backup_to(&self, destination: &Path) -> Result<()> {
        anyhow::ensure!(
            !destination.exists(),
            "refusing to overwrite literature registry backup {}",
            destination.display()
        );
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create registry backup directory {}",
                    parent.display()
                )
            })?;
        }
        let _write_permit = self
            .write_gate
            .acquire()
            .await
            .context("literature registry write gate was closed")?;
        sqlx::query("VACUUM INTO ?")
            .bind(destination.to_string_lossy().to_string())
            .execute(&self.pool)
            .await
            .with_context(|| {
                format!(
                    "failed to create consistent literature registry backup {}",
                    destination.display()
                )
            })?;
        Ok(())
    }

    pub(super) async fn register(
        &self,
        input: LiteratureInput,
        source: Option<SourceRecord>,
        now: &str,
    ) -> Result<RegistrationOutcome> {
        let _write_permit = self
            .write_gate
            .acquire()
            .await
            .context("literature registry write gate was closed")?;
        let normalized = normalize_input(input);
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut matches = self.identifier_matches(&mut tx, &normalized).await?;
        let identifier_matched = !matches.is_empty();
        let source_match = if let Some(source) = source.as_ref() {
            sqlx::query_scalar::<_, String>(
                "SELECT literature_id FROM literature_source_records WHERE source_system = ? AND source_record_key = ?",
            )
            .bind(source.system.trim())
            .bind(source.key.trim())
            .fetch_optional(&mut *tx)
            .await?
        } else {
            None
        };
        let source_matched = source_match.is_some();
        if let Some(source_match) = source_match {
            matches.insert(self.resolve_alias_in(&mut tx, &source_match).await?);
        }
        if matches.len() > 1 {
            let matched_literature_ids = matches.into_iter().collect::<Vec<_>>();
            let review_case_id = insert_review_case(
                &mut tx,
                "identifier_conflict",
                &normalized,
                &matched_literature_ids,
                None,
                now,
            )
            .await?;
            tx.commit().await?;
            return Ok(RegistrationOutcome::Conflict {
                review_case_id,
                matched_literature_ids,
            });
        }

        let mut created = false;
        let literature_id = if let Some(existing) = matches.into_iter().next() {
            existing
        } else {
            let candidate_id = Uuid::new_v4().to_string();
            let inserted = sqlx::query(
                r#"
                INSERT OR IGNORE INTO literatures(
                    literature_id, pmid, doi, paper_id, title, abstract,
                    authors_json, journal, publication_date, metadata_json,
                    created_at, updated_at
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
            )
            .bind(&candidate_id)
            .bind(&normalized.pmid)
            .bind(&normalized.doi)
            .bind(&normalized.paper_id)
            .bind(&normalized.title)
            .bind(&normalized.abstract_text)
            .bind(serde_json::to_string(&normalized.authors)?)
            .bind(&normalized.journal)
            .bind(&normalized.publication_date)
            .bind(serde_json::to_string(&metadata_with_defaults(
                normalized.metadata.clone(),
            ))?)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            if inserted.rows_affected() == 1 {
                created = true;
                candidate_id
            } else {
                let concurrent = self.identifier_matches(&mut tx, &normalized).await?;
                anyhow::ensure!(
                    concurrent.len() == 1,
                    "concurrent literature registration could not resolve a single canonical record"
                );
                concurrent.into_iter().next().ok_or_else(|| {
                    anyhow::anyhow!(
                        "concurrent literature registration did not return a canonical record"
                    )
                })?
            }
        };

        if !created
            && self
                .has_identifier_conflict(&mut tx, &literature_id, &normalized)
                .await?
        {
            let matched_literature_ids = vec![literature_id];
            let review_case_id = insert_review_case(
                &mut tx,
                "identifier_conflict",
                &normalized,
                &matched_literature_ids,
                None,
                now,
            )
            .await?;
            tx.commit().await?;
            return Ok(RegistrationOutcome::Conflict {
                review_case_id,
                matched_literature_ids,
            });
        }

        self.merge_metadata(&mut tx, &literature_id, &normalized, now)
            .await?;

        if let Some(source) = source.as_ref() {
            bind_source_record(&mut tx, source, &literature_id, now).await?;
            let bound: String = sqlx::query_scalar(
                "SELECT literature_id FROM literature_source_records WHERE source_system = ? AND source_record_key = ?",
            )
            .bind(source.system.trim())
            .bind(source.key.trim())
            .fetch_one(&mut *tx)
            .await?;
            anyhow::ensure!(
                bound == literature_id,
                "source record was concurrently bound to a different literature"
            );
        }

        let review_case_id = if created {
            self.possible_duplicate_case(&mut tx, &literature_id, &normalized, now)
                .await?
        } else {
            self.pending_possible_duplicate_case(&mut tx, &literature_id)
                .await?
        };
        tx.commit().await?;
        let match_method = if created {
            if review_case_id.is_some() {
                "possible_duplicate_candidate"
            } else {
                "new_literature"
            }
        } else if identifier_matched {
            "strong_identifier"
        } else if source_matched {
            "source_record"
        } else {
            "concurrent_unique_constraint"
        };
        Ok(RegistrationOutcome::Registered(RegisteredLiterature {
            literature_id,
            created,
            review_case_id,
            match_method: match_method.to_string(),
        }))
    }

    async fn identifier_matches(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        input: &LiteratureInput,
    ) -> Result<BTreeSet<String>> {
        let mut matches = BTreeSet::new();
        for (column, value) in [
            ("pmid", input.pmid.as_deref()),
            ("doi", input.doi.as_deref()),
            ("paper_id", input.paper_id.as_deref()),
        ] {
            let Some(value) = value else {
                continue;
            };
            let query = format!("SELECT literature_id FROM literatures WHERE {column} = ?");
            if let Some(id) = sqlx::query_scalar::<_, String>(&query)
                .bind(value)
                .fetch_optional(&mut **tx)
                .await?
            {
                matches.insert(self.resolve_alias_in(tx, &id).await?);
            }
        }
        Ok(matches)
    }

    async fn has_identifier_conflict(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        literature_id: &str,
        input: &LiteratureInput,
    ) -> Result<bool> {
        let row =
            sqlx::query("SELECT pmid, doi, paper_id FROM literatures WHERE literature_id = ?")
                .bind(literature_id)
                .fetch_one(&mut **tx)
                .await?;
        for (column, incoming) in [
            ("pmid", input.pmid.as_deref()),
            ("doi", input.doi.as_deref()),
            ("paper_id", input.paper_id.as_deref()),
        ] {
            let existing: Option<String> = row.try_get(column)?;
            if existing
                .as_deref()
                .zip(incoming)
                .is_some_and(|(left, right)| left != right)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) async fn merge_metadata(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        literature_id: &str,
        input: &LiteratureInput,
        now: &str,
    ) -> Result<()> {
        let row = sqlx::query(
            "SELECT pmid, doi, paper_id, title, abstract, authors_json, journal, publication_date, metadata_json FROM literatures WHERE literature_id = ?",
        )
        .bind(literature_id)
        .fetch_one(&mut **tx)
        .await?;
        for (name, existing, incoming) in [
            (
                "pmid",
                row.try_get::<Option<String>, _>("pmid")?,
                input.pmid.clone(),
            ),
            (
                "doi",
                row.try_get::<Option<String>, _>("doi")?,
                input.doi.clone(),
            ),
            (
                "paper_id",
                row.try_get::<Option<String>, _>("paper_id")?,
                input.paper_id.clone(),
            ),
        ] {
            anyhow::ensure!(
                existing.is_none() || incoming.is_none() || existing == incoming,
                "incoming {name} conflicts with the canonical literature"
            );
        }
        let existing_metadata: String = row.try_get("metadata_json")?;
        let existing_metadata: Value = serde_json::from_str(&existing_metadata)
            .unwrap_or_else(|_| metadata_with_defaults(json!({})));
        let existing_title: Option<String> = row.try_get("title")?;
        let existing_abstract: Option<String> = row.try_get("abstract")?;
        let existing_authors_json: String = row.try_get("authors_json")?;
        let existing_authors: Vec<String> =
            serde_json::from_str(&existing_authors_json).unwrap_or_default();
        let existing_journal: Option<String> = row.try_get("journal")?;
        let existing_publication_date: Option<String> = row.try_get("publication_date")?;
        let replace_title = should_replace_field(
            &existing_metadata,
            &input.metadata,
            "title",
            existing_title
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            input
                .title
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
        );
        let replace_abstract = should_replace_field(
            &existing_metadata,
            &input.metadata,
            "abstract",
            existing_abstract
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            input
                .abstract_text
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
        );
        let replace_authors = should_replace_field(
            &existing_metadata,
            &input.metadata,
            "authors",
            !existing_authors.is_empty(),
            !input.authors.is_empty(),
        );
        let replace_journal = should_replace_field(
            &existing_metadata,
            &input.metadata,
            "journal",
            existing_journal
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            input
                .journal
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
        );
        let replace_publication_date = should_replace_field(
            &existing_metadata,
            &input.metadata,
            "publication_date",
            existing_publication_date
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
            input
                .publication_date
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty()),
        );
        let mut metadata = existing_metadata;
        merge_json_missing(&mut metadata, input.metadata.clone());
        for (field, replace) in [
            ("title", replace_title),
            ("abstract", replace_abstract),
            ("authors", replace_authors),
            ("journal", replace_journal),
            ("publication_date", replace_publication_date),
        ] {
            if replace {
                update_field_source(&mut metadata, &input.metadata, field);
            }
        }
        for field in ["mesh_terms", "publication_types", "language", "relations"] {
            if should_replace_metadata_field(&metadata, &input.metadata, field) {
                if let Some(incoming) = input.metadata.get(field).cloned()
                    && let Some(object) = metadata.as_object_mut()
                {
                    object.insert(field.to_string(), incoming);
                }
                update_field_source(&mut metadata, &input.metadata, field);
                if field == "publication_types" {
                    copy_metadata_flag(&mut metadata, &input.metadata, "retracted");
                    copy_metadata_flag(&mut metadata, &input.metadata, "corrected");
                }
            }
        }
        if replace_abstract {
            copy_metadata_flag(&mut metadata, &input.metadata, "abstract_missing");
        }
        metadata = metadata_with_defaults(metadata);

        sqlx::query(
            r#"
            UPDATE literatures SET
                pmid = COALESCE(pmid, ?),
                doi = COALESCE(doi, ?),
                paper_id = COALESCE(paper_id, ?),
                title = CASE WHEN ? THEN ? ELSE title END,
                abstract = CASE WHEN ? THEN ? ELSE abstract END,
                authors_json = CASE WHEN ? THEN ? ELSE authors_json END,
                journal = CASE WHEN ? THEN ? ELSE journal END,
                publication_date = CASE WHEN ? THEN ? ELSE publication_date END,
                metadata_json = ?,
                updated_at = ?
            WHERE literature_id = ?
            "#,
        )
        .bind(&input.pmid)
        .bind(&input.doi)
        .bind(&input.paper_id)
        .bind(replace_title)
        .bind(&input.title)
        .bind(replace_abstract)
        .bind(&input.abstract_text)
        .bind(replace_authors)
        .bind(serde_json::to_string(&input.authors)?)
        .bind(replace_journal)
        .bind(&input.journal)
        .bind(replace_publication_date)
        .bind(&input.publication_date)
        .bind(serde_json::to_string(&metadata)?)
        .bind(now)
        .bind(literature_id)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn possible_duplicate_case(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        literature_id: &str,
        input: &LiteratureInput,
        now: &str,
    ) -> Result<Option<String>> {
        let Some(title) = input.title.as_deref() else {
            return Ok(None);
        };
        let normalized_title = normalize_title(title);
        if normalized_title.is_empty() {
            return Ok(None);
        }
        let rows = sqlx::query(
            r#"
            SELECT literature_id, title, authors_json, publication_date
            FROM literatures
            WHERE literature_id != ? AND title IS NOT NULL
            "#,
        )
        .bind(literature_id)
        .fetch_all(&mut **tx)
        .await?;
        let mut candidates = Vec::new();
        let mut evaluations = Vec::new();
        let incoming_author = normalized_first_author(&input.authors);
        let incoming_year = publication_year(input.publication_date.as_deref());
        for row in rows {
            let candidate_id: String = row.try_get("literature_id")?;
            let candidate_title: String = row.try_get("title")?;
            let candidate_authors: Vec<String> =
                serde_json::from_str(&row.try_get::<String, _>("authors_json")?)
                    .unwrap_or_default();
            let candidate_author = normalized_first_author(&candidate_authors);
            let candidate_date: Option<String> = row.try_get("publication_date")?;
            let candidate_year = publication_year(candidate_date.as_deref());
            let jaccard = title_jaccard(title, &candidate_title);
            let author_conflict = incoming_author.is_some()
                && candidate_author.is_some()
                && incoming_author != candidate_author;
            let year_conflict = incoming_year
                .zip(candidate_year)
                .is_some_and(|(left, right)| (left - right).abs() > 1);
            let exact_title = normalize_title(&candidate_title) == normalized_title;
            let exact_rule = exact_title && !author_conflict && !year_conflict;
            let fuzzy_rule = jaccard >= 0.90
                && incoming_author.is_some()
                && incoming_author == candidate_author
                && incoming_year
                    .zip(candidate_year)
                    .is_some_and(|(left, right)| (left - right).abs() <= 1);
            if exact_rule || fuzzy_rule {
                candidates.push(candidate_id.clone());
                evaluations.push(json!({
                    "literature_id": candidate_id,
                    "normalized_title": normalize_title(&candidate_title),
                    "title_jaccard": jaccard,
                    "first_author": candidate_author,
                    "publication_year": candidate_year,
                    "matched_rule": if exact_rule { "exact_title_no_conflict" } else { "jaccard_author_year" },
                }));
            }
        }
        if candidates.is_empty() {
            return Ok(None);
        }
        let review_case_id = insert_review_case(
            tx,
            "possible_duplicate",
            input,
            &candidates,
            Some(json!({
                "candidate_rule_version": "v1",
                "incoming_literature_id": literature_id,
                "incoming_normalized_title": normalized_title,
                "incoming_first_author": incoming_author,
                "incoming_publication_year": incoming_year,
                "evaluations": evaluations,
            })),
            now,
        )
        .await?;
        Ok(Some(review_case_id))
    }

    async fn pending_possible_duplicate_case(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        literature_id: &str,
    ) -> Result<Option<String>> {
        let rows = sqlx::query(
            r#"
            SELECT review_case_id, incoming_metadata_json
            FROM literature_review_cases
            WHERE conflict_type = 'possible_duplicate' AND status = 'pending'
            "#,
        )
        .fetch_all(&mut **tx)
        .await?;
        for row in rows {
            let metadata: String = row.try_get("incoming_metadata_json")?;
            let metadata: Value = serde_json::from_str(&metadata).unwrap_or(Value::Null);
            if metadata
                .pointer("/candidate_evaluation/incoming_literature_id")
                .and_then(Value::as_str)
                == Some(literature_id)
            {
                return Ok(Some(row.try_get("review_case_id")?));
            }
        }
        Ok(None)
    }

    pub(super) async fn resolve_alias_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        literature_id: &str,
    ) -> Result<String> {
        let mut current = literature_id.to_string();
        for _ in 0..16 {
            let canonical = sqlx::query_scalar::<_, String>(
                "SELECT canonical_literature_id FROM literature_id_aliases WHERE alias_literature_id = ?",
            )
            .bind(&current)
            .fetch_optional(&mut **tx)
            .await?;
            let Some(canonical) = canonical else {
                return Ok(current);
            };
            current = canonical;
        }
        anyhow::bail!("literature alias chain is unexpectedly deep or cyclic")
    }

    pub(super) async fn resolve_literature_id(&self, literature_id: &str) -> Result<String> {
        let mut tx = self.pool.begin().await?;
        let canonical = self.resolve_alias_in(&mut tx, literature_id).await?;
        tx.commit().await?;
        Ok(canonical)
    }

    pub(super) async fn get(&self, literature_id: &str) -> Result<Option<Literature>> {
        let canonical = self.resolve_literature_id(literature_id).await?;
        let row = sqlx::query(
            r#"
            SELECT literature_id, pmid, doi, paper_id, title, abstract,
                   authors_json, journal, publication_date, metadata_json
            FROM literatures WHERE literature_id = ?
            "#,
        )
        .bind(canonical)
        .fetch_optional(&self.pool)
        .await?;
        row.map(row_to_literature).transpose()
    }

    pub(super) async fn source_record_literature_id(
        &self,
        source_system: &str,
        source_record_key: &str,
    ) -> Result<Option<String>> {
        let id = sqlx::query_scalar::<_, String>(
            r#"
            SELECT literature_id
            FROM literature_source_records
            WHERE source_system = ? AND source_record_key = ?
            "#,
        )
        .bind(source_system)
        .bind(source_record_key)
        .fetch_optional(&self.pool)
        .await?;
        match id {
            Some(id) => Ok(Some(self.resolve_literature_id(&id).await?)),
            None => Ok(None),
        }
    }

    pub(super) async fn create_qdrant_possible_duplicate_case(
        &self,
        literature: &Literature,
        collection_name: &str,
        candidate: Value,
        now: &str,
    ) -> Result<String> {
        let _write_permit = self
            .write_gate
            .acquire()
            .await
            .context("literature registry write gate was closed")?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let source_system = format!("qdrant:{collection_name}");
        let candidate_literature_ids =
            if let Some(document_id) = candidate.get("document_id").and_then(Value::as_str) {
                match sqlx::query_scalar::<_, String>(
                    r#"
                SELECT literature_id
                FROM literature_source_records
                WHERE source_system = ? AND source_record_key = ?
                "#,
                )
                .bind(&source_system)
                .bind(document_id)
                .fetch_optional(&mut *tx)
                .await?
                {
                    Some(id) => vec![self.resolve_alias_in(&mut tx, &id).await?],
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            };
        let input = LiteratureInput {
            pmid: literature.pmid.clone(),
            doi: literature.doi.clone(),
            paper_id: literature.paper_id.clone(),
            title: literature.title.clone(),
            abstract_text: literature.abstract_text.clone(),
            authors: literature.authors.clone(),
            journal: literature.journal.clone(),
            publication_date: literature.publication_date.clone(),
            metadata: literature.metadata.clone(),
        };
        let review_case_id = insert_review_case(
            &mut tx,
            "possible_duplicate",
            &input,
            &candidate_literature_ids,
            Some(json!({
                "candidate_rule_version": "qdrant-v1",
                "incoming_literature_id": literature.literature_id,
                "collection": collection_name,
                "candidate": candidate,
            })),
            now,
        )
        .await?;
        tx.commit().await?;
        Ok(review_case_id)
    }

    pub(super) async fn qdrant_candidate_was_confirmed_different(
        &self,
        literature_id: &str,
        collection_name: &str,
        candidate: &Value,
    ) -> Result<bool> {
        let rows = sqlx::query_scalar::<_, String>(
            r#"
            SELECT incoming_metadata_json
            FROM literature_review_cases
            WHERE conflict_type = 'possible_duplicate'
              AND status = 'resolved_different_literatures'
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let candidate_point = candidate.get("point_id");
        let candidate_document = candidate.get("document_id");
        Ok(rows.into_iter().any(|row| {
            let Ok(value) = serde_json::from_str::<Value>(&row) else {
                return false;
            };
            let evaluation = value.pointer("/candidate_evaluation");
            evaluation
                .and_then(|value| value.get("incoming_literature_id"))
                .and_then(Value::as_str)
                == Some(literature_id)
                && evaluation
                    .and_then(|value| value.get("collection"))
                    .and_then(Value::as_str)
                    == Some(collection_name)
                && ((candidate_point.is_some()
                    && evaluation.and_then(|value| value.pointer("/candidate/point_id"))
                        == candidate_point)
                    || (candidate_document.is_some()
                        && evaluation.and_then(|value| value.pointer("/candidate/document_id"))
                            == candidate_document))
        }))
    }

    pub(super) async fn has_validated_qdrant_initialization(
        &self,
        collection_name: &str,
    ) -> Result<bool> {
        let source_system = format!("qdrant:{collection_name}");
        let details = sqlx::query_scalar::<_, String>(
            "SELECT details_json FROM literature_registry_initializations WHERE source_system = ? AND status = 'complete'",
        )
        .bind(source_system)
        .fetch_optional(&self.pool)
        .await?;
        Ok(details
            .as_deref()
            .and_then(|details| serde_json::from_str::<Value>(details).ok())
            .and_then(|details| {
                details
                    .pointer("/validation/passed")
                    .and_then(Value::as_bool)
            })
            == Some(true))
    }

    #[cfg(test)]
    async fn literature_count(&self) -> Result<i64> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM literatures")
            .fetch_one(&self.pool)
            .await?)
    }
}

fn should_replace_metadata_field(
    existing_metadata: &Value,
    incoming_metadata: &Value,
    field: &str,
) -> bool {
    let present = |value: Option<&Value>| match value {
        Some(Value::Null) | None => false,
        Some(Value::String(value)) => !value.trim().is_empty(),
        Some(Value::Array(values)) => !values.is_empty(),
        Some(Value::Object(values)) => !values.is_empty(),
        Some(_) => true,
    };
    should_replace_field(
        existing_metadata,
        incoming_metadata,
        field,
        present(existing_metadata.get(field)),
        present(incoming_metadata.get(field)),
    )
}

fn copy_metadata_flag(metadata: &mut Value, incoming_metadata: &Value, flag: &str) {
    let Some(incoming) = incoming_metadata
        .pointer(&format!("/flags/{flag}"))
        .cloned()
    else {
        return;
    };
    if let Some(flags) = metadata.get_mut("flags").and_then(Value::as_object_mut) {
        flags.insert(flag.to_string(), incoming);
    }
}

fn row_to_literature(row: sqlx::sqlite::SqliteRow) -> Result<Literature> {
    let authors_json: String = row.try_get("authors_json")?;
    let metadata_json: String = row.try_get("metadata_json")?;
    Ok(Literature {
        literature_id: row.try_get("literature_id")?,
        pmid: row.try_get("pmid")?,
        doi: row.try_get("doi")?,
        paper_id: row.try_get("paper_id")?,
        title: row.try_get("title")?,
        abstract_text: row.try_get("abstract")?,
        authors: serde_json::from_str(&authors_json).unwrap_or_default(),
        journal: row.try_get("journal")?,
        publication_date: row.try_get("publication_date")?,
        metadata: serde_json::from_str(&metadata_json).unwrap_or_else(|_| json!({})),
    })
}

async fn bind_source_record(
    tx: &mut Transaction<'_, Sqlite>,
    source: &SourceRecord,
    literature_id: &str,
    now: &str,
) -> Result<()> {
    let system = source.system.trim();
    let key = source.key.trim();
    anyhow::ensure!(!system.is_empty(), "source system must not be empty");
    anyhow::ensure!(!key.is_empty(), "source record key must not be empty");
    sqlx::query(
        r#"
        INSERT OR IGNORE INTO literature_source_records(
            source_system, source_record_key, literature_id, match_method,
            created_at, updated_at
        ) VALUES (?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(system)
    .bind(key)
    .bind(literature_id)
    .bind(source.match_method.trim())
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_review_case(
    tx: &mut Transaction<'_, Sqlite>,
    conflict_type: &str,
    input: &LiteratureInput,
    matched_literature_ids: &[String],
    candidate_evaluation: Option<Value>,
    now: &str,
) -> Result<String> {
    let review_case_id = Uuid::new_v4().to_string();
    sqlx::query(
        r#"
        INSERT INTO literature_review_cases(
            review_case_id, conflict_type, incoming_identifiers_json,
            matched_literature_ids_json, incoming_metadata_json, status,
            created_at
        ) VALUES (?, ?, ?, ?, ?, 'pending', ?)
        "#,
    )
    .bind(&review_case_id)
    .bind(conflict_type)
    .bind(serde_json::to_string(&json!({
        "pmid": input.pmid,
        "doi": input.doi,
        "paper_id": input.paper_id,
    }))?)
    .bind(serde_json::to_string(matched_literature_ids)?)
    .bind(serde_json::to_string(&json!({
        "title": input.title,
        "authors": input.authors,
        "journal": input.journal,
        "publication_date": input.publication_date,
        "metadata": input.metadata,
        "candidate_evaluation": candidate_evaluation,
    }))?)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    Ok(review_case_id)
}

#[cfg(test)]
mod tests {
    use super::super::literature_registry_identity::identifiers_from_source_uri;
    use super::super::literature_registry_identity::normalize_doi;
    use super::super::literature_registry_identity::normalize_paper_id;
    use super::super::literature_registry_identity::normalize_pmid;
    use super::super::literature_registry_identity::paper_id_from_source_uri;
    use super::*;
    use pretty_assertions::assert_eq;

    async fn registry() -> (tempfile::TempDir, LiteratureRegistry) {
        let temp = tempfile::tempdir().expect("temp dir");
        let registry = LiteratureRegistry::open(temp.path().join("literatures.sqlite3"))
            .await
            .expect("open registry");
        (temp, registry)
    }

    fn input(pmid: &str, title: &str) -> LiteratureInput {
        LiteratureInput {
            pmid: Some(pmid.to_string()),
            title: Some(title.to_string()),
            metadata: json!({}),
            ..Default::default()
        }
    }

    #[test]
    fn normalizes_strong_identifiers() {
        assert_eq!(normalize_pmid(" PMID: 001234 "), Some("1234".to_string()));
        assert_eq!(
            normalize_pmid("https://pubmed.ncbi.nlm.nih.gov/12345678/"),
            Some("12345678".to_string())
        );
        assert_eq!(
            normalize_doi("https://doi.org/10.1000/ABC.123"),
            Some("10.1000/abc.123".to_string())
        );
        assert_eq!(
            normalize_doi("https://doi.org/10.1000%2FABC.123"),
            Some("10.1000/abc.123".to_string())
        );
        assert_eq!(
            identifiers_from_source_uri(
                "https://pubmed.ncbi.nlm.nih.gov/12345678/?utm_source=test"
            )
            .0,
            Some("12345678".to_string())
        );
        assert_eq!(
            normalize_paper_id("ep 0323806 a1"),
            Some("EP0323806A1".to_string())
        );
        assert_eq!(
            paper_id_from_source_uri(
                "file:///home/wysi/data/data-extract-new/EP0739981A1/ocr_repaired.md"
            ),
            Some("EP0739981A1".to_string())
        );
        assert_eq!(normalize_pmid("PMID:none"), None);
        assert_eq!(normalize_doi("not-a-doi"), None);
    }

    #[tokio::test]
    async fn reuses_one_identity_for_repeated_pmid() {
        let (_temp, registry) = registry().await;
        let first = registry
            .register(
                input("PMID:123", "First title"),
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("first registration");
        let second = registry
            .register(
                input("https://pubmed.ncbi.nlm.nih.gov/123/", "Second title"),
                None,
                "2026-01-02T00:00:00Z",
            )
            .await
            .expect("second registration");
        let RegistrationOutcome::Registered(first) = first else {
            panic!("expected registered literature");
        };
        let RegistrationOutcome::Registered(second) = second else {
            panic!("expected registered literature");
        };
        assert!(first.created);
        assert!(!second.created);
        assert_eq!(first.literature_id, second.literature_id);
        assert_eq!(registry.literature_count().await.expect("count"), 1);
    }

    #[tokio::test]
    async fn concurrent_repeated_registration_returns_one_identity() {
        let (_temp, registry) = registry().await;
        let registrations = (0..16).map(|_| {
            let registry = registry.clone();
            async move {
                registry
                    .register(
                        input("789", "Concurrent title"),
                        None,
                        "2026-01-01T00:00:00Z",
                    )
                    .await
                    .expect("registration")
            }
        });
        let outcomes = futures::future::join_all(registrations).await;
        let ids = outcomes
            .into_iter()
            .map(|outcome| match outcome {
                RegistrationOutcome::Registered(registered) => registered.literature_id,
                RegistrationOutcome::Conflict { .. } => panic!("unexpected conflict"),
            })
            .collect::<BTreeSet<_>>();

        assert_eq!(ids.len(), 1);
        assert_eq!(registry.literature_count().await.expect("count"), 1);
    }

    #[tokio::test]
    async fn independent_registry_pools_concurrently_return_one_identity() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("literatures.sqlite3");
        let first_registry = LiteratureRegistry::open(path.clone())
            .await
            .expect("first registry");
        let second_registry = LiteratureRegistry::open(path)
            .await
            .expect("second registry");
        let (first, second) = tokio::join!(
            first_registry.register(
                input("790", "Cross-pool title"),
                None,
                "2026-01-01T00:00:00Z"
            ),
            second_registry.register(
                input("790", "Cross-pool title"),
                None,
                "2026-01-01T00:00:00Z"
            ),
        );
        let ids = [first.expect("first"), second.expect("second")]
            .into_iter()
            .map(|outcome| match outcome {
                RegistrationOutcome::Registered(registered) => registered.literature_id,
                RegistrationOutcome::Conflict { .. } => panic!("unexpected conflict"),
            })
            .collect::<BTreeSet<_>>();

        assert_eq!(ids.len(), 1);
        assert_eq!(first_registry.literature_count().await.expect("count"), 1);
    }

    #[tokio::test]
    async fn creates_consistent_sqlite_backup_without_overwriting() {
        let (temp, registry) = registry().await;
        registry
            .register(input("800", "Backed up"), None, "2026-01-01T00:00:00Z")
            .await
            .expect("register");
        let backup = temp.path().join("backups").join("literatures.sqlite3");
        registry.backup_to(&backup).await.expect("backup");
        let backup_registry = LiteratureRegistry::open(backup.clone())
            .await
            .expect("open backup");

        assert_eq!(backup_registry.literature_count().await.expect("count"), 1);
        assert!(registry.backup_to(&backup).await.is_err());
    }

    #[tokio::test]
    async fn pubmed_metadata_replaces_lower_priority_title_but_older_fetch_does_not() {
        let (_temp, registry) = registry().await;
        registry
            .register(
                LiteratureInput {
                    pmid: Some("900".to_string()),
                    title: Some("Local OCR title".to_string()),
                    metadata: json!({
                        "field_sources": {
                            "title": {
                                "source": "qdrant:test",
                                "observed_at": "2026-01-01T00:00:00Z"
                            }
                        }
                    }),
                    ..Default::default()
                },
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("local registration");
        let RegistrationOutcome::Registered(pubmed) = registry
            .register(
                LiteratureInput {
                    pmid: Some("900".to_string()),
                    title: Some("Authoritative PubMed title".to_string()),
                    metadata: json!({
                        "field_sources": {
                            "title": {
                                "source": "pubmed",
                                "observed_at": "2026-02-01T00:00:00Z"
                            }
                        }
                    }),
                    ..Default::default()
                },
                None,
                "2026-02-01T00:00:00Z",
            )
            .await
            .expect("PubMed registration")
        else {
            panic!("expected registration");
        };
        registry
            .register(
                LiteratureInput {
                    pmid: Some("900".to_string()),
                    title: Some("Stale PubMed title".to_string()),
                    metadata: json!({
                        "field_sources": {
                            "title": {
                                "source": "pubmed",
                                "observed_at": "2026-01-15T00:00:00Z"
                            }
                        }
                    }),
                    ..Default::default()
                },
                None,
                "2026-02-02T00:00:00Z",
            )
            .await
            .expect("stale registration");

        assert_eq!(
            registry
                .get(&pubmed.literature_id)
                .await
                .expect("get")
                .expect("literature")
                .title
                .as_deref(),
            Some("Authoritative PubMed title")
        );
    }

    #[tokio::test]
    async fn source_record_reuses_identity_without_strong_identifier() {
        let (_temp, registry) = registry().await;
        let source = SourceRecord {
            system: "qdrant:test".to_string(),
            key: "doc-1".to_string(),
            match_method: "source_record".to_string(),
        };
        let first = registry
            .register(
                LiteratureInput {
                    title: Some("Unidentified local paper".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(source.clone()),
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("first registration");
        let second = registry
            .register(
                LiteratureInput {
                    title: Some("Unidentified local paper".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(source),
                "2026-01-02T00:00:00Z",
            )
            .await
            .expect("second registration");
        let RegistrationOutcome::Registered(first) = first else {
            panic!("expected first registration");
        };
        let RegistrationOutcome::Registered(second) = second else {
            panic!("expected second registration");
        };
        assert_eq!(first.literature_id, second.literature_id);
        assert_eq!(registry.literature_count().await.expect("count"), 1);
    }

    #[tokio::test]
    async fn records_conflicting_strong_identifiers_for_review() {
        let (_temp, registry) = registry().await;
        let RegistrationOutcome::Registered(first) = registry
            .register(input("1", "One"), None, "2026-01-01T00:00:00Z")
            .await
            .expect("register first")
        else {
            panic!("expected first registration");
        };
        let RegistrationOutcome::Registered(second) = registry
            .register(
                LiteratureInput {
                    doi: Some("10.1000/two".to_string()),
                    title: Some("Two".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register second")
        else {
            panic!("expected second registration");
        };
        let outcome = registry
            .register(
                LiteratureInput {
                    pmid: Some("1".to_string()),
                    doi: Some("10.1000/two".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                None,
                "2026-01-02T00:00:00Z",
            )
            .await
            .expect("register conflict");
        let RegistrationOutcome::Conflict {
            matched_literature_ids,
            ..
        } = outcome
        else {
            panic!("expected conflict");
        };
        let mut expected = vec![first.literature_id, second.literature_id];
        expected.sort();
        assert_eq!(matched_literature_ids, expected);
        assert_eq!(registry.literature_count().await.expect("count"), 2);
    }

    #[tokio::test]
    async fn records_conflict_when_matched_record_has_different_identifier_value() {
        let (_temp, registry) = registry().await;
        registry
            .register(
                LiteratureInput {
                    pmid: Some("42".to_string()),
                    doi: Some("10.1000/original".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register original");
        let outcome = registry
            .register(
                LiteratureInput {
                    pmid: Some("42".to_string()),
                    doi: Some("10.1000/different".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                None,
                "2026-01-02T00:00:00Z",
            )
            .await
            .expect("register conflict");

        assert!(matches!(outcome, RegistrationOutcome::Conflict { .. }));
        assert_eq!(registry.literature_count().await.expect("count"), 1);
    }

    #[tokio::test]
    async fn creates_review_case_for_exact_weak_duplicate() {
        let (_temp, registry) = registry().await;
        registry
            .register(
                LiteratureInput {
                    title: Some("A candidate title".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(SourceRecord {
                    system: "qdrant:test".to_string(),
                    key: "doc-1".to_string(),
                    match_method: "source_record".to_string(),
                }),
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register first");
        let RegistrationOutcome::Registered(second) = registry
            .register(
                LiteratureInput {
                    title: Some("A candidate—title!".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(SourceRecord {
                    system: "qdrant:test".to_string(),
                    key: "doc-2".to_string(),
                    match_method: "source_record".to_string(),
                }),
                "2026-01-02T00:00:00Z",
            )
            .await
            .expect("register second")
        else {
            panic!("expected registration");
        };
        assert!(second.created);
        assert!(second.review_case_id.is_some());
        let RegistrationOutcome::Registered(rerun) = registry
            .register(
                LiteratureInput {
                    title: Some("A candidate—title!".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(SourceRecord {
                    system: "qdrant:test".to_string(),
                    key: "doc-2".to_string(),
                    match_method: "source_record".to_string(),
                }),
                "2026-01-03T00:00:00Z",
            )
            .await
            .expect("rerun second")
        else {
            panic!("expected rerun registration");
        };
        assert_eq!(rerun.review_case_id, second.review_case_id);
        assert_eq!(registry.literature_count().await.expect("count"), 2);
    }

    #[tokio::test]
    async fn creates_review_case_for_high_jaccard_author_year_candidate() {
        let (_temp, registry) = registry().await;
        let candidate = |key: &str, title: &str| {
            (
                LiteratureInput {
                    title: Some(title.to_string()),
                    authors: vec!["Ada Lovelace".to_string()],
                    publication_date: Some("2025".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                SourceRecord {
                    system: "qdrant:test".to_string(),
                    key: key.to_string(),
                    match_method: "source_record".to_string(),
                },
            )
        };
        let (first, first_source) =
            candidate("doc-1", "one two three four five six seven eight nine ten");
        registry
            .register(first, Some(first_source), "2026-01-01T00:00:00Z")
            .await
            .expect("register first");
        let (second, second_source) = candidate(
            "doc-2",
            "one two three four five six seven eight nine ten eleven",
        );
        let RegistrationOutcome::Registered(second) = registry
            .register(second, Some(second_source), "2026-01-02T00:00:00Z")
            .await
            .expect("register second")
        else {
            panic!("expected registration");
        };

        assert!(second.review_case_id.is_some());
        assert_eq!(registry.literature_count().await.expect("count"), 2);
    }

    #[tokio::test]
    async fn resolving_candidate_as_different_releases_future_vector_work() {
        let (_temp, registry) = registry().await;
        let source = |key: &str| SourceRecord {
            system: "qdrant:test".to_string(),
            key: key.to_string(),
            match_method: "source_record".to_string(),
        };
        registry
            .register(
                LiteratureInput {
                    title: Some("Shared title".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(source("doc-1")),
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("first");
        let RegistrationOutcome::Registered(candidate) = registry
            .register(
                LiteratureInput {
                    title: Some("Shared title".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(source("doc-2")),
                "2026-01-02T00:00:00Z",
            )
            .await
            .expect("candidate")
        else {
            panic!("expected registration");
        };
        let review_case_id = candidate.review_case_id.expect("review case");
        registry
            .record_vector_status(
                &candidate.literature_id,
                "collection",
                "profile",
                "possible_duplicate",
                None,
                Some(&review_case_id),
                None,
                "2026-01-02T00:00:00Z",
            )
            .await
            .expect("blocked job");
        registry
            .resolve_review_as_different(
                &review_case_id,
                "reviewed source records",
                "2026-01-03T00:00:00Z",
            )
            .await
            .expect("resolve");
        let RegistrationOutcome::Registered(rerun) = registry
            .register(
                LiteratureInput {
                    title: Some("Shared title".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                Some(source("doc-2")),
                "2026-01-04T00:00:00Z",
            )
            .await
            .expect("rerun")
        else {
            panic!("expected registration");
        };

        assert_eq!(rerun.literature_id, candidate.literature_id);
        assert_eq!(rerun.review_case_id, None);
        assert!(matches!(
            registry
                .claim_vector_job(
                    &candidate.literature_id,
                    "collection",
                    "profile",
                    chrono::DateTime::parse_from_rfc3339("2026-01-04T00:00:00Z")
                        .expect("timestamp")
                        .with_timezone(&chrono::Utc),
                )
                .await
                .expect("claim"),
            VectorLeaseOutcome::Acquired(_)
        ));
    }

    #[tokio::test]
    async fn manual_merge_preserves_alias_and_combines_identifiers() {
        let (_temp, registry) = registry().await;
        let RegistrationOutcome::Registered(canonical) = registry
            .register(input("123", "Canonical"), None, "2026-01-01T00:00:00Z")
            .await
            .expect("register canonical")
        else {
            panic!("expected canonical registration");
        };
        let RegistrationOutcome::Registered(alias) = registry
            .register(
                LiteratureInput {
                    doi: Some("10.1000/alias".to_string()),
                    title: Some("Alias record".to_string()),
                    metadata: json!({}),
                    ..Default::default()
                },
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register alias")
        else {
            panic!("expected alias registration");
        };

        registry
            .merge_literatures(
                &canonical.literature_id,
                &alias.literature_id,
                "human review",
                None,
                "2026-01-02T00:00:00Z",
            )
            .await
            .expect("merge");

        assert_eq!(
            registry
                .resolve_literature_id(&alias.literature_id)
                .await
                .expect("resolve alias"),
            canonical.literature_id
        );
        let merged = registry
            .get(&alias.literature_id)
            .await
            .expect("get merged")
            .expect("merged literature");
        assert_eq!(merged.pmid.as_deref(), Some("123"));
        assert_eq!(merged.doi.as_deref(), Some("10.1000/alias"));
        assert_eq!(registry.literature_count().await.expect("count"), 1);
    }

    #[tokio::test]
    async fn vector_lease_blocks_competitors_and_releases_on_completion() {
        let (_temp, registry) = registry().await;
        let RegistrationOutcome::Registered(literature) = registry
            .register(input("456", "Lease"), None, "2026-01-01T00:00:00Z")
            .await
            .expect("register")
        else {
            panic!("expected registration");
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc);
        let VectorLeaseOutcome::Acquired(lease) = registry
            .claim_vector_job(&literature.literature_id, "collection", "profile", now)
            .await
            .expect("first claim")
        else {
            panic!("expected lease");
        };
        assert_eq!(
            registry
                .claim_vector_job(&literature.literature_id, "collection", "profile", now)
                .await
                .expect("second claim"),
            VectorLeaseOutcome::NotAcquired {
                status: "embedding".to_string()
            }
        );
        registry
            .update_claimed_vector_job(&lease, "complete", Some(2), Some(2), None, now)
            .await
            .expect("complete");
        assert_eq!(
            registry
                .claim_vector_job(&literature.literature_id, "collection", "profile", now)
                .await
                .expect("completed claim"),
            VectorLeaseOutcome::NotAcquired {
                status: "complete".to_string()
            }
        );
    }

    #[tokio::test]
    async fn independent_registry_pools_allow_only_one_vector_lease_owner() {
        let (_temp, first_registry) = registry().await;
        let second_registry = LiteratureRegistry::open(first_registry.path().to_path_buf())
            .await
            .expect("second registry");
        let RegistrationOutcome::Registered(literature) = first_registry
            .register(
                input("458", "Concurrent lease"),
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register")
        else {
            panic!("expected registration");
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc);
        let (first, second) = tokio::join!(
            first_registry.claim_vector_job(
                &literature.literature_id,
                "collection",
                "profile",
                now
            ),
            second_registry.claim_vector_job(
                &literature.literature_id,
                "collection",
                "profile",
                now
            ),
        );
        let outcomes = [first.expect("first claim"), second.expect("second claim")];

        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, VectorLeaseOutcome::Acquired(_)))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(
                    outcome,
                    VectorLeaseOutcome::NotAcquired { status } if status == "embedding"
                ))
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn expired_vector_lease_can_be_taken_over() {
        let (_temp, registry) = registry().await;
        let RegistrationOutcome::Registered(literature) = registry
            .register(input("457", "Expired lease"), None, "2026-01-01T00:00:00Z")
            .await
            .expect("register")
        else {
            panic!("expected registration");
        };
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc);
        let VectorLeaseOutcome::Acquired(first) = registry
            .claim_vector_job(&literature.literature_id, "collection", "profile", now)
            .await
            .expect("first claim")
        else {
            panic!("expected first lease");
        };
        let VectorLeaseOutcome::Acquired(second) = registry
            .claim_vector_job(
                &literature.literature_id,
                "collection",
                "profile",
                now + chrono::Duration::minutes(16),
            )
            .await
            .expect("takeover")
        else {
            panic!("expected takeover lease");
        };

        assert_eq!(first.job_id, second.job_id);
        assert_ne!(first.owner_token, second.owner_token);
    }

    #[tokio::test]
    async fn incomplete_qdrant_state_reopens_a_terminal_vector_job() {
        let (_temp, registry) = registry().await;
        let RegistrationOutcome::Registered(literature) = registry
            .register(
                input("459", "Incomplete terminal job"),
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("register")
        else {
            panic!("expected registration");
        };
        registry
            .record_vector_status(
                &literature.literature_id,
                "collection",
                "profile",
                "complete",
                None,
                None,
                None,
                "2026-01-01T00:00:00Z",
            )
            .await
            .expect("complete job");
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc);

        assert!(
            registry
                .reopen_terminal_vector_job_if_incomplete(
                    &literature.literature_id,
                    "collection",
                    "profile",
                    3,
                    1,
                    now,
                )
                .await
                .expect("reopen")
        );
        assert!(matches!(
            registry
                .claim_vector_job(&literature.literature_id, "collection", "profile", now,)
                .await
                .expect("claim"),
            VectorLeaseOutcome::Acquired(_)
        ));
    }
}
