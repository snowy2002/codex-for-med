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
                    "@gair/codex-med-linux-x64": (
                        "npm:@gair/codex-med@0.1.5-linux-x64"
                    )
                },
            )


if __name__ == "__main__":
    unittest.main()
