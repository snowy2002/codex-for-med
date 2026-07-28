//! Transaction-like project artifact commits for literature workflows.

use crate::function_tool::FunctionCallError;
use serde_json::Value;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::fmt::Write;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use uuid::Uuid;

use super::fs_error;
use super::json_error;
use super::project_manifest::read_project_manifest;
use super::project_manifest::record_project_run_unlocked;
use super::relative_display;

#[derive(Debug)]
pub(super) struct LiteratureRunPaths {
    pub(super) project_dir: PathBuf,
    pub(super) latest_ids: PathBuf,
    pub(super) latest_report: PathBuf,
    pub(super) latest_citations: PathBuf,
    pub(super) snapshot_dir: PathBuf,
    pub(super) provenance: PathBuf,
}

pub(super) fn new_run_id(now: &chrono::DateTime<chrono::Utc>, workflow: &str) -> String {
    let suffix = Uuid::new_v4().simple().to_string();
    format!(
        "{}_{}_{}",
        now.format("%Y-%m-%dT%H%M%S.%6fZ"),
        workflow,
        &suffix[..8]
    )
}

pub(super) fn render_literature_ids(ids: &[String]) -> String {
    let mut csv = String::from("literature_id\n");
    for id in ids {
        let _ = writeln!(csv, "{id}");
    }
    csv
}

#[allow(clippy::too_many_arguments)]
pub(super) fn commit_literature_run(
    cwd: &Path,
    project_id: &str,
    source: &str,
    workflow: &str,
    run_id: &str,
    ids_csv: &str,
    report: &str,
    citations: &str,
    mut provenance: Value,
) -> Result<LiteratureRunPaths, FunctionCallError> {
    let project_dir = cwd.join("research_projects").join(project_id);
    let source_dir = project_dir.join("literature").join(source);
    let runs_dir = source_dir.join("runs");
    let snapshot_dir = runs_dir.join(run_id);
    let provenance_dir = project_dir.join("provenance").join(workflow);
    let provenance_path = provenance_dir.join(format!("{run_id}.json"));
    let staging_dir = project_dir.join(".staging").join(run_id);
    let staged_literature = staging_dir.join("literature");
    let staged_provenance = staging_dir.join("provenance.json");

    for dir in [
        &runs_dir,
        &provenance_dir,
        &project_dir.join("code"),
        &project_dir.join("analysis"),
        &project_dir.join("figures"),
        &staged_literature,
    ] {
        fs::create_dir_all(dir).map_err(fs_error("create literature project directory"))?;
    }
    if snapshot_dir.exists() || provenance_path.exists() {
        return Err(FunctionCallError::Fatal(format!(
            "refusing to overwrite immutable literature run {run_id}"
        )));
    }

    let staged_ids = staged_literature.join("literature_ids.csv");
    let staged_report = staged_literature.join("report.md");
    let staged_citations = staged_literature.join("citations.bib");
    fs::write(&staged_ids, ids_csv).map_err(fs_error("write staged literature ids"))?;
    fs::write(&staged_report, report).map_err(fs_error("write staged literature report"))?;
    fs::write(&staged_citations, citations)
        .map_err(fs_error("write staged literature citations"))?;

    let snapshot_outputs = json!({
        "literature_ids": artifact_metadata(
            &project_dir,
            &snapshot_dir.join("literature_ids.csv"),
            ids_csv,
        ),
        "report": artifact_metadata(
            &project_dir,
            &snapshot_dir.join("report.md"),
            report,
        ),
        "citations": artifact_metadata(
            &project_dir,
            &snapshot_dir.join("citations.bib"),
            citations,
        ),
    });
    if let Some(object) = provenance.as_object_mut() {
        object.insert("outputs".to_string(), snapshot_outputs);
    }
    let provenance_text = serde_json::to_string_pretty(&provenance)
        .map_err(json_error("serialize literature provenance"))?;
    fs::write(&staged_provenance, provenance_text)
        .map_err(fs_error("write staged literature provenance"))?;

    fs::rename(&staged_literature, &snapshot_dir)
        .map_err(fs_error("commit immutable literature snapshot"))?;
    fs::rename(&staged_provenance, &provenance_path)
        .map_err(fs_error("commit immutable literature provenance"))?;

    let latest_ids = source_dir.join("literature_ids.csv");
    let latest_report = source_dir.join("report.md");
    let latest_citations = source_dir.join("citations.bib");
    write_atomic(&latest_ids, ids_csv)?;
    write_atomic(&latest_report, report)?;
    write_atomic(&latest_citations, citations)?;
    let _ = fs::remove_dir(&staging_dir);
    let _ = fs::remove_dir(project_dir.join(".staging"));

    Ok(LiteratureRunPaths {
        project_dir,
        latest_ids,
        latest_report,
        latest_citations,
        snapshot_dir,
        provenance: provenance_path,
    })
}

