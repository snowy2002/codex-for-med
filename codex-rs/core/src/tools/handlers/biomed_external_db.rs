use std::time::Duration;

use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::handlers::biomed_external_db_spec::FETCH_GENBANK_RECORD_TOOL_NAME;
use crate::tools::handlers::biomed_external_db_spec::FETCH_PDB_ENTRY_TOOL_NAME;
use crate::tools::handlers::biomed_external_db_spec::FETCH_PUBMED_RECORD_TOOL_NAME;
use crate::tools::handlers::biomed_external_db_spec::FETCH_UNIPROT_ENTRY_TOOL_NAME;
use crate::tools::handlers::biomed_external_db_spec::SEARCH_PUBMED_LITERATURE_TOOL_NAME;
use crate::tools::handlers::biomed_external_db_spec::SEARCH_UNIPROT_TOOL_NAME;
use crate::tools::handlers::biomed_external_db_spec::VALIDATE_CITATIONS_TOOL_NAME;
use crate::tools::handlers::biomed_external_db_spec::create_fetch_genbank_record_tool;
use crate::tools::handlers::biomed_external_db_spec::create_fetch_pdb_entry_tool;
use crate::tools::handlers::biomed_external_db_spec::create_fetch_pubmed_record_tool;
use crate::tools::handlers::biomed_external_db_spec::create_fetch_uniprot_entry_tool;
use crate::tools::handlers::biomed_external_db_spec::create_search_pubmed_literature_tool;
use crate::tools::handlers::biomed_external_db_spec::create_search_uniprot_tool;
use crate::tools::handlers::biomed_external_db_spec::create_validate_citations_tool;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

#[path = "biomed_external_db/citation_validation.rs"]
mod citation_validation;
#[path = "biomed_external_db/pubmed.rs"]
mod pubmed;

pub(crate) use self::citation_validation::CitationToCheck;
use self::citation_validation::ValidateCitationsArgs;
use self::citation_validation::validate_citations;
pub(crate) use self::citation_validation::validate_one_citation;
use self::pubmed::FetchPubmedRecordArgs;
use self::pubmed::SearchPubmedLiteratureArgs;
use self::pubmed::fetch_pubmed_record;
use self::pubmed::search_pubmed_literature;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = "codex-for-med/biomed-external-db";
const RCSB_ENTRY_BASE: &str = "https://data.rcsb.org/rest/v1/core/entry";
const RCSB_FASTA_BASE: &str = "https://www.rcsb.org/fasta/entry";
const NCBI_EFETCH_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils/efetch.fcgi";
const UNIPROT_ENTRY_BASE: &str = "https://rest.uniprot.org/uniprotkb";
const UNIPROT_SEARCH_URL: &str = "https://rest.uniprot.org/uniprotkb/search";

#[derive(Clone, Copy)]
enum BiomedExternalDbToolKind {
    FetchPdbEntry,
    FetchGenbankRecord,
    FetchUniprotEntry,
    SearchUniprot,
    SearchPubmedLiterature,
    FetchPubmedRecord,
    ValidateCitations,
}

pub struct BiomedExternalDbHandler {
    kind: BiomedExternalDbToolKind,
}

impl BiomedExternalDbHandler {
    fn new(kind: BiomedExternalDbToolKind) -> Self {
        Self { kind }
    }

    pub fn fetch_pdb_entry() -> Self {
        Self::new(BiomedExternalDbToolKind::FetchPdbEntry)
    }

    pub fn fetch_genbank_record() -> Self {
        Self::new(BiomedExternalDbToolKind::FetchGenbankRecord)
    }

    pub fn fetch_uniprot_entry() -> Self {
        Self::new(BiomedExternalDbToolKind::FetchUniprotEntry)
    }

    pub fn search_uniprot() -> Self {
        Self::new(BiomedExternalDbToolKind::SearchUniprot)
    }

    pub fn search_pubmed_literature() -> Self {
        Self::new(BiomedExternalDbToolKind::SearchPubmedLiterature)
    }

    pub fn fetch_pubmed_record() -> Self {
        Self::new(BiomedExternalDbToolKind::FetchPubmedRecord)
    }

    pub fn validate_citations() -> Self {
        Self::new(BiomedExternalDbToolKind::ValidateCitations)
    }

    fn client() -> Result<reqwest::Client, FunctionCallError> {
        reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|err| FunctionCallError::Fatal(format!("failed to build HTTP client: {err}")))
    }
}

