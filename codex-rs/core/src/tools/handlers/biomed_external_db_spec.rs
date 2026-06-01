use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use serde_json::json;
use std::collections::BTreeMap;

pub const FETCH_PDB_ENTRY_TOOL_NAME: &str = "fetch_pdb_entry";
pub const FETCH_GENBANK_RECORD_TOOL_NAME: &str = "fetch_genbank_record";
pub const FETCH_UNIPROT_ENTRY_TOOL_NAME: &str = "fetch_uniprot_entry";
pub const SEARCH_UNIPROT_TOOL_NAME: &str = "search_uniprot";

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

#[cfg(test)]
#[path = "biomed_external_db_spec_tests.rs"]
mod tests;
