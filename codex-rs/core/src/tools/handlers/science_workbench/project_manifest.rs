use super::*;
use std::fs::OpenOptions;

// Project manifest (project.json): a Claude-Science-style run registry at the
// research project root.
const PROJECT_MANIFEST_VERSION: u32 = 2;
pub(super) const PROJECT_MANIFEST_FILE: &str = "project.json";
const PROJECT_LOCK_FILE: &str = ".literature.lock";

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
    output_source: &str,
    source_outputs: Value,
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
    let incoming_run_id = run_entry.get("run_id").and_then(Value::as_str);
    let already_recorded = incoming_run_id.is_some_and(|incoming_run_id| {
        runs.iter()
            .any(|run| run.get("run_id").and_then(Value::as_str) == Some(incoming_run_id))
    });
    if !already_recorded {
        runs.push(run_entry);
    }
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
    let mut outputs = map
        .get("outputs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    outputs.insert(output_source.to_string(), source_outputs);
    map.insert("outputs".to_string(), Value::Object(outputs));
    Value::Object(map)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn record_project_run_unlocked(
    project_dir: &Path,
    project_id: &str,
    topic: &str,
    now_rfc3339: &str,
    run: Value,
    output_source: &str,
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
    if let Some(run_id) = run_entry.get("run_id").and_then(Value::as_str) {
        outputs.insert("latest_run_id".to_string(), json!(run_id));
    }

    let manifest = build_project_manifest(
        read_project_manifest(&manifest_path),
        project_id,
        topic,
        now_rfc3339,
        run_entry,
        output_source,
        Value::Object(outputs),
    );
    write_file_atomic(&manifest_path, &pretty_json(manifest)?)?;
    Ok(manifest_path)
}

pub(super) fn with_project_lock<T>(
    project_dir: &Path,
    operation: impl FnOnce() -> Result<T, FunctionCallError>,
) -> Result<T, FunctionCallError> {
    fs::create_dir_all(project_dir).map_err(fs_error("create research project directory"))?;
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(project_dir.join(PROJECT_LOCK_FILE))
        .map_err(fs_error("open literature project lock"))?;
    lock_file
        .lock()
        .map_err(fs_error("acquire literature project lock"))?;
    let result = operation();
    let unlock_result = lock_file
        .unlock()
        .map_err(fs_error("release literature project lock"));
    match (result, unlock_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(value), Ok(())) => Ok(value),
    }
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
    use std::process::Stdio;

    const LOCK_HELPER_PROJECT_ENV: &str = "CODEX_MED_TEST_LOCK_HELPER_PROJECT";
    const LOCK_HELPER_MARKER_ENV: &str = "CODEX_MED_TEST_LOCK_HELPER_MARKER";
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
            /*existing*/ None,
            "isr_aging",
            "isr aging",
            "2026-07-16T00:00:00+00:00",
            json!({"run_id": "2026-07-16T000000Z_literature_map", "backends": {"vector": {"collection": "medical_knowledge_qwen3_4b"}}}),
            "local",
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
        assert_eq!(
            manifest["outputs"]["local"]["report"],
            "literature/report.md"
        );
    }
    #[test]
    fn rerun_preserves_created_at_and_appends_run() {
        let first = build_project_manifest(
            /*existing*/ None,
            "proj",
            "orig topic",
            "2026-01-01T00:00:00+00:00",
            json!({"run_id": "r1"}),
            "local",
            json!({"report": "literature/report.md"}),
        );
        let first_run = first["runs"][0].clone();
        let second = build_project_manifest(
            Some(first),
            "proj",
            "changed topic",
            "2026-02-02T00:00:00+00:00",
            json!({"run_id": "r2"}),
            "local",
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
            "local",
            json!({}),
        );
        assert_eq!(manifest["custom_annotation"], "human-added note");
        assert_eq!(manifest["created_at"], "2025-12-31T00:00:00+00:00");
        assert_eq!(manifest["run_count"], 1);
    }

    #[test]
    fn updating_pubmed_outputs_preserves_local_outputs() {
        let local = build_project_manifest(
            /*existing*/ None,
            "proj",
            "topic",
            "2026-03-03T00:00:00+00:00",
            json!({"run_id": "local-1"}),
            "local",
            json!({"literature_ids": "literature/local/literature_ids.csv"}),
        );
        let combined = build_project_manifest(
            Some(local),
            "proj",
            "topic",
            "2026-03-04T00:00:00+00:00",
            json!({"run_id": "pubmed-1"}),
            "pubmed",
            json!({"literature_ids": "literature/pubmed/literature_ids.csv"}),
        );

        assert_eq!(
            combined["outputs"]["local"]["literature_ids"],
            "literature/local/literature_ids.csv"
        );
        assert_eq!(
            combined["outputs"]["pubmed"]["literature_ids"],
            "literature/pubmed/literature_ids.csv"
        );
        assert_eq!(combined["run_count"], 2);
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

    #[test]
    fn project_lock_subprocess_helper() {
        let (Some(project), Some(marker)) = (
            std::env::var_os(LOCK_HELPER_PROJECT_ENV),
            std::env::var_os(LOCK_HELPER_MARKER_ENV),
        ) else {
            return;
        };
        with_project_lock(Path::new(&project), || {
            fs::write(marker, "acquired").map_err(fs_error("write lock test marker"))
        })
        .expect("child lock");
    }

    #[test]
    fn project_lock_blocks_a_second_process() {
        let temp = tempfile::tempdir().expect("temp dir");
        let project = temp.path().join("project");
        let marker = temp.path().join("child-acquired");
        let mut child = with_project_lock(&project, || {
            let child = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "tools::handlers::science_workbench::project_manifest::tests::project_lock_subprocess_helper",
                    "--nocapture",
                ])
                .env(LOCK_HELPER_PROJECT_ENV, &project)
                .env(LOCK_HELPER_MARKER_ENV, &marker)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(fs_error("spawn project lock subprocess"))?;
            std::thread::sleep(std::time::Duration::from_millis(250));
            assert!(
                !marker.exists(),
                "child acquired the project lock before the parent released it"
            );
            Ok(child)
        })
        .expect("parent lock");
        assert!(child.wait().expect("wait for child").success());
        assert_eq!(fs::read_to_string(marker).expect("marker"), "acquired");
    }
}