#[async_trait::async_trait]
impl ToolExecutor<ToolInvocation> for BiomedExternalDbHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(match self.kind {
            BiomedExternalDbToolKind::FetchPdbEntry => FETCH_PDB_ENTRY_TOOL_NAME,
            BiomedExternalDbToolKind::FetchGenbankRecord => FETCH_GENBANK_RECORD_TOOL_NAME,
            BiomedExternalDbToolKind::FetchUniprotEntry => FETCH_UNIPROT_ENTRY_TOOL_NAME,
            BiomedExternalDbToolKind::SearchUniprot => SEARCH_UNIPROT_TOOL_NAME,
            BiomedExternalDbToolKind::SearchPubmedLiterature => SEARCH_PUBMED_LITERATURE_TOOL_NAME,
            BiomedExternalDbToolKind::FetchPubmedRecord => FETCH_PUBMED_RECORD_TOOL_NAME,
            BiomedExternalDbToolKind::ValidateCitations => VALIDATE_CITATIONS_TOOL_NAME,
        })
    }

    fn spec(&self) -> ToolSpec {
        match self.kind {
            BiomedExternalDbToolKind::FetchPdbEntry => create_fetch_pdb_entry_tool(),
            BiomedExternalDbToolKind::FetchGenbankRecord => create_fetch_genbank_record_tool(),
            BiomedExternalDbToolKind::FetchUniprotEntry => create_fetch_uniprot_entry_tool(),
            BiomedExternalDbToolKind::SearchUniprot => create_search_uniprot_tool(),
            BiomedExternalDbToolKind::SearchPubmedLiterature => {
                create_search_pubmed_literature_tool()
            }
            BiomedExternalDbToolKind::FetchPubmedRecord => create_fetch_pubmed_record_tool(),
            BiomedExternalDbToolKind::ValidateCitations => create_validate_citations_tool(),
        }
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    async fn handle(
        &self,
        invocation: ToolInvocation,
    ) -> Result<Box<dyn crate::tools::context::ToolOutput>, FunctionCallError> {
        let ToolInvocation { payload, .. } = invocation;
        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(format!(
                    "{} handler received unsupported payload",
                    self.tool_name()
                )));
            }
        };

        let output = match self.kind {
            BiomedExternalDbToolKind::FetchPdbEntry => {
                let args: FetchPdbEntryArgs = parse_arguments(&arguments)?;
                fetch_pdb_entry(args).await?
            }
            BiomedExternalDbToolKind::FetchGenbankRecord => {
                let args: FetchGenbankRecordArgs = parse_arguments(&arguments)?;
                fetch_genbank_record(args).await?
            }
            BiomedExternalDbToolKind::FetchUniprotEntry => {
                let args: FetchUniprotEntryArgs = parse_arguments(&arguments)?;
                fetch_uniprot_entry(args).await?
            }
            BiomedExternalDbToolKind::SearchUniprot => {
                let args: SearchUniprotArgs = parse_arguments(&arguments)?;
                search_uniprot(args).await?
            }
            BiomedExternalDbToolKind::SearchPubmedLiterature => {
                let args: SearchPubmedLiteratureArgs = parse_arguments(&arguments)?;
                search_pubmed_literature(args).await?
            }
            BiomedExternalDbToolKind::FetchPubmedRecord => {
                let args: FetchPubmedRecordArgs = parse_arguments(&arguments)?;
                fetch_pubmed_record(args).await?
            }
            BiomedExternalDbToolKind::ValidateCitations => {
                let args: ValidateCitationsArgs = parse_arguments(&arguments)?;
                validate_citations(args).await?
            }
        };

        Ok(boxed_tool_output(FunctionToolOutput::from_text(
            output,
            Some(true),
        )))
    }
}

impl CoreToolRuntime for BiomedExternalDbHandler {}

#[derive(Debug, Deserialize)]
struct FetchPdbEntryArgs {
    pdb_id: String,
    #[serde(default = "default_true")]
    include_fasta: bool,
    #[serde(default)]
    include_raw: bool,
}

