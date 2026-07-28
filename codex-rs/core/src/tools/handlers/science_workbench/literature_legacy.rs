//! Non-destructive compatibility migration for pre-v2 project artifacts.

use crate::function_tool::FunctionCallError;
use serde_json::Value;
use serde_json::json;
use std::fs;
use std::path::Path;
use uuid::Uuid;

use super::fs_error;
use super::pretty_json;
use super::project_manifest::read_project_manifest;

const LEGACY_ARTIFACTS: &[&str] = &[
    "evidence_table.csv",
    "pubmed_records.csv",
    "pubmed_records.jsonl",
    "report.md",
    "citations.bib",
];

pub(super) fn migrate_legacy_project(
    project_dir: &Path,
) -> Result<Option<Value>, FunctionCallError> {
    let literature_dir = project_dir.join("literature");
    let existing = LEGACY_ARTIFACTS
        .iter()
        .filter(|name| literature_dir.join(name).is_file())
        .copied()
        .collect::<Vec<_>>();
    let legacy_provenance = project_dir.join("provenance").join("run.json");
    if existing.is_empty() && !legacy_provenance.is_file() {
        return Ok(None);
    }

    let provenance = fs::read_to_string(&legacy_provenance)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let manifest = read_project_manifest(&project_dir.join("project.json"));
    let workflow = provenance
        .as_ref()
        .and_then(|value| value.get("workflow"))
        .and_then(Value::as_str)
        .or_else(|| latest_manifest_workflow(manifest.as_ref()));
    let inferred_source = match workflow {
        Some("literature_map") => Some("local"),
        Some("pubmed_literature_map") => Some("pubmed"),
        _ if existing.contains(&"evidence_table.csv")
            && !existing.contains(&"pubmed_records.csv") =>
        {
            Some("local")
        }
        _ if existing.contains(&"pubmed_records.csv")
            && !existing.contains(&"evidence_table.csv") =>
        {
            Some("pubmed")
        }
        _ => None,
    };
    let raw_run_id = provenance
        .as_ref()
        .and_then(|value| value.get("run_id"))
        .and_then(Value::as_str)
        .or_else(|| latest_manifest_run_id(manifest.as_ref()))
        .unwrap_or("unknown");
    let legacy_run_id = format!(
        "legacy_{}_{}",
        safe_component(raw_run_id),
        &Uuid::new_v5(&Uuid::NAMESPACE_URL, raw_run_id.as_bytes())
            .simple()
            .to_string()[..8]
    );
    let destination = inferred_source.map_or_else(
        || literature_dir.join("_legacy").join(&legacy_run_id),
        |source| {
            literature_dir
                .join(source)
                .join("runs")
                .join(&legacy_run_id)
        },
    );
    if destination.exists() {
        return Ok(Some(json!({
            "status": "already_migrated",
            "legacy_run_id": legacy_run_id,
            "source": inferred_source,
            "destination": relative_to_project(project_dir, &destination),
        })));
    }

    fs::create_dir_all(&destination).map_err(fs_error("create legacy literature snapshot"))?;
    for name in &existing {
        fs::copy(literature_dir.join(name), destination.join(name))
            .map_err(fs_error("copy legacy literature artifact"))?;
    }
    if legacy_provenance.is_file() {
        fs::copy(&legacy_provenance, destination.join("provenance.json"))
            .map_err(fs_error("copy legacy literature provenance"))?;
    }

    let warning = inferred_source.is_none().then_some({
        "Legacy artifact source could not be determined; files were preserved under literature/_legacy and were not promoted to latest."
    });
    let result = json!({
        "status": "migrated",
        "legacy_run_id": legacy_run_id,
        "source": inferred_source,
        "destination": relative_to_project(project_dir, &destination),
        "copied_files": existing,
        "warning": warning,
    });
    if warning.is_some() {
        let warning_path = literature_dir
            .join("_legacy")
            .join(format!("migration_warning_{legacy_run_id}.json"));
        fs::write(&warning_path, pretty_json(result.clone())?)
            .map_err(fs_error("write legacy migration warning"))?;
    }
    Ok(Some(result))
}

fn latest_manifest_workflow(manifest: Option<&Value>) -> Option<&str> {
    manifest?
        .get("runs")?
        .as_array()?
        .iter()
        .rev()
        .find_map(|run| run.get("workflow").and_then(Value::as_str))
}

fn latest_manifest_run_id(manifest: Option<&Value>) -> Option<&str> {
    manifest?
        .get("runs")?
        .as_array()?
        .iter()
        .rev()
        .find_map(|run| run.get("run_id").and_then(Value::as_str))
}

fn safe_component(value: &str) -> String {
    let component = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '_'
            }
        })
        .take(96)
        .collect::<String>();
    if component.is_empty() {
        "unknown".to_string()
    } else {
        component
    }
}

fn relative_to_project(project_dir: &Path, path: &Path) -> String {
    path.strip_prefix(project_dir)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn migrates_known_legacy_source_without_deleting_originals() {
        let temp = tempfile::tempdir().expect("temp dir");
        let project = temp.path().join("research_projects/project");
        fs::create_dir_all(project.join("literature")).expect("literature");
        fs::create_dir_all(project.join("provenance")).expect("provenance");
        fs::write(project.join("literature/report.md"), "# Legacy\n").expect("report");
        fs::write(
            project.join("provenance/run.json"),
            r#"{"workflow":"literature_map","run_id":"run/one"}"#,
        )
        .expect("provenance");

        let migrated = migrate_legacy_project(&project)
            .expect("migration")
            .expect("result");
        let destination = project.join(migrated["destination"].as_str().expect("destination"));
        assert_eq!(migrated["source"], "local");
        assert!(destination.join("report.md").is_file());
        assert!(project.join("literature/report.md").is_file());
        assert_eq!(
            migrate_legacy_project(&project)
                .expect("rerun")
                .expect("result")["status"],
            "already_migrated"
        );
    }

    #[test]
    fn preserves_ambiguous_legacy_files_with_warning() {
        let temp = tempfile::tempdir().expect("temp dir");
        let project = temp.path().join("project");
        fs::create_dir_all(project.join("literature")).expect("literature");
        fs::write(project.join("literature/report.md"), "# Legacy\n").expect("report");

        let migrated = migrate_legacy_project(&project)
            .expect("migration")
            .expect("result");
        assert!(migrated["source"].is_null());
        assert!(
            project
                .join(migrated["destination"].as_str().expect("destination"))
                .join("report.md")
                .is_file()
        );
        assert_eq!(
            fs::read_dir(project.join("literature/_legacy"))
                .expect("legacy")
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("migration_warning_"))
                .count(),
            1
        );
    }
}
