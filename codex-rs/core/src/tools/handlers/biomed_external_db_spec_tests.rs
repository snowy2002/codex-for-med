use super::*;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;
use serde_json::json;

fn function_tool_name(spec: ToolSpec) -> String {
    let ToolSpec::Function(tool) = spec else {
        panic!("biomed external db tools should be function tools");
    };
    tool.name
}

#[test]
fn creates_expected_tool_names() {
    assert_eq!(
        function_tool_name(create_fetch_pdb_entry_tool()),
        FETCH_PDB_ENTRY_TOOL_NAME
    );
    assert_eq!(
        function_tool_name(create_fetch_genbank_record_tool()),
        FETCH_GENBANK_RECORD_TOOL_NAME
    );
    assert_eq!(
        function_tool_name(create_fetch_uniprot_entry_tool()),
        FETCH_UNIPROT_ENTRY_TOOL_NAME
    );
    assert_eq!(
        function_tool_name(create_search_uniprot_tool()),
        SEARCH_UNIPROT_TOOL_NAME
    );
    assert_eq!(
        function_tool_name(create_search_pubmed_literature_tool()),
        SEARCH_PUBMED_LITERATURE_TOOL_NAME
    );
    assert_eq!(
        function_tool_name(create_fetch_pubmed_record_tool()),
        FETCH_PUBMED_RECORD_TOOL_NAME
    );
    assert_eq!(
        function_tool_name(create_validate_citations_tool()),
        VALIDATE_CITATIONS_TOOL_NAME
    );
}

#[test]
fn validate_citations_tool_requires_citations_array() {
    let ToolSpec::Function(tool) = create_validate_citations_tool() else {
        panic!("validate_citations should be a function tool");
    };
    assert_eq!(
        tool.parameters.required,
        Some(vec!["citations".to_string()])
    );
}

#[test]
fn pubmed_search_tool_restricts_sort_values_and_requires_query() {
    let ToolSpec::Function(tool) = create_search_pubmed_literature_tool() else {
        panic!("search_pubmed_literature should be a function tool");
    };
    let properties = tool
        .parameters
        .properties
        .expect("properties should be present");

    assert_eq!(
        properties
            .get("sort")
            .and_then(|schema| schema.enum_values.clone()),
        Some(vec![json!("relevance"), json!("pub_date")])
    );
    assert_eq!(tool.parameters.required, Some(vec!["query".to_string()]));
}

#[test]
fn pubmed_record_tool_requires_pmid() {
    let ToolSpec::Function(tool) = create_fetch_pubmed_record_tool() else {
        panic!("fetch_pubmed_record should be a function tool");
    };

    assert_eq!(tool.parameters.required, Some(vec!["pmid".to_string()]));
}

#[test]
fn genbank_tool_restricts_database_and_format_values() {
    let ToolSpec::Function(tool) = create_fetch_genbank_record_tool() else {
        panic!("fetch_genbank_record should be a function tool");
    };
    let properties = tool
        .parameters
        .properties
        .expect("properties should be present");

    assert_eq!(
        properties
            .get("db")
            .and_then(|schema| schema.enum_values.clone()),
        Some(vec![json!("auto"), json!("nucleotide"), json!("protein")])
    );
    assert_eq!(
        properties
            .get("format")
            .and_then(|schema| schema.enum_values.clone()),
        Some(vec![json!("genbank"), json!("fasta")])
    );
}