#[derive(Debug, Deserialize)]
struct FetchGenbankRecordArgs {
    accession: String,
    #[serde(default = "default_genbank_db")]
    db: GenbankDb,
    #[serde(default = "default_genbank_format")]
    format: GenbankFormat,
    #[serde(default)]
    include_raw: bool,
    #[serde(default = "default_max_features")]
    max_features: usize,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GenbankDb {
    Auto,
    Nucleotide,
    Protein,
}

impl GenbankDb {
    fn as_entrez_db(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Nucleotide => "nucleotide",
            Self::Protein => "protein",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GenbankFormat {
    Genbank,
    Fasta,
}

impl GenbankFormat {
    fn rettype(self) -> &'static str {
        match self {
            Self::Genbank => "gb",
            Self::Fasta => "fasta",
        }
    }
}

#[derive(Debug, Deserialize)]
struct FetchUniprotEntryArgs {
    accession: String,
    #[serde(default = "default_uniprot_format")]
    format: UniprotFormat,
    #[serde(default)]
    include_raw: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum UniprotFormat {
    Json,
    Fasta,
    Tsv,
}

impl UniprotFormat {
    fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Fasta => "fasta",
            Self::Tsv => "tsv",
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchUniprotArgs {
    query: String,
    #[serde(default = "default_search_size")]
    size: usize,
}

fn default_true() -> bool {
    true
}

fn default_genbank_db() -> GenbankDb {
    GenbankDb::Auto
}

fn default_genbank_format() -> GenbankFormat {
    GenbankFormat::Genbank
}

fn default_uniprot_format() -> UniprotFormat {
    UniprotFormat::Json
}

fn default_max_features() -> usize {
    100
}

fn default_search_size() -> usize {
    10
}

async fn fetch_pdb_entry(args: FetchPdbEntryArgs) -> Result<String, FunctionCallError> {
    let pdb_id = normalize_pdb_id(&args.pdb_id)?;
    let client = BiomedExternalDbHandler::client()?;
    let entry_url = format!("{RCSB_ENTRY_BASE}/{pdb_id}");
    let entry_text = http_get_text(&client, &entry_url).await?;
    let entry_json: Value = serde_json::from_str(&entry_text).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse RCSB PDB entry JSON: {err}"))
    })?;

    let mut result = json!({
        "source": "RCSB PDB",
        "pdb_id": pdb_id,
        "url": entry_url,
        "title": entry_json.pointer("/struct/title").and_then(Value::as_str),
        "experimental_method": first_string_at(&entry_json, "/exptl", "method"),
        "deposition_date": entry_json.pointer("/rcsb_accession_info/deposit_date").and_then(Value::as_str),
        "release_date": entry_json.pointer("/rcsb_accession_info/initial_release_date").and_then(Value::as_str),
    });

    if args.include_fasta {
        let fasta_url = format!("{RCSB_FASTA_BASE}/{pdb_id}/display");
        let fasta_text = http_get_text(&client, &fasta_url).await?;
        result["fasta_url"] = Value::String(fasta_url);
        result["fasta_entries"] = serde_json::to_value(parse_fasta(&fasta_text))
            .map_err(|err| FunctionCallError::Fatal(format!("failed to encode FASTA: {err}")))?;
    }

    if args.include_raw {
        result["raw_entry"] = entry_json;
    }

