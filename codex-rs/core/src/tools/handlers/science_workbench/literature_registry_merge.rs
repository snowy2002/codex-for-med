//! Transactional human-confirmed literature merges and durable aliases.

use anyhow::Result;
use serde_json::Value;
use serde_json::json;
use sqlx::Row;
use std::collections::BTreeSet;

use super::literature_registry::LiteratureInput;
use super::literature_registry::LiteratureRegistry;

impl LiteratureRegistry {
    pub(crate) async fn merge_literatures(
        &self,
        canonical_literature_id: &str,
        alias_literature_id: &str,
        reason: &str,
        review_case_id: Option<&str>,
        now: &str,
    ) -> Result<String> {
        anyhow::ensure!(!reason.trim().is_empty(), "alias reason must not be empty");
        let _write_permit = self
            .write_gate
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("literature registry write gate was closed"))?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let canonical = self
            .resolve_alias_in(&mut tx, canonical_literature_id)
            .await?;
        let alias = self.resolve_alias_in(&mut tx, alias_literature_id).await?;
        if let Some(review_case_id) = review_case_id {
            let case = sqlx::query(
                r#"
                SELECT conflict_type, status, matched_literature_ids_json,
                       incoming_metadata_json
                FROM literature_review_cases
                WHERE review_case_id = ?
                "#,
            )
            .bind(review_case_id)
            .fetch_one(&mut *tx)
            .await?;
            let conflict_type: String = case.try_get("conflict_type")?;
            let status: String = case.try_get("status")?;
            anyhow::ensure!(
                conflict_type == "possible_duplicate" && status == "pending",
                "review case is not a pending possible_duplicate"
            );
            let matched: String = case.try_get("matched_literature_ids_json")?;
            let metadata: String = case.try_get("incoming_metadata_json")?;
            let mut allowed_ids = serde_json::from_str::<Vec<String>>(&matched).unwrap_or_default();
            if let Some(incoming) =
                serde_json::from_str::<Value>(&metadata)
                    .ok()
                    .and_then(|value| {
                        value
                            .pointer("/candidate_evaluation/incoming_literature_id")
                            .and_then(Value::as_str)
                            .map(ToString::to_string)
                    })
            {
                allowed_ids.push(incoming);
            }
            let mut allowed_canonical_ids = BTreeSet::new();
            for id in allowed_ids {
                allowed_canonical_ids.insert(self.resolve_alias_in(&mut tx, &id).await?);
            }
            anyhow::ensure!(
                allowed_canonical_ids.contains(&canonical)
                    && allowed_canonical_ids.contains(&alias),
                "the requested IDs are not both participants in this review case"
            );
        }
        if canonical == alias {
            anyhow::bail!("canonical and alias already resolve to the same literature");
        }

        let canonical_row =
            sqlx::query("SELECT pmid, doi, paper_id FROM literatures WHERE literature_id = ?")
                .bind(&canonical)
                .fetch_one(&mut *tx)
                .await?;
        let alias_row = sqlx::query(
            r#"
            SELECT pmid, doi, paper_id, title, abstract, authors_json, journal,
                   publication_date, metadata_json
            FROM literatures WHERE literature_id = ?
            "#,
        )
        .bind(&alias)
        .fetch_one(&mut *tx)
        .await?;
        for column in ["pmid", "doi", "paper_id"] {
            let canonical_value: Option<String> = canonical_row.try_get(column)?;
            let alias_value: Option<String> = alias_row.try_get(column)?;
            anyhow::ensure!(
                canonical_value.is_none()
                    || alias_value.is_none()
                    || canonical_value == alias_value,
                "cannot merge literatures with conflicting {column}"
            );
        }
        let alias_input = LiteratureInput {
            pmid: alias_row.try_get("pmid")?,
            doi: alias_row.try_get("doi")?,
            paper_id: alias_row.try_get("paper_id")?,
            title: alias_row.try_get("title")?,
            abstract_text: alias_row.try_get("abstract")?,
            authors: serde_json::from_str(&alias_row.try_get::<String, _>("authors_json")?)
                .unwrap_or_default(),
            journal: alias_row.try_get("journal")?,
            publication_date: alias_row.try_get("publication_date")?,
            metadata: serde_json::from_str(&alias_row.try_get::<String, _>("metadata_json")?)
                .unwrap_or_else(|_| json!({})),
        };

