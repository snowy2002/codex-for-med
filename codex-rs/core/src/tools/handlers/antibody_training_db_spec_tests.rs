use super::*;
use codex_tools::ToolSpec;
use pretty_assertions::assert_eq;

#[test]
fn creates_expected_tool_name() {
    let ToolSpec::Function(tool) = create_query_antibody_training_records_tool() else {
        panic!("query_antibody_training_records should be a function tool");
    };

    assert_eq!(tool.name, QUERY_ANTIBODY_TRAINING_RECORDS_TOOL_NAME);
    assert!(tool.description.contains("antibodies"));
    assert!(tool.description.contains("codex-med"));
}
