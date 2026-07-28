use super::pubmed_cache::PubmedCache;
use super::*;
use crate::tools::handlers::biomed_external_db::CitationToCheck;
use crate::tools::handlers::biomed_external_db::validate_one_citation;

const NCBI_EFETCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi";
const NCBI_ESEARCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esearch.fcgi";
const NCBI_ESUMMARY_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi";

#[derive(Debug, Deserialize)]
pub(super) struct PubmedLiteratureMapArgs {
    topic: String,
    #[serde(default)]
    pubmed_query: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default = "default_pubmed_map_retmax")]
    retmax: usize,
    #[serde(default = "default_pubmed_map_sort")]
    sort: PubmedMapSort,
    #[serde(default)]
    min_year: Option<u32>,
    #[serde(default)]
    max_year: Option<u32>,
    #[serde(default = "default_true")]
    fetch_abstracts: bool,
    #[serde(default = "default_pubmed_map_max_mesh_terms")]
    max_mesh_terms: usize,
    #[serde(default)]
    validate_citations: bool,
    #[serde(default)]
    force_refresh: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PubmedMapSort {
    Relevance,
    PubDate,
}

impl PubmedMapSort {
    fn as_esearch_sort(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::PubDate => "pub_date",
        }
    }
}

fn default_pubmed_map_retmax() -> usize {
    10
}

fn default_pubmed_map_sort() -> PubmedMapSort {
    PubmedMapSort::Relevance
}

fn default_pubmed_map_max_mesh_terms() -> usize {
    50
}