    to_pretty_json(&result)
}

async fn fetch_genbank_record(args: FetchGenbankRecordArgs) -> Result<String, FunctionCallError> {
    let accession = normalize_accession(&args.accession, "GenBank accession")?;
    let max_features = args.max_features.min(1_000);
    let client = BiomedExternalDbHandler::client()?;
    let dbs = match args.db {
        GenbankDb::Auto => vec![GenbankDb::Protein, GenbankDb::Nucleotide],
        GenbankDb::Nucleotide => vec![GenbankDb::Nucleotide],
        GenbankDb::Protein => vec![GenbankDb::Protein],
    };

    let mut last_error = None;
    for db in dbs {
        match fetch_genbank_record_from_db(&client, &accession, db, args.format).await {
            Ok(raw_text) if !looks_like_ncbi_empty_response(&raw_text) => {
                let mut result = match args.format {
                    GenbankFormat::Genbank => serde_json::to_value(parse_genbank_flatfile(
                        &raw_text,
                        db,
                        &accession,
                        max_features,
                    )),
                    GenbankFormat::Fasta => {
                        serde_json::to_value(parse_genbank_fasta(&raw_text, db, &accession))
                    }
                }
                .map_err(|err| {
                    FunctionCallError::Fatal(format!("failed to encode GenBank response: {err}"))
                })?;
                result["url"] = Value::String(NCBI_EFETCH_URL.to_string());
                result["format"] = Value::String(match args.format {
                    GenbankFormat::Genbank => "genbank".to_string(),
                    GenbankFormat::Fasta => "fasta".to_string(),
                });
                if args.include_raw {
                    result["raw_record"] = Value::String(raw_text);
                }
                return to_pretty_json(&result);
            }
            Ok(raw_text) => {
                last_error = Some(format!(
                    "NCBI {} returned no record for {accession}: {}",
                    db.as_entrez_db(),
                    raw_text.trim()
                ));
            }
            Err(err) => last_error = Some(err.to_string()),
        }
    }

    Err(FunctionCallError::RespondToModel(
        last_error.unwrap_or_else(|| format!("NCBI returned no record for {accession}")),
    ))
}

async fn fetch_uniprot_entry(args: FetchUniprotEntryArgs) -> Result<String, FunctionCallError> {
    let accession = normalize_accession(&args.accession, "UniProt accession")?;
    let client = BiomedExternalDbHandler::client()?;
    let url = format!(
        "{UNIPROT_ENTRY_BASE}/{}.{}",
        accession,
        args.format.extension()
    );
    let text = match args.format {
        UniprotFormat::Tsv => {
            http_get_text_with_query(
                &client,
                &url,
                &[(
                    "fields",
                    "accession,id,protein_name,gene_names,organism_name,length,xref_pdb"
                        .to_string(),
                )],
            )
            .await?
        }
        UniprotFormat::Json | UniprotFormat::Fasta => http_get_text(&client, &url).await?,
    };

    let mut result = match args.format {
        UniprotFormat::Json => {
            let entry_json: Value = serde_json::from_str(&text).map_err(|err| {
                FunctionCallError::RespondToModel(format!(
                    "failed to parse UniProt entry JSON: {err}"
                ))
            })?;
            summarize_uniprot_json(&entry_json, &url)?
        }
        UniprotFormat::Fasta => json!({
            "source": "UniProtKB",
            "accession": accession,
            "url": url,
            "fasta_entries": parse_fasta(&text),
        }),
        UniprotFormat::Tsv => json!({
            "source": "UniProtKB",
            "accession": accession,
            "url": url,
            "tsv": text,
        }),
    };

    if args.include_raw {
        result["raw_record"] = match args.format {
            UniprotFormat::Json => serde_json::from_str(&text).unwrap_or(Value::String(text)),
            UniprotFormat::Fasta | UniprotFormat::Tsv => Value::String(text),
        };
    }

    to_pretty_json(&result)
}

async fn search_uniprot(args: SearchUniprotArgs) -> Result<String, FunctionCallError> {
    let query = args.query.trim();
    if query.is_empty() {
        return Err(FunctionCallError::RespondToModel(
            "UniProt query must not be empty".to_string(),
        ));
    }

    let size = args.size.clamp(1, 25);
    let client = BiomedExternalDbHandler::client()?;
    let text = http_get_text_with_query(
        &client,
        UNIPROT_SEARCH_URL,
        &[
            ("query", query.to_string()),
            ("format", "json".to_string()),
            ("size", size.to_string()),
            (
                "fields",
                "accession,id,protein_name,gene_names,organism_name,length,xref_pdb".to_string(),
            ),
        ],
    )
    .await?;
    let search_json: Value = serde_json::from_str(&text).map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to parse UniProt search JSON: {err}"))
    })?;

    let results = search_json
        .get("results")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(summarize_uniprot_search_entry)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let result = json!({
        "source": "UniProtKB",
        "url": UNIPROT_SEARCH_URL,
        "query": query,
        "size": size,
        "results": results,
    });
    to_pretty_json(&result)
}

async fn fetch_genbank_record_from_db(
    client: &reqwest::Client,
    accession: &str,
    db: GenbankDb,
    format: GenbankFormat,
) -> Result<String, FunctionCallError> {
    let mut query = ncbi_common_query();
    query.push(("db", db.as_entrez_db().to_string()));
    query.push(("id", accession.to_string()));
    query.push(("rettype", format.rettype().to_string()));
    query.push(("retmode", "text".to_string()));

    http_get_text_with_query(client, NCBI_EFETCH_URL, &query).await
}

/// Query parameters every NCBI E-utilities request should carry: the `tool`
/// identifier plus the optional `email`/`api_key` credentials that raise the
/// rate limit from 3 to 10 requests per second.
///
/// The 3/s floor is per source IP and easy to exceed: these tools allow parallel
/// calls, and `search_pubmed_literature` alone spends two requests. Without
/// `NCBI_API_KEY`, concurrent PubMed calls answer 429. The live tests must run
/// with `--test-threads=1` for the same reason.
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

async fn http_get_text(client: &reqwest::Client, url: &str) -> Result<String, FunctionCallError> {
    http_get_text_with_query(client, url, &[]).await
}

async fn http_get_text_with_query(
    client: &reqwest::Client,
    url: &str,
    query: &[(&str, String)],
) -> Result<String, FunctionCallError> {
    let mut request = client.get(url);
    if !query.is_empty() {
        request = request.query(query);
    }
    let response = request.send().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("HTTP request to {url} failed: {err}"))
    })?;
    let status = response.status();
    let text = response.text().await.map_err(|err| {
        FunctionCallError::RespondToModel(format!("failed to read HTTP response from {url}: {err}"))
    })?;
    if !status.is_success() {
        return Err(FunctionCallError::RespondToModel(format!(
            "HTTP request to {url} returned {status}: {}",
            truncate_for_error(&text)
        )));
    }
    Ok(text)
}

