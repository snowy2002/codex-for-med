//! Durable vector-job status and lease coordination.

use anyhow::Result;
use sqlx::Row;
use uuid::Uuid;

use super::literature_registry::LiteratureRegistry;
use super::literature_registry::VectorLease;
use super::literature_registry::VectorLeaseOutcome;

impl LiteratureRegistry {
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn record_vector_status(
        &self,
        literature_id: &str,
        collection_name: &str,
        embedding_profile: &str,
        status: &str,
        existing_dataset: Option<&str>,
        review_case_id: Option<&str>,
        last_error: Option<&str>,
        now: &str,
    ) -> Result<()> {
        validate_vector_status(status)?;
        sqlx::query(
            r#"
            INSERT INTO literature_vector_jobs(
                job_id, literature_id, collection_name, embedding_profile,
                status, existing_dataset, review_case_id, last_error,
                created_at, updated_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(literature_id, collection_name) DO UPDATE SET
                embedding_profile = excluded.embedding_profile,
                status = excluded.status,
                existing_dataset = excluded.existing_dataset,
                review_case_id = excluded.review_case_id,
                last_error = excluded.last_error,
                owner_token = NULL,
                lease_expires_at = NULL,
                updated_at = excluded.updated_at
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(literature_id)
        .bind(collection_name)
        .bind(embedding_profile)
        .bind(status)
        .bind(existing_dataset)
        .bind(review_case_id)
        .bind(last_error)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub(super) async fn claim_vector_job(
        &self,
        literature_id: &str,
        collection_name: &str,
        embedding_profile: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<VectorLeaseOutcome> {
        let now_text = now.to_rfc3339();
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            r#"
            INSERT OR IGNORE INTO literature_vector_jobs(
                job_id, literature_id, collection_name, embedding_profile,
                status, created_at, updated_at
            ) VALUES (?, ?, ?, ?, 'pending', ?, ?)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(literature_id)
        .bind(collection_name)
        .bind(embedding_profile)
        .bind(&now_text)
        .bind(&now_text)
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query(
            r#"
            SELECT job_id, status, lease_expires_at
            FROM literature_vector_jobs
            WHERE literature_id = ? AND collection_name = ?
            "#,
        )
        .bind(literature_id)
        .bind(collection_name)
        .fetch_one(&mut *tx)
        .await?;
        let status: String = row.try_get("status")?;
        let lease_expires_at: Option<String> = row.try_get("lease_expires_at")?;
        let active_lease = matches!(status.as_str(), "embedding" | "upserting" | "verifying")
            && lease_expires_at
                .as_deref()
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .is_some_and(|expires| expires > now);
        if matches!(
            status.as_str(),
            "complete" | "already_vectorized" | "possible_duplicate" | "blocked_conflict"
        ) || active_lease
        {
            tx.commit().await?;
            return Ok(VectorLeaseOutcome::NotAcquired { status });
        }

        let job_id: String = row.try_get("job_id")?;
        let owner_token = Uuid::new_v4().to_string();
        let lease_expires_at = (now + chrono::Duration::minutes(15)).to_rfc3339();
        let claimed = sqlx::query(
            r#"
            UPDATE literature_vector_jobs SET
                embedding_profile = ?,
                status = 'embedding',
                owner_token = ?,
                lease_expires_at = ?,
                attempt_count = attempt_count + 1,
                last_error = NULL,
                updated_at = ?
            WHERE job_id = ?
              AND status NOT IN (
                  'complete', 'already_vectorized',
                  'possible_duplicate', 'blocked_conflict'
              )
              AND (
                  owner_token IS NULL
                  OR lease_expires_at IS NULL
                  OR lease_expires_at <= ?
              )
            "#,
        )
        .bind(embedding_profile)
        .bind(&owner_token)
        .bind(lease_expires_at)
        .bind(&now_text)
        .bind(&job_id)
        .bind(&now_text)
        .execute(&mut *tx)
        .await?;
        if claimed.rows_affected() != 1 {
            let status = sqlx::query_scalar::<_, String>(
                "SELECT status FROM literature_vector_jobs WHERE job_id = ?",
            )
            .bind(&job_id)
            .fetch_one(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(VectorLeaseOutcome::NotAcquired { status });
        }
        tx.commit().await?;
        Ok(VectorLeaseOutcome::Acquired(VectorLease {
            job_id,
            owner_token,
        }))
    }

    pub(super) async fn update_claimed_vector_job(
        &self,
        lease: &VectorLease,
        status: &str,
        expected_point_count: Option<usize>,
        verified_point_count: Option<usize>,
        last_error: Option<&str>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<()> {
        validate_vector_status(status)?;
        let release = matches!(status, "complete" | "failed");
        let lease_expires_at =
            (!release).then(|| (now + chrono::Duration::minutes(15)).to_rfc3339());
        let result = sqlx::query(
            r#"
            UPDATE literature_vector_jobs SET
                status = ?,
                expected_point_count = COALESCE(?, expected_point_count),
                verified_point_count = COALESCE(?, verified_point_count),
                last_error = ?,
                owner_token = CASE WHEN ? THEN NULL ELSE owner_token END,
                lease_expires_at = ?,
                updated_at = ?
            WHERE job_id = ? AND owner_token = ?
            "#,
        )
        .bind(status)
        .bind(expected_point_count.map(|count| count as i64))
        .bind(verified_point_count.map(|count| count as i64))
        .bind(last_error)
        .bind(release)
        .bind(lease_expires_at)
        .bind(now.to_rfc3339())
        .bind(&lease.job_id)
        .bind(&lease.owner_token)
        .execute(&self.pool)
        .await?;
        anyhow::ensure!(
            result.rows_affected() == 1,
            "vector job lease was lost before status update"
        );
        Ok(())
    }
}

fn validate_vector_status(status: &str) -> Result<()> {
    anyhow::ensure!(
        matches!(
            status,
            "pending"
                | "embedding"
                | "upserting"
                | "verifying"
                | "complete"
                | "already_vectorized"
                | "possible_duplicate"
                | "blocked_conflict"
                | "failed"
        ),
        "invalid vector job status {status}"
    );
    Ok(())
}