pub(super) async fn pubmed_literature_map(
    client: &reqwest::Client,
    args: PubmedLiteratureMapArgs,
    cwd: &Path,
) -> Result<String, FunctionCallError> {
    let topic = args.topic.trim();
    if topic.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "topic must not be empty".to_string(),
        ));
    }
    let started_at = chrono::Utc::now();
    let pubmed_query = args
        .pubmed_query
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(topic);
    let project_id = args
        .project_id
        .as_deref()
        .map(slugify_project_id)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| slugify_project_id(topic));
    let retmax = args.retmax.clamp(1, 50);
    let max_mesh_terms = args.max_mesh_terms.min(200);
    let year_window = pubmed_map_year_range(args.min_year, args.max_year)?;
    let ncbi_client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| {
            FunctionCallError::Fatal(format!("failed to build the PubMed HTTP client: {error}"))
        })?;

    let cache = PubmedCache::new(cwd, args.force_refresh);
    let mut ncbi_stats = NcbiRequestStats::default();
    let mut search = pubmed_map_search(
        &ncbi_client,
        &cache,
        &mut ncbi_stats,
        pubmed_query,
        retmax,
        args.sort,
        year_window,
    )
    .await?;

    let mut fetch_errors = 0usize;
    if args.fetch_abstracts {
        for idx in 0..search.records.len() {
            match fetch_pubmed_map_details(
                &ncbi_client,
                &cache,
                &mut ncbi_stats,
                &search.records[idx].pmid,
                max_mesh_terms,
            )
            .await
            {
                Ok(details) => search.records[idx].merge_details(details),
                Err(err) => {
                    fetch_errors += 1;
                    search.records[idx].fetch_error = Some(err.to_string());
                }
            }
        }
    }
    let citation_validation = if args.validate_citations {
        validate_pubmed_map_citations(client, &mut search.records).await
    } else {
        PubmedCitationValidationSummary::disabled()
    };

    let now = chrono::Utc::now();
    let now_rfc3339 = now.to_rfc3339();
    let run_id = new_run_id(&started_at, "pubmed_literature_map");
    let registry = LiteratureRegistry::open_workspace(cwd)
        .await
        .map_err(|err| {
            FunctionCallError::Fatal(format!("failed to open literature registry: {err:#}"))
        })?;
    let mut registration_conflicts = Vec::new();
    for record in &mut search.records {
        let registration = registry
            .register(
                LiteratureInput {
                    pmid: Some(record.pmid.clone()),
                    doi: Some(record.doi.clone()),
                    paper_id: Some(format!("PMID:{}", record.pmid)),
                    title: Some(record.title.clone()),
                    abstract_text: Some(record.abstract_text.clone()),
                    authors: record.authors.clone(),
                    journal: Some(record.journal.clone()),
                    publication_date: Some(record.publication_date.clone()),
                    metadata: json!({
                        "raw_identifiers": {
                            "pmid": record.pmid,
                            "doi": record.doi,
                        },
                        "field_sources": {
                            "title": {
                                "source": "pubmed",
                                "observed_at": now_rfc3339,
                            },
                            "abstract": {
                                "source": "pubmed",
                                "observed_at": now_rfc3339,
                            },
                            "authors": {
                                "source": "pubmed",
                                "observed_at": now_rfc3339,
                            },
                            "journal": {
                                "source": "pubmed",
                                "observed_at": now_rfc3339,
                            },
                            "publication_date": {
                                "source": "pubmed",
                                "observed_at": now_rfc3339,
                            },
                            "mesh_terms": {
                                "source": "pubmed",
                                "observed_at": now_rfc3339,
                            },
                            "publication_types": {
                                "source": "pubmed",
                                "observed_at": now_rfc3339,
                            },
                            "language": {
                                "source": "pubmed",
                                "observed_at": now_rfc3339,
                            },
                            "relations": {
                                "source": "pubmed",
                                "observed_at": now_rfc3339,
                            }
                        },
                        "mesh_terms": record.mesh_terms,
                        "publication_types": record.publication_types,
                        "language": record.language,
                        "relations": record.relations,
                        "publication_date_precision": publication_date_precision(&record.publication_date),
                        "flags": {
                            "abstract_missing": record.abstract_text.trim().is_empty(),
                            "retracted": has_publication_type(&record.publication_types, "retracted"),
                            "corrected": has_publication_type(&record.publication_types, "erratum")
                                || has_publication_type(&record.publication_types, "corrected"),
                        }
                    }),
                },
                Some(SourceRecord {
                    system: "pubmed".to_string(),
                    key: record.pmid.clone(),
                    match_method: "pmid".to_string(),
                }),
                &now_rfc3339,
            )
            .await
            .map_err(|err| {
                FunctionCallError::Fatal(format!(
                    "failed to register PubMed literature metadata: {err:#}"
                ))
            })?;
        match registration {
            RegistrationOutcome::Registered(registered) => {
                record.literature_id = registered.literature_id;
                record.review_case_id = registered.review_case_id;
                record.match_method = registered.match_method;
            }
            RegistrationOutcome::Conflict {
                review_case_id,
                matched_literature_ids,
            } => {
                record.review_case_id = Some(review_case_id.clone());
                record.match_method = "identifier_conflict".to_string();
                registration_conflicts.push(json!({
                    "review_case_id": review_case_id,
                    "matched_literature_ids": matched_literature_ids,
                    "pmid": record.pmid,
                    "doi": record.doi,
                }));
            }
        }
    }

    let mut vector_collection = None;
    let mut vector_embedding_model = None;
    let mut vector_writes_enabled = false;
    let mut vector_registry_backup = None;
    match PubmedVectorConfig::from_environment() {
        Ok(config) => {
            vector_collection = Some(config.collection_name().to_string());
            vector_embedding_model = Some(config.embedding_model().to_string());
            vector_writes_enabled = config.write_enabled();
            if vector_writes_enabled {
                let backup = cwd
                    .join(".codex-med")
                    .join("backups")
                    .join(format!("literatures_before_{run_id}.sqlite3"));
                registry.backup_to(&backup).await.map_err(|err| {
                    FunctionCallError::Fatal(format!(
                        "refusing vector writes because the registry backup failed: {err:#}"
                    ))
                })?;
                vector_registry_backup = Some(relative_display(cwd, &backup));
            }
            let ingestor = PubmedVectorIngestor::new(client, config);
            for record in &mut search.records {
                if record.literature_id.is_empty() {
                    record.vector_status = "blocked_conflict".to_string();
                    continue;
                }
                let literature = match registry.get(&record.literature_id).await {
                    Ok(Some(literature)) => literature,
                    Ok(None) => {
                        record.vector_status = "failed".to_string();
                        record.vector_error =
                            Some("registered literature metadata is missing".to_string());
                        continue;
                    }
                    Err(err) => {
                        record.vector_status = "failed".to_string();
                        record.vector_error = Some(format!(
                            "failed to read registered literature metadata: {err:#}"
                        ));
                        continue;
                    }
                };
                let vector = ingestor
                    .ingest(
                        &registry,
                        &literature,
                        record.review_case_id.as_deref(),
                        now,
                    )
                    .await;
                record.apply_global_metadata(&literature);
                record.vector_status = vector.status;
                record.expected_points = vector.expected_points;
                record.verified_points = vector.verified_points;
                record.existing_vector_dataset = vector.existing_dataset;
                if vector.review_case_id.is_some() {
                    record.review_case_id = vector.review_case_id;
                }
                record.vector_error = vector.error;
            }
        }
        Err(err) => {
            for record in &mut search.records {
                record.vector_status = "failed".to_string();
                record.vector_error = Some(format!("invalid PubMed vector configuration: {err:#}"));
            }
        }
    }
    for record in &mut search.records {
        if record.literature_id.is_empty() {
            continue;
        }
        match registry.get(&record.literature_id).await {
            Ok(Some(literature)) => record.apply_global_metadata(&literature),
            Ok(None) => {
                record.vector_status = "failed".to_string();
                record.vector_error = Some("registered literature metadata is missing".to_string());
            }
            Err(error) => {
                record.vector_status = "failed".to_string();
                record.vector_error = Some(format!("failed to resolve global metadata: {error:#}"));
            }
        }
    }

    let mut seen_literature_ids = std::collections::HashSet::new();
    let literature_ids = search
        .records
        .iter()
        .filter(|record| !record.literature_id.is_empty())
        .filter(|&record| seen_literature_ids.insert(record.literature_id.clone()))
        .map(|record| record.literature_id.clone())
        .collect::<Vec<_>>();
    let ids_csv = render_literature_ids(&literature_ids);
    let report = render_pubmed_report(
        topic,
        pubmed_query,
        &search,
        &citation_validation,
        &ncbi_stats,
    );
    let citations = render_pubmed_citations_bib(&search.records);
    let project_dir = cwd.join("research_projects").join(&project_id);
    let vector_statuses = search.records.iter().fold(
        std::collections::BTreeMap::<String, usize>::new(),
        |mut counts, record| {
            *counts.entry(record.vector_status.clone()).or_default() += 1;
            counts
        },
    );
    let mut run_errors = registration_conflicts
        .iter()
        .map(|conflict| {
            json!({
                "stage": "sqlite_registration",
                "retryable": false,
                "recovery": "Resolve the review_case_id before rerunning the affected record.",
                "details": conflict,
            })
        })
        .collect::<Vec<_>>();
    for record in &search.records {
        if let Some(error) = record.fetch_error.as_deref() {
            run_errors.push(json!({
                "stage": "pubmed_efetch",
                "record": {"pmid": record.pmid},
                "retryable": true,
                "recovery": "Rerun with force_refresh=true after NCBI recovers.",
                "message": error,
            }));
        }
        if let Some(error) = record.vector_error.as_deref() {
            run_errors.push(json!({
                "stage": "vector_ingestion",
                "record": {
                    "literature_id": record.literature_id,
                    "pmid": record.pmid,
                },
                "retryable": record.vector_status == "failed",
                "recovery": "Inspect the vector job and review case before retrying.",
                "message": error,
            }));
        }
        if let Some(validation) = record.citation_validation.as_ref().filter(|validation| {
            validation.status != "verified" && !validation.warning.trim().is_empty()
        }) {
            run_errors.push(json!({
                "stage": "citation_validation",
                "record": {
                    "literature_id": record.literature_id,
                    "pmid": record.pmid,
                    "doi": record.doi,
                },
                "retryable": validation.status == "validation_error",
                "recovery": "Review the resolved citation metadata before manuscript use.",
                "message": validation.warning,
            }));
        }
    }
    for degradation in &search.query_degraded {
        run_errors.push(json!({
            "stage": "pubmed_query_translation",
            "retryable": false,
            "recovery": "Review the translated PubMed query and refine pubmed_query if needed.",
            "message": degradation,
        }));
    }
    let finished_at = chrono::Utc::now().to_rfc3339();
    let run_status = if run_errors.is_empty() {
        "completed"
    } else {
        "completed_with_errors"
    };
    let provenance_hits = search
        .records
        .iter()
        .map(|record| {
            let citation_validation = record.citation_validation.as_ref().map(|validation| {
                json!({
                    "status": validation.status,
                    "warning": validation.warning,
                    "title_match": validation.title_match,
                    "authors_match": validation.authors_match,
                    "year_match": validation.year_match,
                    "resolved_title": validation.resolved_title,
                    "resolved_year": validation.resolved_year,
                })
            });
            json!({
                "rank": record.rank,
                "literature_id": record.literature_id,
                "pmid": record.pmid,
                "doi": record.doi,
                "source_uri": format!("https://pubmed.ncbi.nlm.nih.gov/{}/", record.pmid),
                "external_identifiers": {
                    "pmid": record.pmid,
                    "doi": record.doi,
                    "paper_id": format!("PMID:{}", record.pmid),
                },
                "retrieval_score": Value::Null,
                "match_method": record.match_method,
                "duplicate_status": if record.literature_id.is_empty() {
                    "identifier_conflict"
                } else if record.review_case_id.is_some() {
                    "possible_duplicate"
                } else {
                    "canonical"
                },
                "review_case_id": record.review_case_id,
                "vector_job_status": record.vector_status,
                "fetch_error": record.fetch_error,
                "citation_validation": citation_validation,
                "vector": {
                    "status": record.vector_status,
                    "expected_points": record.expected_points,
                    "verified_points": record.verified_points,
                    "existing_dataset": record.existing_vector_dataset,
                    "error": record.vector_error,
                }
            })
        })
        .collect::<Vec<_>>();

    let mut run = json!({
        "schema_version": 1,
        "workflow": "pubmed_literature_map",
        "run_id": run_id,
        "started_at": started_at.to_rfc3339(),
        "finished_at": finished_at,
        "status": run_status,
        "project_id": project_id,
        "topic": topic,
        "pubmed_query": pubmed_query,
        "sort": args.sort.as_esearch_sort(),
        "year_range": pubmed_map_year_range_json(args.min_year, args.max_year),
        "retmax_effective": retmax,
        "fetch_abstracts": args.fetch_abstracts,
        "max_mesh_terms": max_mesh_terms,
        "validate_citations": args.validate_citations,
        "force_refresh": args.force_refresh,
        "input": {
            "original": {
                "topic": args.topic,
                "pubmed_query": args.pubmed_query,
                "project_id": args.project_id,
                "retmax": args.retmax,
                "sort": args.sort.as_esearch_sort(),
                "min_year": args.min_year,
                "max_year": args.max_year,
                "fetch_abstracts": args.fetch_abstracts,
                "max_mesh_terms": args.max_mesh_terms,
                "validate_citations": args.validate_citations,
                "force_refresh": args.force_refresh,
            },
            "normalized": {
                "topic": topic,
                "pubmed_query": pubmed_query,
                "project_id": project_id,
                "retmax": retmax,
                "sort": args.sort.as_esearch_sort(),
                "year_range": pubmed_map_year_range_json(args.min_year, args.max_year),
                "fetch_abstracts": args.fetch_abstracts,
                "max_mesh_terms": max_mesh_terms,
                "validate_citations": args.validate_citations,
                "force_refresh": args.force_refresh,
            },
        },
        "cache": relative_display(cwd, &cwd.join(".codex-med").join("cache").join("pubmed")),
        "vector_ingestion": {
            "collection": vector_collection,
            "embedding_model": vector_embedding_model,
            "embedding_profile": super::pubmed_chunks::PUBMED_EMBEDDING_PROFILE,
            "writes_enabled": vector_writes_enabled,
            "registry_backup": vector_registry_backup,
        },
        "backends": {
            "pubmed": {
                "provider": "NCBI E-utilities",
                "esearch_url": NCBI_ESEARCH_URL,
                "esummary_url": NCBI_ESUMMARY_URL,
                "efetch_url": NCBI_EFETCH_URL
            },
            "vector": {
                "provider": "qdrant",
                "collection": vector_collection,
                "embedding_model": vector_embedding_model,
                "embedding_profile": super::pubmed_chunks::PUBMED_EMBEDDING_PROFILE,
                "reranker_model": Value::Null,
            }
        },
        "query_translation": search.query_translation,
        "query_degraded": search.query_degraded,
        "ncbi_requests": ncbi_stats.to_json(),
        "total_count": search.total_count,
        "returned": search.records.len(),
        "dropped_by_esummary": search.dropped_by_esummary,
        "fetch_errors": fetch_errors,
        "citation_validation": citation_validation.to_json(),
        "registry": relative_display(cwd, registry.path()),
        "registration_conflicts": registration_conflicts,
        "errors": run_errors,
        "hits": provenance_hits,
        "counts": {
            "pubmed_records": search.records.len(),
            "output_literatures": literature_ids.len(),
            "registration_conflicts": registration_conflicts.len(),
            "fetch_errors": fetch_errors,
            "vector_statuses": vector_statuses,
        }
    });

    let (committed, manifest_path) = with_project_lock(&project_dir, || {
        let legacy_migration = migrate_legacy_project(&project_dir)?;
        let recovered_runs = recover_incomplete_literature_runs(&project_dir)?;
        if let Some(object) = run.as_object_mut() {
            object.insert("legacy_migration".to_string(), json!(legacy_migration));
            object.insert("recovered_runs".to_string(), json!(recovered_runs));
        }
        let committed = commit_literature_run(
            cwd,
            &project_id,
            "pubmed",
            "pubmed_literature_map",
            &run_id,
            &ids_csv,
            &report,
            &citations,
            run.clone(),
        )?;
        let manifest_path = record_project_run_unlocked(
            &committed.project_dir,
            &project_id,
            topic,
            &finished_at,
            run.clone(),
            "pubmed",
            &committed.provenance,
            &[
                ("literature_ids", &committed.latest_ids),
                ("report", &committed.latest_report),
                ("citations", &committed.latest_citations),
            ],
        )?;
        Ok((committed, manifest_path))
    })?;

    let output = json!({
        "project_id": project_id,
        "project_dir": project_dir,
        "topic": topic,
        "pubmed_query": pubmed_query,
        "returned": search.records.len(),
        "total_count": search.total_count,
        "fetch_abstracts": args.fetch_abstracts,
        "fetch_errors": fetch_errors,
        "vector_writes_enabled": vector_writes_enabled,
        "vector_statuses": vector_statuses,
        "validate_citations": args.validate_citations,
        "citation_validation": citation_validation.to_json(),
        "manifest": manifest_path,
        "created_files": [
            committed.latest_ids,
            committed.latest_report,
            committed.latest_citations,
            committed.provenance,
            committed.snapshot_dir,
            manifest_path
        ],
        "next_steps": [
            "Review literature/pubmed/literature_ids.csv and report.md.",
            "Check this run's provenance before trusting PubMed filters.",
            "Resolve any review_case_id before vectorizing a possible duplicate."
        ]
    });
    pretty_json(output)
}