fn normalize_pdb_id(pdb_id: &str) -> Result<String, FunctionCallError> {
    let pdb_id = pdb_id.trim().to_ascii_uppercase();
    if pdb_id.len() == 4 && pdb_id.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        Ok(pdb_id)
    } else {
        Err(FunctionCallError::RespondToModel(
            "pdb_id must be a four-character alphanumeric RCSB PDB ID".to_string(),
        ))
    }
}

fn normalize_accession(accession: &str, label: &str) -> Result<String, FunctionCallError> {
    let accession = accession.trim();
    if accession.is_empty() {
        return Err(FunctionCallError::RespondToModel(format!(
            "{label} must not be empty"
        )));
    }
    if accession
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
    {
        Ok(accession.to_string())
    } else {
        Err(FunctionCallError::RespondToModel(format!(
            "{label} contains unsupported characters"
        )))
    }
}

fn first_string_at(value: &Value, array_pointer: &str, key: &str) -> Option<String> {
    value
        .pointer(array_pointer)
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get(key))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct FastaEntry {
    header: String,
    chains: Vec<String>,
    sequence_length: usize,
    sequence: String,
}

fn parse_fasta(text: &str) -> Vec<FastaEntry> {
    let mut entries = Vec::new();
    let mut current_header: Option<String> = None;
    let mut current_sequence = String::new();

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(header) = line.strip_prefix('>') {
            push_fasta_entry(&mut entries, current_header.take(), &mut current_sequence);
            current_header = Some(header.to_string());
        } else {
            current_sequence.push_str(line);
        }
    }
    push_fasta_entry(&mut entries, current_header, &mut current_sequence);
    entries
}

fn push_fasta_entry(entries: &mut Vec<FastaEntry>, header: Option<String>, sequence: &mut String) {
    if let Some(header) = header {
        let sequence_value = std::mem::take(sequence);
        entries.push(FastaEntry {
            chains: parse_chains_from_fasta_header(&header),
            sequence_length: sequence_value.len(),
            sequence: sequence_value,
            header,
        });
    }
}

