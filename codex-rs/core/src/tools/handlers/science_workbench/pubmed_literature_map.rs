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

    let mut search =
        pubmed_map_search(client, pubmed_query, retmax, args.sort, year_window).await?;

    let mut fetch_errors = 0usize;
    if args.fetch_abstracts {
        let delay = ncbi_fetch_delay();
        for idx in 0..search.records.len() {
            if idx > 0 {
                tokio::time::sleep(delay).await;
            }
            match fetch_pubmed_map_details(client, &search.records[idx].pmid, max_mesh_terms).await
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

    let project_dir = cwd.join("research_projects").join(&project_id);
    let literature_dir = project_dir.join("literature");
    let code_dir = project_dir.join("code");
    let provenance_dir = project_dir.join("provenance");
    let figures_dir = project_dir.join("figures");
    let analysis_dir = project_dir.join("analysis");
    fs::create_dir_all(&literature_dir).map_err(fs_error("create literature directory"))?;
    fs::create_dir_all(&code_dir).map_err(fs_error("create code directory"))?;
    fs::create_dir_all(&provenance_dir).map_err(fs_error("create provenance directory"))?;
    fs::create_dir_all(&figures_dir).map_err(fs_error("create figures directory"))?;
    fs::create_dir_all(&analysis_dir).map_err(fs_error("create analysis directory"))?;

    let records_csv = render_pubmed_records_csv(&search.records);
    let records_jsonl = render_pubmed_records_jsonl(&search.records)?;
    let report = render_pubmed_report(topic, pubmed_query, &search, &citation_validation);
    let citations = render_pubmed_citations_bib(&search.records);

    let now = chrono::Utc::now();
    let now_rfc3339 = now.to_rfc3339();
    let run_id = format!("{}_pubmed_literature_map", now.format("%Y-%m-%dT%H%M%SZ"));

    let run = json!({
        "workflow": "pubmed_literature_map",
        "run_id": run_id,
        "run_at": now_rfc3339,
        "project_id": project_id,
        "topic": topic,
        "pubmed_query": pubmed_query,
        "sort": args.sort.as_esearch_sort(),
        "year_range": pubmed_map_year_range_json(args.min_year, args.max_year),
        "retmax_effective": retmax,
        "fetch_abstracts": args.fetch_abstracts,
        "max_mesh_terms": max_mesh_terms,
        "validate_citations": args.validate_citations,
        "backends": {
            "pubmed": {
                "provider": "NCBI E-utilities",
                "esearch_url": NCBI_ESEARCH_URL,
                "esummary_url": NCBI_ESUMMARY_URL,
                "efetch_url": NCBI_EFETCH_URL
            }
        },
        "query_translation": search.query_translation,
        "query_degraded": search.query_degraded,
        "total_count": search.total_count,
        "returned": search.records.len(),
        "dropped_by_esummary": search.dropped_by_esummary,
        "fetch_errors": fetch_errors,
        "citation_validation": citation_validation.to_json(),
        "outputs": {
            "pubmed_records_csv": relative_display(&project_dir, &literature_dir.join("pubmed_records.csv")),
            "pubmed_records_jsonl": relative_display(&project_dir, &literature_dir.join("pubmed_records.jsonl")),
            "report": relative_display(&project_dir, &literature_dir.join("report.md")),
            "citations": relative_display(&project_dir, &literature_dir.join("citations.bib"))
        }
    });

    write_file(&literature_dir.join("pubmed_records.csv"), &records_csv)?;
    write_file(&literature_dir.join("pubmed_records.jsonl"), &records_jsonl)?;
    write_file(&literature_dir.join("report.md"), &report)?;
    write_file(&literature_dir.join("citations.bib"), &citations)?;
    write_file(
        &provenance_dir.join("run.json"),
        &serde_json::to_string_pretty(&run).map_err(json_error("serialize run provenance"))?,
    )?;

    let manifest_path = record_project_run(
        &project_dir,
        &project_id,
        topic,
        &now_rfc3339,
        run,
        &provenance_dir.join("run.json"),
        &[
            (
                "pubmed_records_csv",
                literature_dir.join("pubmed_records.csv").as_path(),
            ),
            (
                "pubmed_records_jsonl",
                literature_dir.join("pubmed_records.jsonl").as_path(),
            ),
            ("report", literature_dir.join("report.md").as_path()),
            ("citations", literature_dir.join("citations.bib").as_path()),
        ],
    )?;

    let output = json!({
        "project_id": project_id,
        "project_dir": project_dir,
        "topic": topic,
        "pubmed_query": pubmed_query,
        "returned": search.records.len(),
        "total_count": search.total_count,
        "fetch_abstracts": args.fetch_abstracts,
        "fetch_errors": fetch_errors,
        "validate_citations": args.validate_citations,
        "citation_validation": citation_validation.to_json(),
        "manifest": manifest_path,
        "created_files": [
            literature_dir.join("pubmed_records.csv"),
            literature_dir.join("pubmed_records.jsonl"),
            literature_dir.join("report.md"),
            literature_dir.join("citations.bib"),
            provenance_dir.join("run.json"),
            manifest_path
        ],
        "created_directories": [
            literature_dir,
            code_dir,
            analysis_dir,
            figures_dir,
            provenance_dir
        ],
        "next_steps": [
            "Review literature/pubmed_records.csv for relevance and missing abstracts.",
            "Check query_translation and query_degraded in provenance/run.json before trusting PubMed filters.",
            "Review citation_status and citation_warning columns before manuscript use."
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

#[derive(Debug, Clone)]
struct PubmedMapRecord {
    rank: usize,
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
        self.mesh_term_count = details.mesh_term_count;
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
}

async fn pubmed_map_search(
    client: &reqwest::Client,
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

    let esearch_json =
        http_get_json_with_query(client, NCBI_ESEARCH_URL, &esearch_query, "PubMed esearch")
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
    pmid: &str,
    max_mesh_terms: usize,
) -> Result<PubmedMapDetails, FunctionCallError> {
    let mut query = ncbi_common_query();
    query.push(("db", "pubmed".to_string()));
    query.push(("id", pmid.to_string()));
    query.push(("rettype", "medline".to_string()));
    query.push(("retmode", "text".to_string()));

    let raw_text = http_get_text_with_query(client, NCBI_EFETCH_URL, &query).await?;
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
        fetch_error: None,
        citation_validation: None,
    }
}

fn replace_non_empty(target: &mut String, value: String) {
    if !value.trim().is_empty() {
        *target = value;
    }
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
    url: &str,
    query: &[(&str, String)],
    label: &str,
) -> Result<Value, FunctionCallError> {
    let response = client
        .get(url)
        .query(query)
        .send()
        .await
        .map_err(http_error("PubMed request failed"))?;
    parse_http_json(response, label).await
}

async fn http_get_text_with_query(
    client: &reqwest::Client,
    url: &str,
    query: &[(&str, String)],
) -> Result<String, FunctionCallError> {
    let response = client
        .get(url)
        .query(query)
        .send()
        .await
        .map_err(http_error("PubMed request failed"))?;
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

fn render_pubmed_records_csv(records: &[PubmedMapRecord]) -> String {
    let mut out =
        "rank,pmid,title,authors,journal,publication_date,year,doi,citation_status,citation_warning,crossref_title_match,crossref_author_match,crossref_year_match,publication_types,mesh_terms,mesh_term_count,language,abstract,fetch_error\n"
            .to_string();
    for record in records {
        let validation = record.citation_validation.as_ref();
        out.push_str(&format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
            record.rank,
            csv_escape(&record.pmid),
            csv_escape(&record.title),
            csv_escape(&record.authors.join("; ")),
            csv_escape(&record.journal),
            csv_escape(&record.publication_date),
            csv_escape(&pubmed_year_from_date(&record.publication_date).unwrap_or_default()),
            csv_escape(&record.doi),
            csv_escape(validation.map(|v| v.status.as_str()).unwrap_or_default()),
            csv_escape(validation.map(|v| v.warning.as_str()).unwrap_or_default()),
            csv_escape(&validation_bool(validation.and_then(|v| v.title_match))),
            csv_escape(&validation_bool(validation.and_then(|v| v.authors_match))),
            csv_escape(&validation_bool(validation.and_then(|v| v.year_match))),
            csv_escape(&record.publication_types.join("; ")),
            csv_escape(&record.mesh_terms.join("; ")),
            record.mesh_term_count,
            csv_escape(&record.language),
            csv_escape(&record.abstract_text),
            csv_escape(record.fetch_error.as_deref().unwrap_or_default()),
        ));
    }
    out
}

fn render_pubmed_records_jsonl(records: &[PubmedMapRecord]) -> Result<String, FunctionCallError> {
    let mut out = String::new();
    for record in records {
        let line = serde_json::to_string(&pubmed_record_json(record))
            .map_err(json_error("serialize PubMed JSONL record"))?;
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

fn render_pubmed_report(
    topic: &str,
    pubmed_query: &str,
    search: &PubmedMapSearchResult,
    citation_validation: &PubmedCitationValidationSummary,
) -> String {
    let mut out = String::new();
    out.push_str("# PubMed Literature Map\n\n");
    out.push_str(&format!("Topic: {topic}\n\n"));
    out.push_str(&format!("PubMed query: `{}`\n\n", pubmed_query));
    if let Some(translation) = &search.query_translation
        && !translation.trim().is_empty()
    {
        out.push_str(&format!("Query translation: `{}`\n\n", translation));
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
    out.push_str("| Rank | PMID | Year | Title | Journal | DOI | Citation |\n");
    out.push_str("| ---: | --- | --- | --- | --- | --- | --- |\n");
    for record in &search.records {
        out.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} | {} |\n",
            record.rank,
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
    out.push_str("- `pubmed_records.csv` is intended for screening and annotation.\n");
    out.push_str(
        "- Enable `validate_citations` for DOI/title/author/year checks before manuscript use.\n",
    );
    out
}

fn render_pubmed_citations_bib(records: &[PubmedMapRecord]) -> String {
    let mut out = String::new();
    for record in records {
        let key = format!("pmid_{}", sanitize_bib_key(&record.pmid));
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

fn pubmed_record_json(record: &PubmedMapRecord) -> Value {
    json!({
        "rank": record.rank,
        "pmid": record.pmid,
        "title": record.title,
        "authors": record.authors,
        "journal": record.journal,
        "publication_date": record.publication_date,
        "year": pubmed_year_from_date(&record.publication_date),
        "doi": record.doi,
        "abstract": record.abstract_text,
        "publication_types": record.publication_types,
        "mesh_terms": record.mesh_terms,
        "mesh_term_count": record.mesh_term_count,
        "language": record.language,
        "fetch_error": record.fetch_error,
        "citation_validation": record.citation_validation.as_ref().map(pubmed_citation_validation_json),
    })
}

fn pubmed_citation_validation_json(validation: &PubmedCitationValidation) -> Value {
    json!({
        "status": validation.status,
        "warning": validation.warning,
        "title_match": validation.title_match,
        "authors_match": validation.authors_match,
        "year_match": validation.year_match,
        "resolved_title": validation.resolved_title,
        "resolved_year": validation.resolved_year,
    })
}

fn pubmed_year_from_date(value: &str) -> Option<String> {
    let year = value.trim().chars().take(4).collect::<String>();
    if year.len() == 4 && year.chars().all(|ch| ch.is_ascii_digit()) {
        Some(year)
    } else {
        None
    }
}

fn validation_bool(value: Option<bool>) -> String {
    value.map(|value| value.to_string()).unwrap_or_default()
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
        );
        let fields = parse_medline_fields(medline);
        assert_eq!(medline_first(&fields, "PMID").as_deref(), Some("33301246"));
        assert_eq!(
            medline_first(&fields, "AB").as_deref(),
            Some("First sentence. Continued sentence.")
        );
        assert_eq!(medline_all(&fields, "FAU").len(), 2);
        assert_eq!(medline_doi(&fields).as_deref(), Some("10.1000/test.2020.1"));
    }

    #[test]
    fn renders_pubmed_records_outputs() {
        let records = vec![PubmedMapRecord {
            rank: 1,
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
        let csv = render_pubmed_records_csv(&records);
        assert!(csv.starts_with("rank,pmid,title,authors"));
        assert!(csv.contains("citation_status"));
        assert!(csv.contains("verified"));
        assert!(csv.contains("33301246"));
        assert!(csv.contains("2020"));

        let jsonl = render_pubmed_records_jsonl(&records).unwrap();
        assert!(jsonl.contains("\"pmid\":\"33301246\""));
        assert!(jsonl.contains("\"citation_validation\""));

        let bib = render_pubmed_citations_bib(&records);
        assert!(bib.contains("@article{pmid_33301246"));
        assert!(bib.contains("doi = {10.1000/test.2020.1}"));
    }

    #[test]
    fn converts_crossref_verdict_to_pubmed_citation_validation() {
        let record = PubmedMapRecord {
            rank: 1,
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
