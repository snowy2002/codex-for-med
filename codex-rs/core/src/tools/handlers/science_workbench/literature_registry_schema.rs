//! SQLite schema migrations for the literature registry.

use anyhow::Result;
use sqlx::SqlitePool;

const CURRENT_SCHEMA_VERSION: i64 = 2;

const CREATE_LITERATURES: &str = r#"
CREATE TABLE IF NOT EXISTS literatures (
    literature_id TEXT PRIMARY KEY,
    pmid TEXT UNIQUE,
    doi TEXT UNIQUE,
    paper_id TEXT UNIQUE,
    title TEXT,
    abstract TEXT,
    authors_json TEXT NOT NULL DEFAULT '[]',
    journal TEXT,
    publication_date TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
)"#;

const CREATE_REVIEW_CASES: &str = r#"
CREATE TABLE IF NOT EXISTS literature_review_cases (
    review_case_id TEXT PRIMARY KEY,
    conflict_type TEXT NOT NULL CHECK (
        conflict_type IN ('identifier_conflict', 'possible_duplicate')
    ),
    incoming_identifiers_json TEXT NOT NULL,
    matched_literature_ids_json TEXT NOT NULL,
    incoming_metadata_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN (
            'pending',
            'resolved_same_literature',
            'resolved_different_literatures',
            'ignored_invalid_input'
        )
    ),
    resolution_json TEXT,
    created_at TEXT NOT NULL,
    resolved_at TEXT
)"#;

const CREATE_ALIASES: &str = r#"
CREATE TABLE IF NOT EXISTS literature_id_aliases (
    alias_literature_id TEXT PRIMARY KEY,
    canonical_literature_id TEXT NOT NULL
        REFERENCES literatures(literature_id),
    reason TEXT NOT NULL,
    created_at TEXT NOT NULL
)"#;

const CREATE_SOURCE_RECORDS: &str = r#"
CREATE TABLE IF NOT EXISTS literature_source_records (
    source_system TEXT NOT NULL,
    source_record_key TEXT NOT NULL,
    literature_id TEXT NOT NULL
        REFERENCES literatures(literature_id),
    match_method TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (source_system, source_record_key)
)"#;

const CREATE_VECTOR_JOBS: &str = r#"
CREATE TABLE IF NOT EXISTS literature_vector_jobs (
    job_id TEXT PRIMARY KEY,
    literature_id TEXT NOT NULL
        REFERENCES literatures(literature_id),
    collection_name TEXT NOT NULL,
    embedding_profile TEXT NOT NULL,
    status TEXT NOT NULL CHECK (
        status IN (
            'pending',
            'embedding',
            'upserting',
            'verifying',
            'complete',
            'already_vectorized',
            'possible_duplicate',
            'blocked_conflict',
            'failed'
        )
    ),
    expected_point_count INTEGER,
    verified_point_count INTEGER,
    existing_dataset TEXT,
    review_case_id TEXT
        REFERENCES literature_review_cases(review_case_id),
    owner_token TEXT,
    lease_expires_at TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (literature_id, collection_name)
)"#;

const CREATE_INITIALIZATIONS: &str = r#"
CREATE TABLE IF NOT EXISTS literature_registry_initializations (
    source_system TEXT PRIMARY KEY,
    status TEXT NOT NULL CHECK (status IN ('complete')),
    details_json TEXT NOT NULL,
    completed_at TEXT NOT NULL
)"#;

pub(super) async fn migrate_registry(pool: &SqlitePool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS literature_schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL
        )
        "#,
    )
    .execute(pool)
    .await?;
    let version: i64 =
        sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM literature_schema_migrations")
            .fetch_one(pool)
            .await?;
    anyhow::ensure!(
        version <= CURRENT_SCHEMA_VERSION,
        "literature registry schema version {version} is newer than supported version {CURRENT_SCHEMA_VERSION}"
    );
    if version == 0 {
        let mut tx = pool.begin().await?;
        for statement in [
            CREATE_LITERATURES,
            CREATE_REVIEW_CASES,
            CREATE_ALIASES,
            CREATE_SOURCE_RECORDS,
            CREATE_VECTOR_JOBS,
        ] {
            sqlx::query(statement).execute(&mut *tx).await?;
        }
        sqlx::query(
            "INSERT INTO literature_schema_migrations(version, name, applied_at) VALUES(1, 'initial literature registry', ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    if version < 2 {
        let mut tx = pool.begin().await?;
        sqlx::query(CREATE_INITIALIZATIONS)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO literature_schema_migrations(version, name, applied_at) VALUES(2, 'registry initialization markers', ?)",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
    }
    Ok(())
}
