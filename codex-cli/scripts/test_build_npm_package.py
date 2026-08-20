#!/usr/bin/env python3
"""Tests for staging the Codex Med npm package."""

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
BUILD_SCRIPT = SCRIPT_DIR / "build_npm_package.py"
SPEC = importlib.util.spec_from_file_location("build_npm_package", BUILD_SCRIPT)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError(f"Unable to load build script: {BUILD_SCRIPT}")
build_npm_package = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(build_npm_package)


class BuildNpmPackageTest(unittest.TestCase):
    def test_codex_package_includes_literature_workflow_skill(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-med-npm-stage-") as temp_dir:
            staging_dir = Path(temp_dir)
            version = "0.1.5"

            build_npm_package.stage_sources(staging_dir, version, "codex")

            staged_skill_root = (
                staging_dir
                / "skills"
                / build_npm_package.CODEX_MED_SKILL_NAME
            )
            self.assertEqual(
                (staged_skill_root / "SKILL.md").read_text(encoding="utf-8"),
                (
                    build_npm_package.CODEX_MED_SKILL_ROOT / "SKILL.md"
                ).read_text(encoding="utf-8"),
            )
            self.assertTrue(
                (staged_skill_root / "agents" / "openai.yaml").is_file()
            )

            package_json = json.loads(
                (staging_dir / "package.json").read_text(encoding="utf-8")
            )
            self.assertEqual(package_json["version"], version)
            self.assertEqual(package_json["files"], ["bin/codex.js", "skills"])
            self.assertEqual(
                package_json["optionalDependencies"],
                {
                    config["npm_name"]: (
                        "npm:@gair/codex-med@"
                        + build_npm_package.compute_platform_package_version(
                            version, config["npm_tag"]
                        )
                    )
                    for config in build_npm_package.CODEX_PLATFORM_PACKAGES.values()
                },
            )

    def test_platform_packages_cover_the_release_matrix(self) -> None:
        self.assertEqual(
            {
                config["target_triple"]
                for config in build_npm_package.CODEX_PLATFORM_PACKAGES.values()
            },
            {
                "x86_64-unknown-linux-musl",
                "aarch64-unknown-linux-musl",
                "x86_64-apple-darwin",
                "aarch64-apple-darwin",
                "x86_64-pc-windows-msvc",
                "aarch64-pc-windows-msvc",
            },
        )

    def test_launcher_and_package_builder_use_the_same_platform_mapping(self) -> None:
        launcher = (build_npm_package.CODEX_CLI_ROOT / "bin/codex.js").read_text(
            encoding="utf-8"
        )

        for config in build_npm_package.CODEX_PLATFORM_PACKAGES.values():
            self.assertIn(config["target_triple"], launcher)
            self.assertIn(config["npm_name"], launcher)

    def test_release_staging_uses_codex_med_artifacts_and_only_publishes_med(self) -> None:
        stage_script = (
            build_npm_package.REPO_ROOT / "scripts/stage_npm_packages.py"
        ).read_text(encoding="utf-8")
        release_workflow = (
            build_npm_package.REPO_ROOT / ".github/workflows/rust-release.yml"
        ).read_text(encoding="utf-8")

        self.assertIn('GITHUB_REPO = "snowy2002/codex-for-med"', stage_script)
        for config in build_npm_package.CODEX_PLATFORM_PACKAGES.values():
            self.assertIn(config["target_triple"], stage_script)
        self.assertNotIn("--package codex-responses-api-proxy", release_workflow)
        self.assertNotIn("--package codex-sdk", release_workflow)


if __name__ == "__main__":
    unittest.main()