#[derive(Debug, Clone)]
struct PubmedMapSearchResult {
    query_translation: Option<String>,
    query_degraded: Vec<String>,
    total_count: Option<u64>,
    dropped_by_esummary: usize,
    records: Vec<PubmedMapRecord>,
}

#[derive(Debug, Default)]
struct NcbiRequestStats {
    logical_requests: usize,
    cache_hits: usize,
    network_attempts: usize,
    retries: usize,
    throttled_responses: usize,
    server_error_responses: usize,
    transient_network_errors: usize,
}

impl NcbiRequestStats {
    fn to_json(&self) -> Value {
        json!({
            "logical_requests": self.logical_requests,
            "cache_hits": self.cache_hits,
            "network_attempts": self.network_attempts,
            "retries": self.retries,
            "throttled_responses": self.throttled_responses,
            "server_error_responses": self.server_error_responses,
            "transient_network_errors": self.transient_network_errors,
        })
    }
}

#[derive(Debug, Clone)]
struct PubmedMapRecord {
    rank: usize,
    literature_id: String,
    review_case_id: Option<String>,
    match_method: String,
    vector_status: String,
    expected_points: usize,
    verified_points: usize,
    existing_vector_dataset: Option<String>,
    vector_error: Option<String>,
    pmid: String,
    title: String,
    authors: Vec<String>,
    journal: String,
    publication_date: String,
    doi: String,
    abstract_text: String,
    publication_types: Vec<String>,
    mesh_terms: Vec<String>,
    mesh_term_count: usize,
    language: String,
    relations: Vec<Value>,
    fetch_error: Option<String>,
    citation_validation: Option<PubmedCitationValidation>,
}

