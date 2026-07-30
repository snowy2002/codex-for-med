#!/usr/bin/env python3
"""Black-box tests for the Codex Med npm launcher."""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
CODEX_CLI_ROOT = SCRIPT_DIR.parent
TARGET_TRIPLE = "x86_64-unknown-linux-gnu"


class CodexMedLauncherTest(unittest.TestCase):
    def run_launcher(
        self,
        *,
        home: Path,
        codex_med_home: Path | None = None,
    ) -> dict[str, str]:
        with tempfile.TemporaryDirectory(prefix="codex-med-launcher-") as temp_dir:
            package_root = Path(temp_dir)
            bin_dir = package_root / "bin"
            native_bin_dir = (
                package_root / "vendor" / TARGET_TRIPLE / "bin"
            )
            bin_dir.mkdir(parents=True)
            native_bin_dir.mkdir(parents=True)
            shutil.copy2(CODEX_CLI_ROOT / "bin" / "codex.js", bin_dir / "codex.js")

            native_binary = native_bin_dir / "codex"
            native_binary.write_text(
                "#!/bin/sh\n"
                'printf "CODEX_MED_HOME=%s\\n" "$CODEX_MED_HOME"\n'
                'printf "CODEX_HOME=%s\\n" "$CODEX_HOME"\n'
                'printf "CODEX_MANAGED_PACKAGE_ROOT=%s\\n" '
                '"$CODEX_MANAGED_PACKAGE_ROOT"\n',
                encoding="utf-8",
            )
            native_binary.chmod(0o755)

            env = os.environ.copy()
            env["HOME"] = str(home)
            env["CODEX_HOME"] = str(home / "regular-codex-home")
            if codex_med_home is None:
                env.pop("CODEX_MED_HOME", None)
            else:
                env["CODEX_MED_HOME"] = str(codex_med_home)

            stdout = subprocess.check_output(
                ["node", str(bin_dir / "codex.js"), "--version"],
                env=env,
                text=True,
            )
            return dict(line.split("=", 1) for line in stdout.splitlines())

    def test_inherits_regular_codex_home(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-med-home-") as temp_dir:
            home = Path(temp_dir)
            values = self.run_launcher(home=home)
            expected_home = str(home / "regular-codex-home")

            self.assertEqual(values["CODEX_MED_HOME"], "")
            self.assertEqual(values["CODEX_HOME"], expected_home)
            self.assertFalse((home / ".codex-med").exists())

    def test_codex_med_home_does_not_override_regular_codex_home(self) -> None:
        with tempfile.TemporaryDirectory(prefix="codex-med-home-") as temp_dir:
            home = Path(temp_dir)
            configured_home = home / "isolated-medical-agent"
            values = self.run_launcher(
                home=home,
                codex_med_home=configured_home,
            )

            self.assertEqual(values["CODEX_MED_HOME"], str(configured_home))
            self.assertEqual(
                values["CODEX_HOME"],
                str(home / "regular-codex-home"),
            )
            self.assertFalse(configured_home.exists())


if __name__ == "__main__":
    unittest.main()
