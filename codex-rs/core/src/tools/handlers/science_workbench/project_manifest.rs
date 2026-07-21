use super::*;

// Project manifest (project.json): a Claude-Science-style run registry at the
// research project root.
const PROJECT_MANIFEST_VERSION: u32 = 1;
pub(super) const PROJECT_MANIFEST_FILE: &str = "project.json";

// Serializes the project.json read-modify-write within the process so two
// concurrent same-project literature_map runs in one turn cannot lose a run
// from the append-only registry.
pub(super) static MANIFEST_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Accept only a JSON object as a project manifest. Non-JSON, arrays, and scalars
/// return None so corrupt or legacy files degrade to a fresh manifest.
fn parse_project_manifest(text: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(text) {
        Ok(value @ Value::Object(_)) => Some(value),
        _ => None,
    }
}

/// Read an existing project.json if present. Missing, unreadable, or corrupt files
/// return None and never error the tool. This is the only new I/O; the merge logic
/// in `build_project_manifest` is pure so it stays unit-testable without the disk.
pub(super) fn read_project_manifest(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    parse_project_manifest(&text)
}

/// Merge a new run onto any existing manifest. Pure: no clock, no I/O — the caller
/// injects `now_rfc3339`. Builds ON TOP of the existing object so unknown or
/// human-added fields survive, preserves the original `created_at`, and appends to
/// an append-only `runs` registry (prior runs are never mutated or dropped).
pub(super) fn build_project_manifest(
    existing: Option<Value>,
    project_id: &str,
    topic: &str,
    now_rfc3339: &str,
    run_entry: Value,
    outputs: Value,
) -> Value {
    let mut map = match existing {
        Some(Value::Object(map)) => map,
        _ => serde_json::Map::new(),
    };
    let created_at = map
        .get("created_at")
        .and_then(Value::as_str)
        .unwrap_or(now_rfc3339)
        .to_string();
    let mut runs = map
        .get("runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    runs.push(run_entry);
    let run_count = runs.len();

    map.insert(
        "schema_version".to_string(),
        json!(PROJECT_MANIFEST_VERSION),
    );
    map.insert("project_id".to_string(), json!(project_id));
    map.insert("topic".to_string(), json!(topic));
    map.insert("created_at".to_string(), json!(created_at));
    map.insert("updated_at".to_string(), json!(now_rfc3339));
    map.insert("run_count".to_string(), json!(run_count));
    map.insert("runs".to_string(), Value::Array(runs));
    map.insert("outputs".to_string(), outputs);
    Value::Object(map)
}

pub(super) fn record_project_run(
    project_dir: &Path,
    project_id: &str,
    topic: &str,
    now_rfc3339: &str,
    run: Value,
    provenance_path: &Path,
    artifacts: &[(&str, &Path)],
) -> Result<std::path::PathBuf, FunctionCallError> {
    let manifest_path = project_dir.join(PROJECT_MANIFEST_FILE);
    let run_provenance = relative_display(project_dir, provenance_path);
    let mut run_entry = run;
    if let Some(obj) = run_entry.as_object_mut() {
        obj.insert("provenance".to_string(), json!(run_provenance));
    }

    let mut outputs = serde_json::Map::new();
    for (key, path) in artifacts {
        outputs.insert(
            (*key).to_string(),
            json!(relative_display(project_dir, path)),
        );
    }
    outputs.insert("provenance".to_string(), json!(run_provenance));
    outputs.insert(
        "manifest".to_string(),
        json!(relative_display(project_dir, &manifest_path)),
    );

    let _guard = MANIFEST_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let manifest = build_project_manifest(
        read_project_manifest(&manifest_path),
        project_id,
        topic,
        now_rfc3339,
        run_entry,
        Value::Object(outputs),
    );
    write_file_atomic(&manifest_path, &pretty_json(manifest)?)?;
    Ok(manifest_path)
}

/// Commit a file atomically: write a sibling temp file, then rename it over the
/// target. Prevents an interrupted write from leaving a truncated/corrupt file
/// (which would make the manifest reader degrade to a fresh registry and lose the
/// accumulated run history).
pub(super) fn write_file_atomic(path: &Path, contents: &str) -> Result<(), FunctionCallError> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, contents).map_err(fs_error("write manifest temp file"))?;
    fs::rename(&tmp, path).map_err(fs_error("commit manifest file"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    #[test]
    fn parse_project_manifest_accepts_only_objects() {
        assert!(parse_project_manifest("not json").is_none());
        assert!(parse_project_manifest("[]").is_none());
        assert!(parse_project_manifest("42").is_none());
        assert_eq!(parse_project_manifest("{}"), Some(json!({})));
        assert!(parse_project_manifest(r#"{"topic":"x"}"#).is_some());
    }
    #[test]
    fn builds_fresh_manifest_when_absent() {
        let manifest = build_project_manifest(
            None,
            "isr_aging",
            "isr aging",
            "2026-07-16T00:00:00+00:00",
            json!({"run_id": "2026-07-16T000000Z_literature_map", "backends": {"vector": {"collection": "medical_knowledge_qwen3_4b"}}}),
            json!({"report": "literature/report.md"}),
        );
        assert_eq!(manifest["schema_version"], json!(PROJECT_MANIFEST_VERSION));
        assert_eq!(manifest["project_id"], "isr_aging");
        assert_eq!(manifest["topic"], "isr aging");
        assert_eq!(manifest["created_at"], "2026-07-16T00:00:00+00:00");
        assert_eq!(manifest["updated_at"], "2026-07-16T00:00:00+00:00");
        assert_eq!(manifest["run_count"], 1);
        assert_eq!(manifest["runs"].as_array().unwrap().len(), 1);
        assert_eq!(
            manifest["runs"][0]["run_id"],
            "2026-07-16T000000Z_literature_map"
        );
        assert_eq!(manifest["outputs"]["report"], "literature/report.md");
    }
    #[test]
    fn rerun_preserves_created_at_and_appends_run() {
        let first = build_project_manifest(
            None,
            "proj",
            "orig topic",
            "2026-01-01T00:00:00+00:00",
            json!({"run_id": "r1"}),
            json!({"report": "literature/report.md"}),
        );
        let first_run = first["runs"][0].clone();
        let second = build_project_manifest(
            Some(first),
            "proj",
            "changed topic",
            "2026-02-02T00:00:00+00:00",
            json!({"run_id": "r2"}),
            json!({"report": "literature/report.md"}),
        );
        assert_eq!(second["created_at"], "2026-01-01T00:00:00+00:00");
        assert_eq!(second["updated_at"], "2026-02-02T00:00:00+00:00");
        assert_eq!(second["topic"], "changed topic");
        assert_eq!(second["run_count"], 2);
        assert_eq!(second["runs"].as_array().unwrap().len(), 2);
        assert_eq!(second["runs"][0], first_run);
        assert_eq!(second["runs"][1]["run_id"], "r2");
    }
    #[test]
    fn build_manifest_preserves_unknown_fields() {
        let existing = json!({
            "created_at": "2025-12-31T00:00:00+00:00",
            "topic": "old",
            "runs": [],
            "custom_annotation": "human-added note"
        });
        let manifest = build_project_manifest(
            Some(existing),
            "proj",
            "new",
            "2026-03-03T00:00:00+00:00",
            json!({"run_id": "r1"}),
            json!({}),
        );
        assert_eq!(manifest["custom_annotation"], "human-added note");
        assert_eq!(manifest["created_at"], "2025-12-31T00:00:00+00:00");
        assert_eq!(manifest["run_count"], 1);
    }
    #[test]
    fn read_project_manifest_degrades_gracefully() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.json");
        assert!(read_project_manifest(&path).is_none());
        fs::write(&path, "{ not json").unwrap();
        assert!(read_project_manifest(&path).is_none());
        fs::write(&path, r#"{"topic":"x"}"#).unwrap();
        assert_eq!(
            read_project_manifest(&path).and_then(|value| value.get("topic").cloned()),
            Some(json!("x"))
        );
    }

    #[test]
    fn write_file_atomic_commits_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.json");
        assert!(write_file_atomic(&path, r#"{"ok":true}"#).is_ok());
        assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"ok":true}"#);
        assert!(!dir.path().join("project.json.tmp").exists());
    }
}