impl PubmedMapRecord {
    fn merge_details(&mut self, details: PubmedMapDetails) {
        replace_non_empty(&mut self.title, details.title);
        replace_non_empty(&mut self.journal, details.journal);
        replace_non_empty(&mut self.publication_date, details.publication_date);
        replace_non_empty(&mut self.doi, details.doi);
        replace_non_empty(&mut self.abstract_text, details.abstract_text);
        replace_non_empty(&mut self.language, details.language);
        if !details.authors.is_empty() {
            self.authors = details.authors;
        }
        if !details.publication_types.is_empty() {
            self.publication_types = details.publication_types;
        }
        if !details.mesh_terms.is_empty() {
            self.mesh_terms = details.mesh_terms;
        }
        if !details.relations.is_empty() {
            self.relations = details.relations;
        }
        self.mesh_term_count = details.mesh_term_count;
    }

    fn apply_global_metadata(&mut self, literature: &literature_registry::Literature) {
        self.pmid = literature.pmid.clone().unwrap_or_default();
        self.doi = literature.doi.clone().unwrap_or_default();
        self.title = literature.title.clone().unwrap_or_default();
        self.authors = literature.authors.clone();
        self.journal = literature.journal.clone().unwrap_or_default();
        self.publication_date = literature.publication_date.clone().unwrap_or_default();
        self.abstract_text = literature.abstract_text.clone().unwrap_or_default();
        self.mesh_terms = metadata_string_array(&literature.metadata, "mesh_terms");
        self.mesh_term_count = self.mesh_terms.len();
        self.publication_types = metadata_string_array(&literature.metadata, "publication_types");
        self.language = literature
            .metadata
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        self.relations = literature
            .metadata
            .get("relations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
    }
}

#[derive(Debug, Clone, Default)]
struct PubmedCitationValidation {
    status: String,
    warning: String,
    title_match: Option<bool>,
    authors_match: Option<bool>,
    year_match: Option<bool>,
    resolved_title: Option<String>,
    resolved_year: Option<u64>,
}

#[derive(Debug, Clone, Default)]
struct PubmedCitationValidationSummary {
    enabled: bool,
    checked: usize,
    verified: usize,
    mismatches: usize,
    unresolved: usize,
    missing_doi: usize,
    errors: usize,
}

impl PubmedCitationValidationSummary {
    fn disabled() -> Self {
        Self::default()
    }

    fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "checked": self.checked,
            "verified": self.verified,
            "mismatches": self.mismatches,
            "unresolved": self.unresolved,
            "missing_doi": self.missing_doi,
            "errors": self.errors,
        })
    }
}

#[derive(Debug, Clone)]
struct PubmedMapDetails {
    title: String,
    authors: Vec<String>,
    journal: String,
    publication_date: String,
    doi: String,
    abstract_text: String,
    publication_types: Vec<String>,
    mesh_terms: Vec<String>,
    mesh_term_count: usize,
    language: String,
    relations: Vec<Value>,
}

async fn pubmed_map_search(
    client: &reqwest::Client,
    cache: &PubmedCache,
    stats: &mut NcbiRequestStats,
    query: &str,
    retmax: usize,
    sort: PubmedMapSort,
    year_window: Option<(u32, u32)>,
) -> Result<PubmedMapSearchResult, FunctionCallError> {
    let mut esearch_query = ncbi_common_query();
    esearch_query.push(("db", "pubmed".to_string()));
    esearch_query.push(("term", query.to_string()));
    esearch_query.push(("retmax", retmax.to_string()));
    esearch_query.push(("retmode", "json".to_string()));
    esearch_query.push(("sort", sort.as_esearch_sort().to_string()));
    if let Some((min_year, max_year)) = year_window {
        esearch_query.push(("datetype", "pdat".to_string()));
        esearch_query.push(("mindate", min_year.to_string()));
        esearch_query.push(("maxdate", max_year.to_string()));
    }

    let esearch_json = http_get_json_with_query(
        client,
        cache,
        stats,
        NCBI_ESEARCH_URL,
        &esearch_query,
        "PubMed esearch",
    )
    .await?;
    if let Some(error) = esearch_json
        .pointer("/esearchresult/ERROR")
        .and_then(Value::as_str)
    {
        return Err(FunctionCallError::RespondToModel(format!(
            "PubMed rejected the query: {error}"
        )));
    }

    let query_translation = esearch_json
        .pointer("/esearchresult/querytranslation")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let query_degraded = pubmed_map_query_degradations(&esearch_json);
    let total_count = esearch_json
        .pointer("/esearchresult/count")
        .and_then(Value::as_str)
        .and_then(|count| count.parse::<u64>().ok());
    let pmids = esearch_json
        .pointer("/esearchresult/idlist")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if pmids.is_empty() {
        return Ok(PubmedMapSearchResult {
            query_translation,
            query_degraded,
            total_count,
            dropped_by_esummary: 0,
            records: Vec::new(),
        });
    }

    let mut esummary_query = ncbi_common_query();
    esummary_query.push(("db", "pubmed".to_string()));
    esummary_query.push(("id", pmids.join(",")));
    esummary_query.push(("retmode", "json".to_string()));
    let esummary_json = http_get_json_with_query(
        client,
        cache,
        stats,
        NCBI_ESUMMARY_URL,
        &esummary_query,
        "PubMed esummary",
    )
    .await?;

    let records = pmids
        .iter()
        .enumerate()
        .filter_map(|(idx, pmid)| {
            esummary_json
                .pointer("/result")
                .and_then(|result| result.get(pmid))
                .map(|entry| pubmed_map_record_from_summary(idx + 1, pmid, entry))
        })
        .collect::<Vec<_>>();
    let dropped_by_esummary = pmids.len().saturating_sub(records.len());

    Ok(PubmedMapSearchResult {
        query_translation,
        query_degraded,
        total_count,
        dropped_by_esummary,
        records,
    })
}

async fn fetch_pubmed_map_details(
    client: &reqwest::Client,
    cache: &PubmedCache,
    stats: &mut NcbiRequestStats,
    pmid: &str,
    max_mesh_terms: usize,
) -> Result<PubmedMapDetails, FunctionCallError> {
    let mut query = ncbi_common_query();
    query.push(("db", "pubmed".to_string()));
    query.push(("id", pmid.to_string()));
    query.push(("rettype", "medline".to_string()));
    query.push(("retmode", "text".to_string()));

    let raw_text = http_get_text_with_query(client, cache, stats, NCBI_EFETCH_URL, &query).await?;
    if looks_like_ncbi_empty_response(&raw_text) {
        return Err(FunctionCallError::RespondToModel(format!(
            "NCBI returned no PubMed record for {pmid}: {}",
            raw_text.trim()
        )));
    }

    let fields = parse_medline_fields(&raw_text);
    if fields.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "NCBI returned an unparsable MEDLINE record for {pmid}"
        )));
    }

    let mut mesh_terms = medline_all(&fields, "MH");
    let mesh_term_count = mesh_terms.len();
    mesh_terms.truncate(max_mesh_terms);

    Ok(PubmedMapDetails {
        title: medline_first(&fields, "TI").unwrap_or_default(),
        authors: medline_all(&fields, "FAU"),
        journal: medline_first(&fields, "JT")
            .or_else(|| medline_first(&fields, "TA"))
            .unwrap_or_default(),
        publication_date: medline_first(&fields, "DP").unwrap_or_default(),
        doi: medline_doi(&fields).unwrap_or_default(),
        abstract_text: medline_first(&fields, "AB").unwrap_or_default(),
        publication_types: medline_all(&fields, "PT"),
        mesh_terms,
        mesh_term_count,
        language: medline_first(&fields, "LA").unwrap_or_default(),
        relations: medline_relations(&fields),
    })
}