fn artifact_metadata(project_dir: &Path, path: &Path, contents: &str) -> Value {
    let digest = Sha256::digest(contents.as_bytes());
    json!({
        "path": relative_display(project_dir, path),
        "bytes": contents.len(),
        "sha256": format!("{digest:x}"),
    })
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), FunctionCallError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("artifact");
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", Uuid::new_v4().simple()));
    fs::write(&temporary, contents).map_err(fs_error("write literature artifact temp file"))?;
    fs::rename(&temporary, path).map_err(fs_error("commit literature artifact"))
}

pub(super) fn recover_incomplete_literature_runs(
    project_dir: &Path,
) -> Result<Vec<Value>, FunctionCallError> {
    let mut recovered = Vec::new();
    quarantine_partial_staging(project_dir, &mut recovered)?;
    for (source, workflow) in [
        ("local", "literature_map"),
        ("pubmed", "pubmed_literature_map"),
    ] {
        recover_source_runs(project_dir, source, workflow, &mut recovered)?;
    }
    Ok(recovered)
}

fn quarantine_partial_staging(
    project_dir: &Path,
    recovered: &mut Vec<Value>,
) -> Result<(), FunctionCallError> {
    let staging = project_dir.join(".staging");
    let Ok(entries) = fs::read_dir(&staging) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(fs_error("read literature staging entry"))?;
        if !entry
            .file_type()
            .map_err(fs_error("inspect literature staging entry"))?
            .is_dir()
        {
            continue;
        }
        let run_id = entry.file_name().to_string_lossy().into_owned();
        let destination = recovery_orphan_path(project_dir, "staging", &run_id);
        move_without_overwrite(&entry.path(), &destination)?;
        recovered.push(json!({
            "run_id": run_id,
            "action": "rolled_back_partial_staging",
            "preserved_at": relative_display(project_dir, &destination),
        }));
    }
    let _ = fs::remove_dir(&staging);
    Ok(())
}