        sqlx::query(
            "UPDATE literatures SET pmid = NULL, doi = NULL, paper_id = NULL WHERE literature_id = ?",
        )
        .bind(&alias)
        .execute(&mut *tx)
        .await?;
        self.merge_metadata(&mut tx, &canonical, &alias_input, now)
            .await?;
        sqlx::query(
            "UPDATE literature_source_records SET literature_id = ?, updated_at = ? WHERE literature_id = ?",
        )
        .bind(&canonical)
        .bind(now)
        .bind(&alias)
        .execute(&mut *tx)
        .await?;

        // A canonical row wins if both IDs have a job for the same collection.
        // Otherwise retain the alias-side job by moving it to the canonical ID.
        sqlx::query(
            r#"
            DELETE FROM literature_vector_jobs
            WHERE literature_id = ?
              AND collection_name IN (
                  SELECT collection_name FROM literature_vector_jobs
                  WHERE literature_id = ?
              )
            "#,
        )
        .bind(&alias)
        .bind(&canonical)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE literature_vector_jobs SET literature_id = ?, updated_at = ? WHERE literature_id = ?",
        )
        .bind(&canonical)
        .bind(now)
        .bind(&alias)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE literature_id_aliases SET canonical_literature_id = ? WHERE canonical_literature_id = ?",
        )
        .bind(&canonical)
        .bind(&alias)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO literature_id_aliases(alias_literature_id, canonical_literature_id, reason, created_at) VALUES(?, ?, ?, ?)",
        )
        .bind(&alias)
        .bind(&canonical)
        .bind(reason.trim())
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM literatures WHERE literature_id = ?")
            .bind(&alias)
            .execute(&mut *tx)
            .await?;
        if let Some(review_case_id) = review_case_id {
            let resolved = sqlx::query(
                r#"
                UPDATE literature_review_cases SET
                    status = 'resolved_same_literature',
                    resolution_json = ?,
                    resolved_at = ?
                WHERE review_case_id = ? AND status = 'pending'
                "#,
            )
            .bind(serde_json::to_string(&json!({
                "canonical_literature_id": canonical,
                "alias_literature_id": alias,
                "reason": reason.trim(),
            }))?)
            .bind(now)
            .bind(review_case_id)
            .execute(&mut *tx)
            .await?;
            anyhow::ensure!(
                resolved.rows_affected() == 1,
                "review case was not pending at merge time"
            );
        }
        tx.commit().await?;
        Ok(canonical)
    }

    pub(super) async fn resolve_review_as_different(
        &self,
        review_case_id: &str,
        reason: &str,
        now: &str,
    ) -> Result<()> {
        anyhow::ensure!(!reason.trim().is_empty(), "review reason must not be empty");
        let _write_permit = self
            .write_gate
            .acquire()
            .await
            .map_err(|_| anyhow::anyhow!("literature registry write gate was closed"))?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let case = sqlx::query(
            "SELECT conflict_type, status FROM literature_review_cases WHERE review_case_id = ?",
        )
        .bind(review_case_id)
        .fetch_one(&mut *tx)
        .await?;
        let conflict_type: String = case.try_get("conflict_type")?;
        let status: String = case.try_get("status")?;
        anyhow::ensure!(
            conflict_type == "possible_duplicate",
            "only possible_duplicate reviews can be resolved as different literatures"
        );
        anyhow::ensure!(status == "pending", "review case is not pending");
        sqlx::query(
            r#"
            UPDATE literature_review_cases SET
                status = 'resolved_different_literatures',
                resolution_json = ?,
                resolved_at = ?
            WHERE review_case_id = ?
            "#,
        )
        .bind(serde_json::to_string(&json!({
            "reason": reason.trim(),
        }))?)
        .bind(now)
        .bind(review_case_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            r#"
            UPDATE literature_vector_jobs SET
                status = 'pending',
                owner_token = NULL,
                lease_expires_at = NULL,
                updated_at = ?
            WHERE review_case_id = ? AND status = 'possible_duplicate'
            "#,
        )
        .bind(now)
        .bind(review_case_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