fn pubmed_map_record_from_summary(rank: usize, pmid: &str, entry: &Value) -> PubmedMapRecord {
    let authors = entry
        .get("authors")
        .and_then(Value::as_array)
        .map(|authors| {
            authors
                .iter()
                .filter_map(|author| author.get("name").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let doi = entry
        .get("articleids")
        .and_then(Value::as_array)
        .and_then(|ids| {
            ids.iter()
                .find(|id| id.get("idtype").and_then(Value::as_str) == Some("doi"))
                .and_then(|id| id.get("value").and_then(Value::as_str))
        })
        .unwrap_or_default();

    PubmedMapRecord {
        rank,
        literature_id: String::new(),
        review_case_id: None,
        match_method: "not_registered".to_string(),
        vector_status: "not_processed".to_string(),
        expected_points: 0,
        verified_points: 0,
        existing_vector_dataset: None,
        vector_error: None,
        pmid: pmid.to_string(),
        title: entry
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        authors,
        journal: entry
            .get("fulljournalname")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        publication_date: entry
            .get("pubdate")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        doi: doi.to_string(),
        abstract_text: String::new(),
        publication_types: Vec::new(),
        mesh_terms: Vec::new(),
        mesh_term_count: 0,
        language: String::new(),
        relations: Vec::new(),
        fetch_error: None,
        citation_validation: None,
    }
}

fn replace_non_empty(target: &mut String, value: String) {
    if !value.trim().is_empty() {
        *target = value;
    }
}

fn metadata_string_array(metadata: &Value, key: &str) -> Vec<String> {
    metadata
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect()
}

fn publication_date_precision(value: &str) -> &'static str {
    let parts = value
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .count();
    match parts {
        0 => "unknown",
        1 => "year",
        2 => "month",
        _ => "day",
    }
}

fn has_publication_type(publication_types: &[String], needle: &str) -> bool {
    publication_types
        .iter()
        .any(|publication_type| publication_type.to_ascii_lowercase().contains(needle))
}

async fn validate_pubmed_map_citations(
    client: &reqwest::Client,
    records: &mut [PubmedMapRecord],
) -> PubmedCitationValidationSummary {
    let mut summary = PubmedCitationValidationSummary {
        enabled: true,
        ..PubmedCitationValidationSummary::default()
    };

    for record in records {
        if record.doi.trim().is_empty() {
            record.citation_validation = Some(PubmedCitationValidation {
                status: "missing_doi".to_string(),
                warning: "PubMed record has no DOI to validate against Crossref".to_string(),
                ..PubmedCitationValidation::default()
            });
            summary.missing_doi += 1;
            continue;
        }

        if summary.checked > 0 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        summary.checked += 1;

        let citation = CitationToCheck {
            doi: record.doi.clone(),
            claimed_title: (!record.title.trim().is_empty()).then(|| record.title.clone()),
            // PubMed and Crossref author formatting differs, so use the first
            // few surnames as anchors instead of requiring long author lists.
            claimed_authors: record.authors.iter().take(3).cloned().collect(),
        };
        let validation = match validate_one_citation(client, &citation).await {
            Ok(verdict) => pubmed_citation_validation_from_crossref(record, &verdict),
            Err(err) => PubmedCitationValidation {
                status: "validation_error".to_string(),
                warning: err.to_string(),
                ..PubmedCitationValidation::default()
            },
        };
        update_pubmed_citation_validation_summary(&mut summary, &validation.status);
        record.citation_validation = Some(validation);
    }

    summary
}

fn pubmed_citation_validation_from_crossref(
    record: &PubmedMapRecord,
    verdict: &Value,
) -> PubmedCitationValidation {
    let mut status = verdict
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("validation_error")
        .to_string();
    let title_match = verdict.get("title_match").and_then(Value::as_bool);
    let authors_match = verdict.get("authors_match").and_then(Value::as_bool);
    let resolved_year = verdict.get("resolved_year").and_then(Value::as_u64);
    let year_match = pubmed_year_from_date(&record.publication_date)
        .and_then(|year| year.parse::<u64>().ok())
        .zip(resolved_year)
        .map(|(pubmed_year, crossref_year)| pubmed_year == crossref_year);

    if year_match == Some(false) && status == "verified" {
        status = "mismatch".to_string();
    }

    let warning = pubmed_citation_warning(&status, title_match, authors_match, year_match, verdict);

    PubmedCitationValidation {
        status,
        warning,
        title_match,
        authors_match,
        year_match,
        resolved_title: verdict
            .get("resolved_title")
            .and_then(Value::as_str)
            .map(ToString::to_string),
        resolved_year,
    }
}

fn pubmed_citation_warning(
    status: &str,
    title_match: Option<bool>,
    authors_match: Option<bool>,
    year_match: Option<bool>,
    verdict: &Value,
) -> String {
    let mut warnings = Vec::new();
    if title_match == Some(false) {
        warnings.push("title mismatch".to_string());
    }
    if authors_match == Some(false) {
        warnings.push("author mismatch".to_string());
    }
    if year_match == Some(false) {
        warnings.push("year mismatch".to_string());
    }
    match status {
        "doi_not_found" => warnings.push("Crossref has no record for this DOI".to_string()),
        "invalid_doi" => warnings.push(
            verdict
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("invalid DOI")
                .to_string(),
        ),
        _ => {}
    }
    warnings.join("; ")
}

fn update_pubmed_citation_validation_summary(
    summary: &mut PubmedCitationValidationSummary,
    status: &str,
) {
    match status {
        "verified" => summary.verified += 1,
        "mismatch" => summary.mismatches += 1,
        "doi_not_found" | "invalid_doi" => summary.unresolved += 1,
        "missing_doi" => summary.missing_doi += 1,
        "validation_error" => summary.errors += 1,
        _ => summary.errors += 1,
    }
}

fn ncbi_common_query() -> Vec<(&'static str, String)> {
    let mut query = vec![("tool", "codex-for-med".to_string())];
    if let Ok(email) = std::env::var("NCBI_EMAIL")
        && !email.trim().is_empty()
    {
        query.push(("email", email));
    }
    if let Ok(api_key) = std::env::var("NCBI_API_KEY")
        && !api_key.trim().is_empty()
    {
        query.push(("api_key", api_key));
    }
    query
}

fn ncbi_fetch_delay() -> Duration {
    match std::env::var("NCBI_API_KEY") {
        Ok(value) if !value.trim().is_empty() => Duration::from_millis(110),
        _ => Duration::from_millis(350),
    }
}

fn ncbi_retry_backoff(attempt: usize) -> Duration {
    let exponential = 500 * (1_u64 << attempt.min(4));
    let jitter = u64::from(uuid::Uuid::new_v4().as_bytes()[0]);
    Duration::from_millis(exponential + jitter)
}

fn ncbi_retry_after_delay(value: &str, now: chrono::DateTime<chrono::Utc>) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let retry_at = chrono::DateTime::parse_from_rfc2822(value.trim())
        .ok()?
        .with_timezone(&chrono::Utc);
    Some(
        retry_at
            .signed_duration_since(now)
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
}

static NCBI_REQUEST_GATE: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
static NCBI_LAST_REQUEST: std::sync::Mutex<Option<tokio::time::Instant>> =
    std::sync::Mutex::new(None);

async fn wait_for_ncbi_request_slot() -> Result<(), FunctionCallError> {
    let _permit = NCBI_REQUEST_GATE
        .acquire()
        .await
        .map_err(|_| FunctionCallError::Fatal("NCBI request gate was closed".to_string()))?;
    let last_request = {
        *NCBI_LAST_REQUEST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    if let Some(last_request) = last_request {
        tokio::time::sleep_until(last_request + ncbi_fetch_delay()).await;
    }
    *NCBI_LAST_REQUEST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(tokio::time::Instant::now());
    Ok(())
}

async fn ncbi_get_with_retry(
    client: &reqwest::Client,
    url: &str,
    query: &[(&str, String)],
    stats: &mut NcbiRequestStats,
) -> Result<reqwest::Response, FunctionCallError> {
    const MAX_RETRIES: usize = 5;
    for attempt in 0..=MAX_RETRIES {
        wait_for_ncbi_request_slot().await?;
        stats.network_attempts += 1;
        match client
            .get(url)
            .query(query)
            .timeout(Duration::from_secs(60))
            .send()
            .await
        {
            Ok(response)
                if (response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || response.status().is_server_error())
                    && attempt < MAX_RETRIES =>
            {
                if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    stats.throttled_responses += 1;
                } else {
                    stats.server_error_responses += 1;
                }
                let retry_after = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| ncbi_retry_after_delay(value, chrono::Utc::now()))
                    .unwrap_or_else(|| ncbi_retry_backoff(attempt));
                let _ = response.bytes().await;
                stats.retries += 1;
                tokio::time::sleep(retry_after).await;
            }
            Ok(response) => return Ok(response),
            Err(error)
                if attempt < MAX_RETRIES
                    && (error.is_timeout() || error.is_connect() || error.is_request()) =>
            {
                stats.transient_network_errors += 1;
                stats.retries += 1;
                tokio::time::sleep(ncbi_retry_backoff(attempt)).await;
                tracing::warn!(
                    attempt = attempt + 1,
                    error = %error,
                    "retrying transient NCBI request error"
                );
            }
            Err(error) => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "PubMed request failed after {} attempts: {error}",
                    attempt + 1
                )));
            }
        }
    }
    unreachable!("NCBI retry loop always returns on its final attempt")
}