fn recover_source_runs(
    project_dir: &Path,
    source: &str,
    workflow: &str,
    recovered: &mut Vec<Value>,
) -> Result<(), FunctionCallError> {
    let runs_dir = project_dir.join("literature").join(source).join("runs");
    let provenance_dir = project_dir.join("provenance").join(workflow);
    let manifest = read_project_manifest(&project_dir.join("project.json"));
    let committed_ids = manifest
        .as_ref()
        .and_then(|value| value.get("runs"))
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|run| run.get("run_id").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();
    let snapshot_ids = directory_names(&runs_dir)?;
    let provenance_ids = json_file_stems(&provenance_dir)?;
    let run_ids = snapshot_ids
        .union(&provenance_ids)
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();

    for run_id in run_ids {
        if run_id.starts_with("legacy_") || committed_ids.contains(&run_id) {
            continue;
        }
        let snapshot = runs_dir.join(&run_id);
        let provenance_path = provenance_dir.join(format!("{run_id}.json"));
        if !snapshot.is_dir() || !provenance_path.is_file() {
            let kind = if snapshot.is_dir() {
                "snapshot"
            } else {
                "provenance"
            };
            let path = if snapshot.is_dir() {
                snapshot
            } else {
                provenance_path
            };
            let destination = recovery_orphan_path(project_dir, kind, &run_id);
            move_without_overwrite(&path, &destination)?;
            recovered.push(json!({
                "run_id": run_id,
                "workflow": workflow,
                "action": "rolled_back_incomplete_commit",
                "missing": if kind == "snapshot" { "provenance" } else { "snapshot" },
                "preserved_at": relative_display(project_dir, &destination),
            }));
            continue;
        }

        let provenance_text = fs::read_to_string(&provenance_path)
            .map_err(fs_error("read recoverable literature provenance"))?;
        let provenance: Value = serde_json::from_str(&provenance_text)
            .map_err(json_error("parse recoverable literature provenance"))?;
        let artifacts: Result<(String, String, String), FunctionCallError> = (|| {
            Ok((
                verified_snapshot_artifact(&snapshot, &provenance, "literature_ids")?,
                verified_snapshot_artifact(&snapshot, &provenance, "report")?,
                verified_snapshot_artifact(&snapshot, &provenance, "citations")?,
            ))
        })();
        let (ids, report, citations) = match artifacts {
            Ok(artifacts) => artifacts,
            Err(error) => {
                let snapshot_destination =
                    recovery_orphan_path(project_dir, "invalid_snapshot", &run_id);
                let provenance_destination =
                    recovery_orphan_path(project_dir, "invalid_provenance", &run_id)
                        .with_extension("json");
                move_without_overwrite(&snapshot, &snapshot_destination)?;
                move_without_overwrite(&provenance_path, &provenance_destination)?;
                recovered.push(json!({
                    "run_id": run_id,
                    "workflow": workflow,
                    "action": "rolled_back_invalid_commit",
                    "reason": format!("{error:?}"),
                    "snapshot_preserved_at": relative_display(
                        project_dir,
                        &snapshot_destination,
                    ),
                    "provenance_preserved_at": relative_display(
                        project_dir,
                        &provenance_destination,
                    ),
                }));
                continue;
            }
        };
        let source_dir = project_dir.join("literature").join(source);
        let latest_ids = source_dir.join("literature_ids.csv");
        let latest_report = source_dir.join("report.md");
        let latest_citations = source_dir.join("citations.bib");
        write_atomic(&latest_ids, &ids)?;
        write_atomic(&latest_report, &report)?;
        write_atomic(&latest_citations, &citations)?;

        let project_id = provenance
            .get("project_id")
            .and_then(Value::as_str)
            .or_else(|| project_dir.file_name().and_then(|name| name.to_str()))
            .unwrap_or("research_project")
            .to_string();
        let topic = provenance
            .get("topic")
            .and_then(Value::as_str)
            .unwrap_or("recovered literature run")
            .to_string();
        let updated_at = provenance
            .get("finished_at")
            .or_else(|| provenance.get("started_at"))
            .and_then(Value::as_str)
            .unwrap_or("1970-01-01T00:00:00Z")
            .to_string();
        record_project_run_unlocked(
            project_dir,
            &project_id,
            &topic,
            &updated_at,
            provenance,
            source,
            &provenance_path,
            &[
                ("literature_ids", &latest_ids),
                ("report", &latest_report),
                ("citations", &latest_citations),
            ],
        )?;
        recovered.push(json!({
            "run_id": run_id,
            "workflow": workflow,
            "action": "completed_manifest_commit",
        }));
    }
    Ok(())
}

