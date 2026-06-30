use super::*;
use codex_tools::ToolSpec;

#[test]
fn creates_search_vector_knowledge_tool_schema() {
    let ToolSpec::Function(tool) = create_search_vector_knowledge_tool() else {
        panic!("search_vector_knowledge should be a function tool");
    };

    assert_eq!(tool.name, SEARCH_VECTOR_KNOWLEDGE_TOOL_NAME);
    assert!(tool.description.contains("Qdrant"));
    assert!(tool.description.contains("category"));
    assert!(
        tool.parameters
            .required
            .as_ref()
            .expect("required")
            .contains(&"query".to_string())
    );
}