fn pubmed_map_year_range(
    min_year: Option<u32>,
    max_year: Option<u32>,
) -> Result<Option<(u32, u32)>, FunctionCallError> {
    match (min_year, max_year) {
        (None, None) => Ok(None),
        (Some(min_year), Some(max_year)) if min_year <= max_year => Ok(Some((min_year, max_year))),
        (Some(min_year), Some(max_year)) => Err(FunctionCallError::RespondToModel(format!(
            "min_year ({min_year}) must not be greater than max_year ({max_year})"
        ))),
        _ => Err(FunctionCallError::RespondToModel(
            "min_year and max_year must be provided together".to_string(),
        )),
    }
}

fn pubmed_map_year_range_json(min_year: Option<u32>, max_year: Option<u32>) -> Value {
    match (min_year, max_year) {
        (Some(min_year), Some(max_year)) => json!({
            "datetype": "pdat",
            "mindate": min_year,
            "maxdate": max_year,
        }),
        _ => Value::Null,
    }
}

fn pubmed_map_query_degradations(esearch_json: &Value) -> Vec<String> {
    let mut notes = Vec::new();

    if let Some(errors) = esearch_json.pointer("/esearchresult/errorlist") {
        for (field, label) in [
            ("phrasesnotfound", "phrases not found"),
            ("fieldsnotfound", "field tags not found"),
        ] {
            if let Some(items) = errors.get(field).and_then(Value::as_array)
                && !items.is_empty()
            {
                let joined = items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                notes.push(format!("{label}: {joined}"));
            }
        }
        if notes.is_empty() {
            notes.push(
                "PubMed reported an errorlist without details; compare query_translation against the intended query."
                    .to_string(),
            );
        }
    }

    if let Some(messages) = esearch_json
        .pointer("/esearchresult/warninglist/outputmessages")
        .and_then(Value::as_array)
    {
        notes.extend(
            messages
                .iter()
                .filter_map(Value::as_str)
                .map(ToString::to_string),
        );
    }

    notes
}

async fn http_get_json_with_query(
    client: &reqwest::Client,
    cache: &PubmedCache,
    stats: &mut NcbiRequestStats,
    url: &str,
    query: &[(&str, String)],
    label: &str,
) -> Result<Value, FunctionCallError> {
    let text = http_get_text_with_query(client, cache, stats, url, query).await?;
    serde_json::from_str(&text).map_err(|err| {
        FunctionCallError::RespondToModel(format!("{label} response was not valid JSON: {err}"))
    })
}

async fn http_get_text_with_query(
    client: &reqwest::Client,
    cache: &PubmedCache,
    stats: &mut NcbiRequestStats,
    url: &str,
    query: &[(&str, String)],
) -> Result<String, FunctionCallError> {
    stats.logical_requests += 1;
    if let Some(cached) = cache.get(url, query) {
        stats.cache_hits += 1;
        return Ok(cached);
    }
    let response = ncbi_get_with_retry(client, url, query, stats).await?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(http_error("failed to read PubMed response"))?;
    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "PubMed request returned HTTP {status}: {}",
            truncate_for_error(&text)
        )));
    }
    if let Err(error) = cache.put(url, query, &text) {
        tracing::warn!(error = %error, "failed to update PubMed response cache");
    }
    Ok(text)
}

fn looks_like_ncbi_empty_response(text: &str) -> bool {
    let text = text.trim();
    text.is_empty()
        || text.starts_with("Error:")
        || text.contains("Failed to retrieve sequence")
        || text.contains("Nothing has been found")
}

fn parse_medline_fields(text: &str) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if line.starts_with(' ') {
            if let Some((_, value)) = fields.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        if let Some((tag, value)) = split_medline_line(line) {
            fields.push((tag, value));
        }
    }
    fields
}

fn split_medline_line(line: &str) -> Option<(String, String)> {
    let separator = line.get(4..5)?;
    if separator != "-" {
        return None;
    }
    let tag = line.get(..4)?.trim();
    if tag.is_empty() {
        return None;
    }
    let value = line.get(5..).unwrap_or_default().trim();
    Some((tag.to_string(), value.to_string()))
}

