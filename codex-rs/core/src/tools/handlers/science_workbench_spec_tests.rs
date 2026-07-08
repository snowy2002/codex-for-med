use super::*;

#[test]
fn creates_science_workbench_tool_specs() {
    let ToolSpec::Function(list_tool) = create_list_med_knowledge_collections_tool() else {
        panic!("list_med_knowledge_collections should be a function tool");
    };
    assert_eq!(list_tool.name, LIST_MED_KNOWLEDGE_COLLECTIONS_TOOL_NAME);
    assert!(list_tool.description.contains("knowledge backends"));

    let ToolSpec::Function(describe_tool) = create_describe_med_database_tool() else {
        panic!("describe_med_database should be a function tool");
    };
    assert_eq!(describe_tool.name, DESCRIBE_MED_DATABASE_TOOL_NAME);
    assert!(describe_tool.description.contains("SQL and vector"));

    let ToolSpec::Function(literature_tool) = create_literature_map_tool() else {
        panic!("literature_map should be a function tool");
    };
    assert_eq!(literature_tool.name, LITERATURE_MAP_TOOL_NAME);
    assert!(literature_tool.description.contains("research_projects"));
}