fn parse_chains_from_fasta_header(header: &str) -> Vec<String> {
    header
        .split('|')
        .find_map(|part| {
            let part = part.trim();
            part.strip_prefix("Chains ")
                .or_else(|| part.strip_prefix("Chain "))
        })
        .map(|chains| {
            chains
                .split(',')
                .map(str::trim)
                .filter(|chain| !chain.is_empty())
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ParsedGenbankRecord {
    source: &'static str,
    db: &'static str,
    requested_accession: String,
    accession: Option<String>,
    version: Option<String>,
    definition: Option<String>,
    organism: Option<String>,
    cds_features_count: usize,
    cds_translations: Vec<CdsTranslation>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct CdsTranslation {
    index: usize,
    translation_length: usize,
    translation: String,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
struct ParsedGenbankFasta {
    source: &'static str,
    db: &'static str,
    requested_accession: String,
    fasta_entries: Vec<FastaEntry>,
}

fn parse_genbank_flatfile(
    text: &str,
    db: GenbankDb,
    requested_accession: &str,
    max_features: usize,
) -> ParsedGenbankRecord {
    let lines = text.lines().collect::<Vec<_>>();
    let definition = parse_continued_genbank_field(&lines, "DEFINITION");
    let accession = lines
        .iter()
        .find_map(|line| line.strip_prefix("ACCESSION"))
        .and_then(|value| value.split_whitespace().next())
        .map(ToString::to_string);
    let version = lines
        .iter()
        .find_map(|line| line.strip_prefix("VERSION"))
        .and_then(|value| value.split_whitespace().next())
        .map(ToString::to_string);
    let organism = lines
        .iter()
        .find_map(|line| line.trim_start().strip_prefix("ORGANISM"))
        .map(|value| value.trim().to_string());
    let cds_translations = parse_cds_translations(text, max_features);

    ParsedGenbankRecord {
        source: "NCBI GenBank/Entrez",
        db: db.as_entrez_db(),
        requested_accession: requested_accession.to_string(),
        accession,
        version,
        definition,
        organism,
        cds_features_count: cds_translations.len(),
        cds_translations,
    }
}

fn parse_genbank_fasta(text: &str, db: GenbankDb, requested_accession: &str) -> ParsedGenbankFasta {
    ParsedGenbankFasta {
        source: "NCBI GenBank/Entrez",
        db: db.as_entrez_db(),
        requested_accession: requested_accession.to_string(),
        fasta_entries: parse_fasta(text),
    }
}

fn parse_continued_genbank_field(lines: &[&str], field: &str) -> Option<String> {
    let mut collected = Vec::new();
    let mut in_field = false;

    for line in lines {
        if let Some(value) = line.strip_prefix(field) {
            collected.push(value.trim().to_string());
            in_field = true;
            continue;
        }
        if in_field {
            let is_continuation = line
                .chars()
                .take(field.len())
                .all(|ch| ch.is_ascii_whitespace());
            if is_continuation {
                let value = line.trim();
                if !value.is_empty() {
                    collected.push(value.to_string());
                }
            } else {
                break;
            }
        }
    }

    (!collected.is_empty()).then(|| collected.join(" "))
}

fn parse_cds_translations(text: &str, max_features: usize) -> Vec<CdsTranslation> {
    let mut translations = Vec::new();
    let mut cursor = 0;
    while translations.len() < max_features {
        let Some(start_offset) = text[cursor..].find("/translation=\"") else {
            break;
        };
        let translation_start = cursor + start_offset + "/translation=\"".len();
        let mut translation = String::new();
        let mut escaped = false;
        let mut translation_end = text.len();
        for (offset, ch) in text[translation_start..].char_indices() {
            if escaped {
                translation.push(ch);
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
                continue;
            }
            if ch == '"' {
                translation_end = translation_start + offset + ch.len_utf8();
                break;
            }
            if !ch.is_whitespace() {
                translation.push(ch);
            }
        }
        translations.push(CdsTranslation {
            index: translations.len() + 1,
            translation_length: translation.len(),
            translation,
        });
        cursor = translation_end;
    }
    translations
}

fn looks_like_ncbi_empty_response(text: &str) -> bool {
    let text = text.trim();
    text.is_empty()
        || text.starts_with("Error:")
        || text.contains("Failed to retrieve sequence")
        || text.contains("Nothing has been found")
}

fn summarize_uniprot_json(entry: &Value, url: &str) -> Result<Value, FunctionCallError> {
    let primary_accession = entry
        .get("primaryAccession")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            FunctionCallError::RespondToModel(
                "UniProt JSON did not contain primaryAccession".to_string(),
            )
        })?;
    let sequence = entry
        .get("sequence")
        .and_then(|sequence| sequence.get("value"))
        .and_then(Value::as_str);
    let sequence_length = entry
        .get("sequence")
        .and_then(|sequence| sequence.get("length"))
        .and_then(Value::as_u64)
        .or_else(|| sequence.map(|seq| seq.len() as u64));
    let pdb_cross_refs = entry
        .get("uniProtKBCrossReferences")
        .and_then(Value::as_array)
        .map(|refs| {
            refs.iter()
                .filter(|reference| {
                    reference.get("database").and_then(Value::as_str) == Some("PDB")
                })
                .filter_map(|reference| reference.get("id").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(json!({
        "source": "UniProtKB",
        "url": url,
        "primary_accession": primary_accession,
        "entry_name": entry.get("uniProtkbId").and_then(Value::as_str),
        "protein_name": uniprot_protein_name(entry),
        "genes": uniprot_gene_names(entry),
        "organism": entry.pointer("/organism/scientificName").and_then(Value::as_str),
        "taxon_id": entry.pointer("/organism/taxonId").and_then(Value::as_u64),
        "sequence_length": sequence_length,
        "sequence": sequence,
        "pdb_cross_references": pdb_cross_refs,
    }))
}

fn summarize_uniprot_search_entry(entry: &Value) -> Value {
    json!({
        "primary_accession": entry.get("primaryAccession").and_then(Value::as_str),
        "entry_name": entry.get("uniProtkbId").and_then(Value::as_str),
        "protein_name": uniprot_protein_name(entry),
        "genes": uniprot_gene_names(entry),
        "organism": entry.pointer("/organism/scientificName").and_then(Value::as_str),
        "taxon_id": entry.pointer("/organism/taxonId").and_then(Value::as_u64),
        "sequence_length": entry.get("sequence").and_then(|sequence| sequence.get("length")).and_then(Value::as_u64),
    })
}

fn uniprot_protein_name(entry: &Value) -> Option<String> {
    entry
        .pointer("/proteinDescription/recommendedName/fullName/value")
        .and_then(Value::as_str)
        .or_else(|| {
            entry
                .pointer("/proteinDescription/submissionNames/0/fullName/value")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            entry
                .pointer("/proteinDescription/alternativeNames/0/fullName/value")
                .and_then(Value::as_str)
        })
        .map(ToString::to_string)
}

fn uniprot_gene_names(entry: &Value) -> Vec<String> {
    entry
        .get("genes")
        .and_then(Value::as_array)
        .map(|genes| {
            genes
                .iter()
                .flat_map(|gene| {
                    let mut names = Vec::new();
                    if let Some(name) = gene.pointer("/geneName/value").and_then(Value::as_str) {
                        names.push(name.to_string());
                    }
                    if let Some(synonyms) = gene.get("synonyms").and_then(Value::as_array) {
                        names.extend(
                            synonyms
                                .iter()
                                .filter_map(|synonym| synonym.get("value").and_then(Value::as_str))
                                .map(ToString::to_string),
                        );
                    }
                    names
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

fn to_pretty_json(value: &Value) -> Result<String, FunctionCallError> {
    serde_json::to_string_pretty(value)
        .map_err(|err| FunctionCallError::Fatal(format!("failed to serialize response: {err}")))
}

fn truncate_for_error(text: &str) -> String {
    const MAX_ERROR_BYTES: usize = 512;
    let trimmed = text.trim();
    if trimmed.len() <= MAX_ERROR_BYTES {
        return trimmed.to_string();
    }
    let mut end = MAX_ERROR_BYTES;
    while !trimmed.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn ncbi_common_query_always_identifies_the_tool() {
        let query = ncbi_common_query();

        assert_eq!(query.first(), Some(&("tool", "codex-for-med".to_string())));
    }

    #[test]
    fn parses_fasta_headers_and_sequences() {
        let entries = parse_fasta(
            ">1SEQ_1|Chains A, B|Fab MNAC13\nEVQL\nVQSG\n>1SEQ_2|Chain C|antigen\nMEEP\n",
        );

        assert_eq!(
            entries,
            vec![
                FastaEntry {
                    header: "1SEQ_1|Chains A, B|Fab MNAC13".to_string(),
                    chains: vec!["A".to_string(), "B".to_string()],
                    sequence_length: 8,
                    sequence: "EVQLVQSG".to_string(),
                },
                FastaEntry {
                    header: "1SEQ_2|Chain C|antigen".to_string(),
                    chains: vec!["C".to_string()],
                    sequence_length: 4,
                    sequence: "MEEP".to_string(),
                },
            ]
        );
    }

    #[test]
    fn parses_genbank_record_summary_and_cds_translation() {
        let record = r#"LOCUS       QHD43416                 1273 aa            linear   VRL 18-JUL-2020
DEFINITION  surface glycoprotein [Severe acute respiratory syndrome coronavirus
            2].
ACCESSION   QHD43416
VERSION     QHD43416.1
SOURCE      Severe acute respiratory syndrome coronavirus 2
  ORGANISM  Severe acute respiratory syndrome coronavirus 2
FEATURES             Location/Qualifiers
     CDS             1..1273
                     /translation="MFVFLVLLPL
                     VSSQCVNLTTRTQLPPAYTNSFTR"
ORIGIN
//
"#;

        let parsed = parse_genbank_flatfile(
            record,
            GenbankDb::Protein,
            "QHD43416.1",
            /*max_features*/ 100,
        );

        assert_eq!(parsed.accession, Some("QHD43416".to_string()));
        assert_eq!(parsed.version, Some("QHD43416.1".to_string()));
        assert_eq!(
            parsed.definition,
            Some(
                "surface glycoprotein [Severe acute respiratory syndrome coronavirus 2]."
                    .to_string()
            )
        );
        assert_eq!(
            parsed.organism,
            Some("Severe acute respiratory syndrome coronavirus 2".to_string())
        );
        assert_eq!(parsed.cds_features_count, 1);
        assert_eq!(parsed.cds_translations[0].translation_length, 34);
        assert_eq!(
            parsed.cds_translations[0].translation,
            "MFVFLVLLPLVSSQCVNLTTRTQLPPAYTNSFTR".to_string()
        );
    }

    #[test]
    fn summarizes_uniprot_json() {
        let entry = json!({
            "primaryAccession": "P05067",
            "uniProtkbId": "A4_HUMAN",
            "proteinDescription": {
                "recommendedName": {
                    "fullName": {"value": "Amyloid-beta precursor protein"}
                }
            },
            "genes": [
                {"geneName": {"value": "APP"}, "synonyms": [{"value": "A4"}]}
            ],
            "organism": {"scientificName": "Homo sapiens", "taxonId": 9606},
            "sequence": {"length": 770, "value": "MLPGLALLLLAAWTARA"},
            "uniProtKBCrossReferences": [
                {"database": "PDB", "id": "1AAP"},
                {"database": "GO", "id": "GO:0000000"}
            ]
        });

        let summary =
            summarize_uniprot_json(&entry, "https://rest.uniprot.org/uniprotkb/P05067.json")
                .expect("summary should parse");

        assert_eq!(summary["primary_accession"], "P05067");
        assert_eq!(summary["protein_name"], "Amyloid-beta precursor protein");
        assert_eq!(summary["genes"], json!(["APP", "A4"]));
        assert_eq!(summary["sequence_length"], 770);
        assert_eq!(summary["pdb_cross_references"], json!(["1AAP"]));
    }

    #[test]
    fn validates_pdb_id() {
        assert_eq!(normalize_pdb_id("1seq").unwrap(), "1SEQ");
        assert!(normalize_pdb_id("1SEQ2").is_err());
        assert!(normalize_pdb_id("1S/Q").is_err());
    }

    #[test]
    fn caps_uniprot_search_size() {
        let args = SearchUniprotArgs {
            query: "gene:APP".to_string(),
            size: 100,
        };
        assert_eq!(args.size.clamp(1, 25), 25);
    }

    #[tokio::test]
    #[ignore = "hits live RCSB PDB, NCBI GenBank, and UniProt APIs"]
    async fn biomed_external_db_live_fetches_reference_records() {
        let uniprot = fetch_uniprot_entry(FetchUniprotEntryArgs {
            accession: "P05067".to_string(),
            format: UniprotFormat::Json,
            include_raw: false,
        })
        .await
        .expect("UniProt P05067 should fetch");
        let uniprot: Value = serde_json::from_str(&uniprot).expect("UniProt output is JSON");
        assert_eq!(uniprot["primary_accession"], "P05067");
        assert_eq!(uniprot["sequence_length"], 770);

        let pdb = fetch_pdb_entry(FetchPdbEntryArgs {
            pdb_id: "1SEQ".to_string(),
            include_fasta: true,
            include_raw: false,
        })
        .await
        .expect("PDB 1SEQ should fetch");
        let pdb: Value = serde_json::from_str(&pdb).expect("PDB output is JSON");
        assert_eq!(pdb["title"], "Fab MNAC13");
        assert_eq!(pdb["experimental_method"], "X-RAY DIFFRACTION");
        assert_eq!(
            pdb["fasta_entries"]
                .as_array()
                .expect("PDB FASTA entries should be an array")
                .len(),
            2
        );

        let genbank = fetch_genbank_record(FetchGenbankRecordArgs {
            accession: "QHD43416.1".to_string(),
            db: GenbankDb::Protein,
            format: GenbankFormat::Genbank,
            include_raw: false,
            max_features: 100,
        })
        .await
        .expect("GenBank QHD43416.1 should fetch");
        let genbank: Value = serde_json::from_str(&genbank).expect("GenBank output is JSON");
        assert_eq!(genbank["version"], "QHD43416.1");
        assert_eq!(
            genbank["organism"],
            "Severe acute respiratory syndrome coronavirus 2"
        );
    }

    #[tokio::test]
    #[ignore = "hits live UniProt API through the Codex tool handler entrypoint"]
    async fn biomed_external_db_live_handler_entrypoint_fetches_uniprot() {
        let (session, turn) = crate::session::tests::make_session_and_context().await;
        let handler = BiomedExternalDbHandler::fetch_uniprot_entry();
        let payload = ToolPayload::Function {
            arguments: json!({
                "accession": "P05067",
                "format": "json",
                "include_raw": false,
            })
            .to_string(),
        };

        let output = handler
            .handle(ToolInvocation {
                session: std::sync::Arc::new(session),
                turn: std::sync::Arc::new(turn),
                cancellation_token: tokio_util::sync::CancellationToken::new(),
                tracker: std::sync::Arc::new(tokio::sync::Mutex::new(
                    crate::turn_diff_tracker::TurnDiffTracker::new(),
                )),
                call_id: "call-uniprot".to_string(),
                tool_name: handler.tool_name(),
                source: crate::tools::context::ToolCallSource::Direct,
                payload: payload.clone(),
            })
            .await
            .expect("handler should fetch UniProt P05067");

        let response = output.to_response_item("call-uniprot", &payload);
        let codex_protocol::models::ResponseInputItem::FunctionCallOutput { output, .. } = response
        else {
            panic!("expected function call output");
        };
        let codex_protocol::models::FunctionCallOutputBody::Text(text) = output.body else {
            panic!("expected text body");
        };
        let value: Value = serde_json::from_str(&text).expect("handler output should be JSON");
        assert_eq!(value["primary_accession"], "P05067");
        assert_eq!(value["sequence_length"], 770);
    }
}