fn medline_first(fields: &[(String, String)], tag: &str) -> Option<String> {
    fields
        .iter()
        .find(|(field_tag, _)| field_tag == tag)
        .map(|(_, value)| value.clone())
}

fn medline_all(fields: &[(String, String)], tag: &str) -> Vec<String> {
    fields
        .iter()
        .filter(|(field_tag, _)| field_tag == tag)
        .map(|(_, value)| value.clone())
        .collect()
}

fn medline_doi(fields: &[(String, String)]) -> Option<String> {
    fields
        .iter()
        .filter(|(tag, _)| tag == "AID" || tag == "LID")
        .find_map(|(_, value)| {
            value
                .strip_suffix("[doi]")
                .map(|doi| doi.trim().to_string())
                .filter(|doi| !doi.is_empty())
        })
}

fn medline_relations(fields: &[(String, String)]) -> Vec<Value> {
    fields
        .iter()
        .filter_map(|(tag, value)| {
            let relation = match tag.as_str() {
                "RIN" => "retracted_in",
                "ROF" => "retraction_of",
                "CRI" => "corrected_and_republished_in",
                "CRF" => "corrected_and_republished_from",
                "EIN" => "erratum_in",
                "EFR" | "EON" => "erratum_for",
                "CIN" => "comment_in",
                "CON" => "comment_on",
                "UIN" => "update_in",
                "UOF" => "update_of",
                _ => return None,
            };
            let pmid = value
                .rfind("PMID:")
                .and_then(|offset| value.get(offset + 5..))
                .map(str::trim_start)
                .and_then(|value| {
                    let digits = value
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>();
                    (!digits.is_empty()).then_some(digits)
                });
            Some(json!({
                "type": relation,
                "pmid": pmid,
                "raw": value,
            }))
        })
        .collect()
}

fn render_pubmed_report(
    topic: &str,
    pubmed_query: &str,
    search: &PubmedMapSearchResult,
    citation_validation: &PubmedCitationValidationSummary,
    ncbi_stats: &NcbiRequestStats,
) -> String {
    let mut out = String::new();
    out.push_str("# PubMed Literature Map\n\n");
    out.push_str(
        "> Content level: bibliographic metadata and abstracts (题录/摘要级内容), not full text.\n\n",
    );
    out.push_str(&format!("Topic: {topic}\n\n"));
    out.push_str(&format!("PubMed query: `{pubmed_query}`\n\n"));
    if let Some(translation) = &search.query_translation
        && !translation.trim().is_empty()
    {
        out.push_str(&format!("Query translation: `{translation}`\n\n"));
    }
    out.push_str(&format!(
        "Total PubMed hits: {}  \nReturned records: {}  \nDropped by esummary: {}\n\n",
        search
            .total_count
            .map(|count| count.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        search.records.len(),
        search.dropped_by_esummary
    ));
    out.push_str(&format!(
        "NCBI logical requests: {}  \nCache hits: {}  \nNetwork attempts: {}  \nRetries: {}\n\n",
        ncbi_stats.logical_requests,
        ncbi_stats.cache_hits,
        ncbi_stats.network_attempts,
        ncbi_stats.retries,
    ));
    if !search.query_degraded.is_empty() {
        out.push_str("## Query Warnings\n\n");
        for warning in &search.query_degraded {
            out.push_str(&format!("- {}\n", markdown_escape(warning)));
        }
        out.push('\n');
    }
    if citation_validation.enabled {
        out.push_str("## Citation Validation\n\n");
        out.push_str(&format!(
            "Checked DOI-bearing records: {}  \nVerified: {}  \nMismatches: {}  \nUnresolved DOI records: {}  \nMissing DOI records: {}  \nValidation errors: {}\n\n",
            citation_validation.checked,
            citation_validation.verified,
            citation_validation.mismatches,
            citation_validation.unresolved,
            citation_validation.missing_doi,
            citation_validation.errors,
        ));
        let warnings = search
            .records
            .iter()
            .filter_map(|record| {
                record
                    .citation_validation
                    .as_ref()
                    .filter(|validation| !validation.warning.trim().is_empty())
                    .map(|validation| (record, validation))
            })
            .collect::<Vec<_>>();
        if !warnings.is_empty() {
            out.push_str("| PMID | DOI | Status | Warning |\n");
            out.push_str("| --- | --- | --- | --- |\n");
            for (record, validation) in warnings {
                out.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    markdown_escape(&record.pmid),
                    markdown_escape(&record.doi),
                    markdown_escape(&validation.status),
                    markdown_escape(&validation.warning),
                ));
            }
            out.push('\n');
        }
    }

    out.push_str("## PubMed Records\n\n");
    out.push_str(
        "| Rank | Literature ID | PMID | Year | Title | Journal | DOI | Citation | Vector |\n",
    );
    out.push_str("| ---: | --- | --- | --- | --- | --- | --- | --- | --- |\n");
    for record in &search.records {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
            record.rank,
            markdown_escape(&record.literature_id),
            markdown_escape(&record.pmid),
            markdown_escape(&pubmed_year_from_date(&record.publication_date).unwrap_or_default()),
            markdown_escape(&record.title),
            markdown_escape(&record.journal),
            markdown_escape(&record.doi),
            markdown_escape(
                record
                    .citation_validation
                    .as_ref()
                    .map(|validation| validation.status.as_str())
                    .unwrap_or("")
            ),
            markdown_escape(&record.vector_status),
        ));
    }

    out.push_str("\n## Abstracts And MeSH\n\n");
    for record in &search.records {
        out.push_str(&format!(
            "### {}. PMID:{} - {}\n\n",
            record.rank,
            record.pmid,
            if record.title.is_empty() {
                "Untitled"
            } else {
                &record.title
            }
        ));
        out.push_str(&format!(
            "Journal: `{}`  \nDate: `{}`  \nDOI: `{}`\n\n",
            record.journal, record.publication_date, record.doi
        ));
        if !record.authors.is_empty() {
            out.push_str(&format!(
                "Authors: {}\n\n",
                markdown_escape(&record.authors.join("; "))
            ));
        }
        if !record.publication_types.is_empty() {
            out.push_str(&format!(
                "Publication types: {}\n\n",
                markdown_escape(&record.publication_types.join("; "))
            ));
        }
        if !record.mesh_terms.is_empty() {
            out.push_str(&format!(
                "MeSH terms: {}\n\n",
                markdown_escape(&record.mesh_terms.join("; "))
            ));
        }
        if !record.relations.is_empty() {
            let relations = record
                .relations
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            out.push_str(&format!(
                "Related records: {}\n\n",
                markdown_escape(&relations)
            ));
        }
        if let Some(error) = &record.fetch_error {
            out.push_str(&format!("Fetch warning: {}\n\n", markdown_escape(error)));
        }
        if record.abstract_text.trim().is_empty() {
            out.push_str("_No abstract fetched or available._\n\n");
        } else {
            out.push_str(&record.abstract_text);
            out.push_str("\n\n");
        }
    }
    out.push_str("## Notes For Human Curation\n\n");
    out.push_str("- This workflow uses live PubMed metadata and optional MEDLINE details; verify query_translation before treating field filters as applied.\n");
    out.push_str("- `literature_ids.csv` is the canonical ranked output; metadata is resolved from the workspace literature registry.\n");
    out.push_str(
        "- Enable `validate_citations` for DOI/title/author/year checks before manuscript use.\n",
    );
    out
}

