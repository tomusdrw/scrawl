#!/usr/bin/env python3
"""Check a transfer archive without changing the real user's installation.

Run: python3 tests/linux_package_smoke.py dist/scrawl-0.3.2-linux-x86_64.tar.gz
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest


ARCHIVE = Path(sys.argv.pop(1)).resolve()


class ArchiveTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="scrawl package '")
        self.root = Path(self.temp.name)
        with tarfile.open(ARCHIVE) as archive:
            archive.extractall(self.root, filter="data")
        self.payload = next(self.root.glob("scrawl-*"))
        self.prefix = self.root / "install prefix '"
        self.config = self.root / "config"
        tools = self.root / "tools"
        tools.mkdir()
        # Any accidental rebuild must fail: there is no source checkout here.
        cargo = tools / "cargo"
        cargo.write_text("#!/bin/sh\necho 'Installer unexpectedly ran Cargo' >&2\nexit 97\n")
        cargo.chmod(0o755)
        self.env = dict(os.environ, XDG_CONFIG_HOME=str(self.config),
                        XDG_DATA_HOME=str(self.root / "data"),
                        PATH=f"{tools}:{os.environ['PATH']}")

    def tearDown(self):
        self.temp.cleanup()

    def run_command(self, *args, check=True, cwd=None):
        return subprocess.run(args, cwd=cwd or self.root, env=self.env, text=True,
                              capture_output=True, check=check, timeout=30)

    def install(self, check=True):
        return self.run_command(str(self.payload / "install.sh"), "--prefix", str(self.prefix), check=check)

    def test_external_and_payload_checksums(self):
        self.run_command("sha256sum", "--check", f"{ARCHIVE.name}.sha256", cwd=ARCHIVE.parent)
        self.run_command("sha256sum", "--check", "SHA256SUMS", cwd=self.payload)
        metadata = json.loads((self.payload / "build-info.json").read_text())
        self.assertEqual(metadata["profile"], "release")
        self.assertNotEqual(metadata["cpu"], "native")
        self.assertIn("minimumGlibc", metadata)

    def test_relocated_install_and_embedded_ui_without_source(self):
        self.install()
        binary = self.prefix / "bin/scrawl"
        self.assertIn("scrawl 0.3.2", self.run_command(str(binary), "--version").stdout)
        self.run_command(str(binary), "ui", "--check")
        self.assertTrue((self.prefix / "share/icons/hicolor/1024x1024/apps/scrawl.png").is_file())
        desktop = self.prefix / "share/applications/scrawl.desktop"
        if shutil.which("desktop-file-validate"):
            self.run_command("desktop-file-validate", str(desktop))
        self.assertTrue((self.config / "scrawl/omarchy.lua").is_file())
        self.assertFalse((self.config / "hypr").exists())
        # The installed app must survive removal of the extracted payload.
        shutil.rmtree(self.payload)
        self.run_command(str(binary), "--help")

    def test_reinstall_preserves_existing_binding_files(self):
        bindings = self.config / "scrawl"
        bindings.mkdir(parents=True)
        for name in ("omarchy.lua", "hyprland.conf"):
            (bindings / name).write_text("user-owned binding\n")
        self.install()
        self.install()
        for name in ("omarchy.lua", "hyprland.conf"):
            self.assertEqual((bindings / name).read_text(), "user-owned binding\n")

    def test_corruption_is_rejected_before_installation(self):
        with (self.payload / "bin/scrawl").open("ab") as binary:
            binary.write(b"bad transfer")
        result = self.install(check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("integrity check failed", result.stderr)
        self.assertFalse(self.prefix.exists())
        self.assertFalse(self.config.exists())


if __name__ == "__main__":
    unittest.main()
