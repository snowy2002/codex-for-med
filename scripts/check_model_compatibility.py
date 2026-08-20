#!/usr/bin/env python3
"""Validate Codex Med's model compatibility contract and audited upstream baseline."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
MANIFEST_PATH = REPO_ROOT / "codex-rs/build-info/upstream_compatibility.json"


def fail(message: str) -> None:
    print(f"model-compatibility: ERROR: {message}", file=sys.stderr)


def git_show(ref: str, path: str) -> bytes:
    result = subprocess.run(
        ["git", "show", f"{ref}:{path}"],
        cwd=REPO_ROOT,
        check=False,
        capture_output=True,
    )
    if result.returncode != 0:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        raise RuntimeError(f"cannot read {ref}:{path}: {detail}")
    return result.stdout


def validate_local_contract(manifest: dict[str, object]) -> list[str]:
    errors: list[str] = []
    with (REPO_ROOT / "codex-rs/Cargo.toml").open("rb") as handle:
        cargo = tomllib.load(handle)
    workspace_version = cargo["workspace"]["package"]["version"]
    if manifest.get("codex_med_version") != workspace_version:
        errors.append(
            "manifest codex_med_version does not match codex-rs workspace version "
            f"({manifest.get('codex_med_version')!r} != {workspace_version!r})"
        )

    if manifest.get("schema_version") != 2:
        errors.append("unsupported compatibility manifest schema_version")
    if manifest.get("model_cache_schema_version") != 1:
        errors.append("unexpected model cache schema version")

    upstream = manifest.get("upstream")
    if not isinstance(upstream, dict):
        errors.append("upstream metadata must be an object")
    else:
        if not re.fullmatch(r"[0-9a-f]{40}", str(upstream.get("revision", ""))):
            errors.append("upstream revision must be a full 40-character Git SHA")
        protocol_version = str(upstream.get("protocol_client_version", ""))
        if not re.fullmatch(r"\d+\.\d+\.\d+", protocol_version):
            errors.append("upstream protocol_client_version must be a stable semantic version")
        if protocol_version != upstream.get("npm_release"):
            errors.append("protocol_client_version must match the audited upstream npm_release")

    tracked_files = manifest.get("tracked_files")
    if not isinstance(tracked_files, list) or not tracked_files:
        errors.append("tracked_files must contain at least one upstream compatibility path")
    else:
        paths: set[str] = set()
        for entry in tracked_files:
            if not isinstance(entry, dict):
                errors.append("every tracked_files entry must be an object")
                continue
            path = str(entry.get("path", ""))
            digest = str(entry.get("sha256", ""))
            if path in paths:
                errors.append(f"duplicate tracked path: {path}")
            paths.add(path)
            if path.startswith("/") or ".." in Path(path).parts:
                errors.append(f"tracked path is not repository-relative: {path}")
            if not re.fullmatch(r"[0-9a-f]{64}", digest):
                errors.append(f"invalid SHA-256 for tracked path: {path}")

    invariants = {
        "codex-rs/protocol/src/openai_models.rs": [
            "Custom(String)",
            'reasoning_effort must not be empty',
        ],
        "codex-rs/models-manager/src/cache.rs": [
            "MODELS_CACHE_SCHEMA_VERSION",
            "load_last_known_good",
            "provider_identity",
            "NamedTempFile",
        ],
        "codex-rs/models-manager/src/manager.rs": [
            "fetch_and_update_models_with_fallback",
            "cache_identity",
        ],
        "codex-cli/bin/codex.js": ["CODEX_MED_HOME", ".codex-med"],
        "codex-rs/login/src/auth/default_client.rs": [
            "upstream_protocol_client_version",
        ],
        "codex-rs/model-provider-info/src/lib.rs": [
            '"version".to_string()',
            "upstream_protocol_client_version",
        ],
    }
    for relative_path, needles in invariants.items():
        contents = (REPO_ROOT / relative_path).read_text(encoding="utf-8")
        for needle in needles:
            if needle not in contents:
                errors.append(f"local compatibility invariant missing in {relative_path}: {needle}")

    return errors


def validate_upstream(manifest: dict[str, object], upstream_ref: str) -> list[str]:
    errors: list[str] = []
    tracked_files = manifest["tracked_files"]
    assert isinstance(tracked_files, list)
    for entry in tracked_files:
        assert isinstance(entry, dict)
        path = str(entry["path"])
        expected = str(entry["sha256"])
        try:
            contents = git_show(upstream_ref, path)
        except RuntimeError as err:
            errors.append(str(err))
            continue
        actual = hashlib.sha256(contents).hexdigest()
        if actual != expected:
            errors.append(
                f"upstream drift in {path}: expected {expected}, observed {actual} at {upstream_ref}"
            )
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--upstream-ref",
        help="Git ref to compare with recorded upstream file hashes; defaults to the audited SHA",
    )
    parser.add_argument(
        "--local-only",
        action="store_true",
        help="validate only the local compatibility contract without reading upstream Git objects",
    )
    args = parser.parse_args()

    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    errors = validate_local_contract(manifest)
    if not args.local_only:
        upstream = manifest.get("upstream", {})
        default_ref = upstream.get("revision") if isinstance(upstream, dict) else None
        upstream_ref = args.upstream_ref or str(default_ref or "")
        errors.extend(validate_upstream(manifest, upstream_ref))

    if errors:
        for error in errors:
            fail(error)
        print(
            "Review upstream model-protocol changes, port relevant behavior, then update the "
            "audited revision and hashes in upstream_compatibility.json.",
            file=sys.stderr,
        )
        return 1

    mode = "local contract" if args.local_only else f"upstream ref {args.upstream_ref or 'audited'}"
    print(f"model-compatibility: OK ({mode})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