fn render_pubmed_citations_bib(records: &[PubmedMapRecord]) -> String {
    let mut out = String::new();
    for record in records
        .iter()
        .filter(|record| !record.literature_id.is_empty())
    {
        let key = format!(
            "lit_{}",
            record
                .literature_id
                .chars()
                .filter(char::is_ascii_hexdigit)
                .take(12)
                .collect::<String>()
        );
        out.push_str(&format!(
            "@article{{{},\n  title = {{{}}},\n",
            key,
            bib_escape(&record.title)
        ));
        if !record.journal.is_empty() {
            out.push_str(&format!(
                "  journal = {{{}}},\n",
                bib_escape(&record.journal)
            ));
        }
        if let Some(year) = pubmed_year_from_date(&record.publication_date) {
            out.push_str(&format!("  year = {{{}}},\n", bib_escape(&year)));
        }
        if !record.authors.is_empty() {
            out.push_str(&format!(
                "  author = {{{}}},\n",
                bib_escape(&record.authors.join(" and "))
            ));
        }
        if !record.doi.is_empty() {
            out.push_str(&format!("  doi = {{{}}},\n", bib_escape(&record.doi)));
        }
        out.push_str(&format!("  note = {{PMID:{}}}\n}}\n\n", record.pmid));
    }
    out
}

fn pubmed_year_from_date(value: &str) -> Option<String> {
    let year = value.trim().chars().take(4).collect::<String>();
    if year.len() == 4 && year.chars().all(|ch| ch.is_ascii_digit()) {
        Some(year)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pubmed_year_range_requires_complete_ordered_window() {
        assert_eq!(pubmed_map_year_range(None, None).ok(), Some(None));
        assert_eq!(
            pubmed_map_year_range(Some(2015), Some(2026)).ok(),
            Some(Some((2015, 2026)))
        );
        assert!(pubmed_map_year_range(Some(2015), None).is_err());
        assert!(pubmed_map_year_range(None, Some(2026)).is_err());
        assert!(pubmed_map_year_range(Some(2026), Some(2015)).is_err());
    }

    #[test]
    fn parses_pubmed_medline_details() {
        let medline = concat!(
            "PMID- 33301246\n",
            "TI  - Integrated stress response in disease.\n",
            "AB  - First sentence.\n",
            "      Continued sentence.\n",
            "FAU - Doe, Jane\n",
            "FAU - Smith, John\n",
            "JT  - Journal of Test Biology\n",
            "DP  - 2020 Dec\n",
            "AID - 10.1000/test.2020.1 [doi]\n",
            "PT  - Journal Article\n",
            "MH  - Stress, Physiological\n",
            "MH  - Neurodegenerative Diseases\n",
            "LA  - eng\n",
            "ROF - Journal citation. PMID: 12345678\n",
        );
        let fields = parse_medline_fields(medline);
        assert_eq!(medline_first(&fields, "PMID").as_deref(), Some("33301246"));
        assert_eq!(
            medline_first(&fields, "AB").as_deref(),
            Some("First sentence. Continued sentence.")
        );
        assert_eq!(medline_all(&fields, "FAU").len(), 2);
        assert_eq!(medline_doi(&fields).as_deref(), Some("10.1000/test.2020.1"));
        assert_eq!(medline_relations(&fields)[0]["pmid"], "12345678");
    }

    #[test]
    fn parses_retry_after_seconds_and_http_date() {
        let now = chrono::DateTime::parse_from_rfc3339("2015-10-21T07:27:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc);
        assert_eq!(
            ncbi_retry_after_delay("120", now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(
            ncbi_retry_after_delay("Wed, 21 Oct 2015 07:28:00 GMT", now),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            ncbi_retry_after_delay("Wed, 21 Oct 2015 07:26:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(ncbi_retry_after_delay("invalid", now), None);
    }

    #[test]
    fn renders_stable_pubmed_citation() {
        let records = vec![PubmedMapRecord {
            rank: 1,
            literature_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            review_case_id: None,
            match_method: "strong_identifier".to_string(),
            vector_status: "already_vectorized".to_string(),
            expected_points: 1,
            verified_points: 1,
            existing_vector_dataset: Some("data-extract-new".to_string()),
            vector_error: None,
            pmid: "33301246".to_string(),
            title: "Integrated stress response, aging, and disease".to_string(),
            authors: vec!["Doe, Jane".to_string(), "Smith, John".to_string()],
            journal: "Journal of Test Biology".to_string(),
            publication_date: "2020 Dec".to_string(),
            doi: "10.1000/test.2020.1".to_string(),
            abstract_text: "A concise abstract.".to_string(),
            publication_types: vec!["Journal Article".to_string()],
            mesh_terms: vec!["Stress, Physiological".to_string()],
            mesh_term_count: 1,
            language: "eng".to_string(),
            relations: Vec::new(),
            fetch_error: None,
            citation_validation: Some(PubmedCitationValidation {
                status: "verified".to_string(),
                warning: String::new(),
                title_match: Some(true),
                authors_match: Some(true),
                year_match: Some(true),
                resolved_title: Some("Integrated stress response, aging, and disease".to_string()),
                resolved_year: Some(2020),
            }),
        }];
        let bib = render_pubmed_citations_bib(&records);
        assert!(bib.contains("@article{lit_550e8400e29b"));
        assert!(bib.contains("doi = {10.1000/test.2020.1}"));
    }

    #[test]
    fn converts_crossref_verdict_to_pubmed_citation_validation() {
        let record = PubmedMapRecord {
            rank: 1,
            literature_id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            review_case_id: None,
            match_method: "strong_identifier".to_string(),
            vector_status: "not_processed".to_string(),
            expected_points: 0,
            verified_points: 0,
            existing_vector_dataset: None,
            vector_error: None,
            pmid: "33301246".to_string(),
            title: "Integrated stress response, aging, and disease".to_string(),
            authors: vec!["Doe, Jane".to_string()],
            journal: "Journal of Test Biology".to_string(),
            publication_date: "2020 Dec".to_string(),
            doi: "10.1000/test.2020.1".to_string(),
            abstract_text: String::new(),
            publication_types: Vec::new(),
            mesh_terms: Vec::new(),
            mesh_term_count: 0,
            language: String::new(),
            relations: Vec::new(),
            fetch_error: None,
            citation_validation: None,
        };
        let validation = pubmed_citation_validation_from_crossref(
            &record,
            &json!({
                "status": "verified",
                "resolved_title": "Integrated stress response, aging, and disease",
                "resolved_authors": ["Jane Doe"],
                "resolved_year": 2021,
                "title_match": true,
                "authors_match": true
            }),
        );

        assert_eq!(validation.status, "mismatch");
        assert_eq!(validation.title_match, Some(true));
        assert_eq!(validation.authors_match, Some(true));
        assert_eq!(validation.year_match, Some(false));
        assert!(validation.warning.contains("year mismatch"));
    }
}