fn verified_snapshot_artifact(
    snapshot: &Path,
    provenance: &Value,
    key: &str,
) -> Result<String, FunctionCallError> {
    let filename = match key {
        "literature_ids" => "literature_ids.csv",
        "report" => "report.md",
        "citations" => "citations.bib",
        _ => {
            return Err(FunctionCallError::Fatal(format!(
                "unknown literature artifact key {key}"
            )));
        }
    };
    let contents = fs::read_to_string(snapshot.join(filename))
        .map_err(fs_error("read recoverable literature snapshot"))?;
    let expected_bytes = provenance
        .pointer(&format!("/outputs/{key}/bytes"))
        .and_then(Value::as_u64);
    let expected_sha = provenance
        .pointer(&format!("/outputs/{key}/sha256"))
        .and_then(Value::as_str);
    let digest = Sha256::digest(contents.as_bytes());
    let actual_sha = format!("{digest:x}");
    if expected_bytes != Some(contents.len() as u64) || expected_sha != Some(actual_sha.as_str()) {
        return Err(FunctionCallError::Fatal(format!(
            "refusing to recover {filename}: snapshot metadata does not match provenance"
        )));
    }
    Ok(contents)
}

fn directory_names(path: &Path) -> Result<std::collections::BTreeSet<String>, FunctionCallError> {
    let Ok(entries) = fs::read_dir(path) else {
        return Ok(std::collections::BTreeSet::new());
    };
    let mut names = std::collections::BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(fs_error("read literature runs directory"))?;
        if entry
            .file_type()
            .map_err(fs_error("inspect literature run snapshot"))?
            .is_dir()
        {
            names.insert(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Ok(names)
}

fn json_file_stems(path: &Path) -> Result<std::collections::BTreeSet<String>, FunctionCallError> {
    let Ok(entries) = fs::read_dir(path) else {
        return Ok(std::collections::BTreeSet::new());
    };
    let mut names = std::collections::BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(fs_error("read literature provenance directory"))?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("json")
            && let Some(stem) = path.file_stem().and_then(|stem| stem.to_str())
        {
            names.insert(stem.to_string());
        }
    }
    Ok(names)
}

fn recovery_orphan_path(project_dir: &Path, kind: &str, run_id: &str) -> PathBuf {
    project_dir
        .join(".recovery_orphans")
        .join(kind)
        .join(run_id)
}

fn move_without_overwrite(source: &Path, destination: &Path) -> Result<(), FunctionCallError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(fs_error("create literature recovery directory"))?;
    }
    if destination.exists() {
        return Err(FunctionCallError::Fatal(format!(
            "refusing to overwrite recovery artifact {}",
            destination.display()
        )));
    }
    fs::rename(source, destination).map_err(fs_error("preserve incomplete literature commit"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn renders_id_only_csv_in_rank_order() {
        assert_eq!(
            render_literature_ids(&["id-b".to_string(), "id-a".to_string()]),
            "literature_id\nid-b\nid-a\n"
        );
    }

    #[test]
    fn same_instant_run_ids_remain_unique() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&chrono::Utc);
        let ids = (0..100)
            .map(|_| new_run_id(&now, "literature_map"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 100);
    }

    #[test]
    fn commits_separate_latest_and_immutable_snapshot() {
        let temp = tempfile::tempdir().expect("temp dir");
        let committed = commit_literature_run(
            temp.path(),
            "project",
            "local",
            "literature_map",
            "run-1",
            "literature_id\nid-1\n",
            "# Report\n",
            "@misc{x}\n",
            json!({"run_id": "run-1"}),
        )
        .expect("commit run");
        assert_eq!(
            fs::read_to_string(&committed.latest_ids).expect("latest ids"),
            "literature_id\nid-1\n"
        );
        assert_eq!(
            fs::read_to_string(committed.snapshot_dir.join("literature_ids.csv"))
                .expect("snapshot ids"),
            "literature_id\nid-1\n"
        );
        let provenance: Value =
            serde_json::from_str(&fs::read_to_string(&committed.provenance).expect("provenance"))
                .expect("parse provenance");
        assert_eq!(provenance["run_id"], "run-1");
        assert_eq!(provenance["outputs"]["literature_ids"]["bytes"], json!(19));
        assert!(
            provenance["outputs"]["literature_ids"]["sha256"]
                .as_str()
                .is_some_and(|hash| hash.len() == 64)
        );
    }

    #[test]
    fn immutable_collision_fails_without_updating_latest_files() {
        let temp = tempfile::tempdir().expect("temp dir");
        commit_literature_run(
            temp.path(),
            "project",
            "local",
            "literature_map",
            "run-1",
            "literature_id\nid-1\n",
            "# First\n",
            "@misc{first}\n",
            json!({"run_id": "run-1"}),
        )
        .expect("first commit");
        let retry = commit_literature_run(
            temp.path(),
            "project",
            "local",
            "literature_map",
            "run-1",
            "literature_id\nid-2\n",
            "# Second\n",
            "@misc{second}\n",
            json!({"run_id": "run-1"}),
        );

        assert!(retry.is_err());
        assert_eq!(
            fs::read_to_string(
                temp.path()
                    .join("research_projects/project/literature/local/literature_ids.csv")
            )
            .expect("latest"),
            "literature_id\nid-1\n"
        );
    }

    #[test]
    fn recovery_finishes_snapshot_and_provenance_commit_missing_from_manifest() {
        let temp = tempfile::tempdir().expect("temp dir");
        let committed = commit_literature_run(
            temp.path(),
            "project",
            "local",
            "literature_map",
            "run-1",
            "literature_id\nid-1\n",
            "# Recovered\n",
            "@misc{recovered}\n",
            json!({
                "run_id": "run-1",
                "workflow": "literature_map",
                "project_id": "project",
                "topic": "recovery",
                "finished_at": "2026-01-01T00:00:00Z",
            }),
        )
        .expect("commit without manifest");
        fs::write(&committed.latest_report, "# Stale\n").expect("stale latest");

        let actions =
            recover_incomplete_literature_runs(&committed.project_dir).expect("recover run");

        assert_eq!(actions[0]["action"], "completed_manifest_commit");
        assert_eq!(
            fs::read_to_string(&committed.latest_report).expect("latest report"),
            "# Recovered\n"
        );
        let manifest =
            read_project_manifest(&committed.project_dir.join("project.json")).expect("manifest");
        assert_eq!(manifest["run_count"], 1);
        assert_eq!(manifest["runs"][0]["run_id"], "run-1");
        assert!(
            recover_incomplete_literature_runs(&committed.project_dir)
                .expect("idempotent recovery")
                .is_empty()
        );
    }

    #[test]
    fn recovery_quarantines_invalid_snapshot_instead_of_promoting_it() {
        let temp = tempfile::tempdir().expect("temp dir");
        let committed = commit_literature_run(
            temp.path(),
            "project",
            "pubmed",
            "pubmed_literature_map",
            "run-1",
            "literature_id\nid-1\n",
            "# Valid\n",
            "@misc{valid}\n",
            json!({
                "run_id": "run-1",
                "workflow": "pubmed_literature_map",
                "project_id": "project",
            }),
        )
        .expect("commit without manifest");
        fs::write(committed.snapshot_dir.join("report.md"), "# Tampered\n")
            .expect("tamper snapshot");

        let actions =
            recover_incomplete_literature_runs(&committed.project_dir).expect("rollback invalid");

        assert_eq!(actions[0]["action"], "rolled_back_invalid_commit");
        assert!(!committed.project_dir.join("project.json").exists());
        assert!(
            committed
                .project_dir
                .join(".recovery_orphans/invalid_snapshot/run-1/report.md")
                .is_file()
        );
        assert!(
            committed
                .project_dir
                .join(".recovery_orphans/invalid_provenance/run-1.json")
                .is_file()
        );
    }
}
