use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use serde_json::json;
use std::collections::BTreeMap;

pub const FETCH_PDB_ENTRY_TOOL_NAME: &str = "fetch_pdb_entry";
pub const FETCH_GENBANK_RECORD_TOOL_NAME: &str = "fetch_genbank_record";
pub const FETCH_UNIPROT_ENTRY_TOOL_NAME: &str = "fetch_uniprot_entry";
pub const SEARCH_UNIPROT_TOOL_NAME: &str = "search_uniprot";
pub const SEARCH_PUBMED_LITERATURE_TOOL_NAME: &str = "search_pubmed_literature";
pub const FETCH_PUBMED_RECORD_TOOL_NAME: &str = "fetch_pubmed_record";
pub const VALIDATE_CITATIONS_TOOL_NAME: &str = "validate_citations";

pub fn create_fetch_pdb_entry_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "pdb_id".to_string(),
            JsonSchema::string(Some(
                "Required four-character RCSB PDB identifier, for example `1SEQ`.".to_string(),
            )),
        ),
        (
            "include_fasta".to_string(),
            JsonSchema::boolean(Some(
                "Whether to include parsed RCSB FASTA chain entries. Defaults to true.".to_string(),
            )),
        ),
        (
            "include_raw".to_string(),
            JsonSchema::boolean(Some(
                "Whether to include the raw RCSB entry JSON in the result. Defaults to false."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: FETCH_PDB_ENTRY_TOOL_NAME.to_string(),
        description: "Fetch an authoritative RCSB PDB entry and optional chain FASTA records by PDB ID. Use for structure metadata, experimental method, title, and chain sequence grounding."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["pdb_id".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_fetch_genbank_record_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "accession".to_string(),
            JsonSchema::string(Some(
                "Required NCBI accession or accession.version, for example `QHD43416.1`."
                    .to_string(),
            )),
        ),
        (
            "db".to_string(),
            JsonSchema::string_enum(
                vec![json!("auto"), json!("nucleotide"), json!("protein")],
                Some(
                    "NCBI Entrez database to query. `auto` tries protein first, then nucleotide."
                        .to_string(),
                ),
            ),
        ),
        (
            "format".to_string(),
            JsonSchema::string_enum(
                vec![json!("genbank"), json!("fasta")],
                Some("Return GenBank flatfile or FASTA text. Defaults to genbank.".to_string()),
            ),
        ),
        (
            "include_raw".to_string(),
            JsonSchema::boolean(Some(
                "Whether to include the raw NCBI efetch text. Defaults to false.".to_string(),
            )),
        ),
        (
            "max_features".to_string(),
            JsonSchema::integer(Some(
                "Maximum number of parsed CDS translation features to return. Defaults to 100."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: FETCH_GENBANK_RECORD_TOOL_NAME.to_string(),
        description: "Fetch an NCBI GenBank/Entrez nucleotide or protein record by accession. Use for deposited sequence records, organism/source metadata, and CDS translations when present."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["accession".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_fetch_uniprot_entry_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "accession".to_string(),
            JsonSchema::string(Some(
                "Required UniProtKB primary or secondary accession, for example `P05067`."
                    .to_string(),
            )),
        ),
        (
            "format".to_string(),
            JsonSchema::string_enum(
                vec![json!("json"), json!("fasta"), json!("tsv")],
                Some("UniProt response format. Defaults to json.".to_string()),
            ),
        ),
        (
            "include_raw".to_string(),
            JsonSchema::boolean(Some(
                "Whether to include the raw UniProt response. Defaults to false.".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: FETCH_UNIPROT_ENTRY_TOOL_NAME.to_string(),
        description: "Fetch an authoritative UniProtKB entry by accession. Use for canonical protein metadata, organism, gene names, sequence length, sequence, and PDB cross-references."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["accession".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_search_uniprot_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "query".to_string(),
            JsonSchema::string(Some(
                "Required UniProt search query, for example `gene:APP AND organism_id:9606`."
                    .to_string(),
            )),
        ),
        (
            "size".to_string(),
            JsonSchema::integer(Some(
                "Maximum number of results to return, capped at 25. Defaults to 10.".to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: SEARCH_UNIPROT_TOOL_NAME.to_string(),
        description: "Search UniProtKB with the official UniProt REST API and return concise result summaries. Use when an exact UniProt accession is unknown."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["query".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_search_pubmed_literature_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "query".to_string(),
            JsonSchema::string(Some(
                "Required PubMed search query. Supports full PubMed syntax including field tags and boolean operators, for example `integrated stress response AND neurodegeneration[Title/Abstract]`."
                    .to_string(),
            )),
        ),
        (
            "retmax".to_string(),
            JsonSchema::integer(Some(
                "Maximum number of records to return, capped at 50. Defaults to 10.".to_string(),
            )),
        ),
        (
            "sort".to_string(),
            JsonSchema::string_enum(
                vec![json!("relevance"), json!("pub_date")],
                Some("Result ordering. Defaults to relevance.".to_string()),
            ),
        ),
        (
            "min_year".to_string(),
            JsonSchema::integer(Some(
                "Optional earliest publication year, inclusive. Requires max_year to also be set."
                    .to_string(),
            )),
        ),
        (
            "max_year".to_string(),
            JsonSchema::integer(Some(
                "Optional latest publication year, inclusive. Requires min_year to also be set."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: SEARCH_PUBMED_LITERATURE_TOOL_NAME.to_string(),
        description: "Search PubMed with the official NCBI E-utilities API and return concise citation summaries (PMID, title, authors, journal, publication date, DOI). Use to discover literature when PMIDs are unknown, then call fetch_pubmed_record for abstracts and MeSH terms. PubMed silently drops qualifiers it cannot honour (a misspelled field tag widens to an all-fields search), so check `query_degraded` and `query_translation` in the result before trusting that a tag or filter applied."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["query".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_fetch_pubmed_record_tool() -> ToolSpec {
    let properties = BTreeMap::from([
        (
            "pmid".to_string(),
            JsonSchema::string(Some(
                "Required PubMed identifier, digits only, for example `33301246`.".to_string(),
            )),
        ),
        (
            "include_raw".to_string(),
            JsonSchema::boolean(Some(
                "Whether to include the raw NCBI MEDLINE text in the result. Defaults to false."
                    .to_string(),
            )),
        ),
        (
            "max_mesh_terms".to_string(),
            JsonSchema::integer(Some(
                "Maximum number of MeSH headings to return, capped at 200. Defaults to 50."
                    .to_string(),
            )),
        ),
    ]);

    ToolSpec::Function(ResponsesApiTool {
        name: FETCH_PUBMED_RECORD_TOOL_NAME.to_string(),
        description: "Fetch a single authoritative PubMed record by PMID, including title, abstract, authors, journal, DOI, and MeSH headings. Use for citation grounding and evidence extraction once a PMID is known."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["pmid".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

pub fn create_validate_citations_tool() -> ToolSpec {
    let citation_item = JsonSchema::object(
        BTreeMap::from([
            (
                "doi".to_string(),
                JsonSchema::string(Some(
                    "Required DOI of the cited work, for example `10.1126/science.1225829`. A `doi:` prefix or a doi.org URL is accepted."
                        .to_string(),
                )),
            ),
            (
                "claimed_title".to_string(),
                JsonSchema::string(Some(
                    "Optional title the citation claims for this DOI. If given, it is compared (tolerant of case, spacing, and dash/quote variants) against the authoritative Crossref title."
                        .to_string(),
                )),
            ),
            (
                "claimed_authors".to_string(),
                JsonSchema::array(
                    JsonSchema::string(/*description*/ None),
                    Some(
                        "Optional author names the citation claims. Each is matched by surname against the Crossref author list."
                            .to_string(),
                    ),
                ),
            ),
            (
                "claimed_year".to_string(),
                JsonSchema::integer(Some(
                    "Optional publication year claimed for this DOI. It is compared with the Crossref issued year to disambiguate works with identical titles."
                        .to_string(),
                )),
            ),
        ]),
        Some(vec!["doi".to_string()]),
        Some(false.into()),
    );

    let properties = BTreeMap::from([(
        "citations".to_string(),
        JsonSchema::array(
            citation_item,
            Some("The citations to verify, up to 25 per call.".to_string()),
        ),
    )]);

    ToolSpec::Function(ResponsesApiTool {
        name: VALIDATE_CITATIONS_TOOL_NAME.to_string(),
        description: "Verify citations against the authoritative Crossref record by DOI. For each citation, resolves the DOI and reports whether it exists and whether the claimed title, publication year, and supporting author metadata match the real article — catching the common failure where a generated reference cites a DOI that resolves to a different paper. This is a deterministic metadata check; it does NOT judge whether the article supports the claim it is cited for."
            .to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["citations".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

#[cfg(test)]
#[path = "biomed_external_db_spec_tests.rs"]
mod tests;
