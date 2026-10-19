#!/usr/bin/env python3
"""Validate a built pacman package, including an isolated install/remove cycle.

Run: python3 tests/omarchy_package.py dist/omarchy/scrawl-*.pkg.tar.zst
The lifecycle test uses fakeroot and a temporary pacman root/database.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


PACKAGE = Path(sys.argv.pop(1)).resolve()


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="scrawl-pacman-")
        self.root = Path(self.temp.name)
        self.env = dict(os.environ, SCRAWL_RUNTIME_DIR=str(self.root / "runtime"))

    def tearDown(self):
        self.temp.cleanup()

    def run_command(self, *args):
        result = subprocess.run(args, env=self.env, text=True, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, f"{args}:\n{result.stdout}\n{result.stderr}")
        return result

    def test_declared_runtime_and_optional_gpu_dependencies(self):
        metadata = self.run_command("bsdtar", "-xOf", str(PACKAGE), ".PKGINFO").stdout
        fields = {}
        for line in metadata.splitlines():
            if " = " in line:
                key, value = line.split(" = ", 1)
                fields.setdefault(key, []).append(value)
        self.assertEqual(fields["pkgname"], ["scrawl"])
        self.assertEqual(fields["arch"], ["x86_64"])
        self.assertTrue({"hyprland", "wf-recorder", "ffmpeg", "wl-clipboard", "slurp",
                         "libpulse", "pipewire-pulse", "python-gobject", "python-cairo",
                         "gtk4-layer-shell", "xdg-utils"}.issubset(fields["depend"]))
        optional = {dep.split(":", 1)[0] for dep in fields["optdepend"]}
        self.assertTrue({"whisper-cpp", "ggml-vulkan", "ggml-cuda"}.issubset(optional))

    def test_payload_is_package_owned_and_has_no_user_configuration(self):
        listing = self.run_command("bsdtar", "-tf", str(PACKAGE)).stdout.splitlines()
        ownership = self.run_command("bsdtar", "--numeric-owner", "-tvf", str(PACKAGE)).stdout.splitlines()
        for entry in ownership:
            self.assertEqual(entry.split()[2:4], ["0", "0"], entry)
        files = {name for name in listing if not name.endswith("/")}
        self.assertEqual(files - {".PKGINFO", ".BUILDINFO", ".MTREE"}, {
            "usr/bin/scrawl", "usr/share/applications/scrawl.desktop",
            "usr/share/icons/hicolor/1024x1024/apps/scrawl.png",
            "usr/share/licenses/scrawl/LICENSE", "usr/share/doc/scrawl/README.md",
            "usr/share/doc/scrawl/omarchy.lua",
        })

    def test_installed_binary_launcher_and_embedded_helper(self):
        self.run_command("bsdtar", "-xf", str(PACKAGE), "-C", str(self.root))
        binary = self.root / "usr/bin/scrawl"
        self.assertIn("scrawl", self.run_command(str(binary), "--version").stdout)
        self.run_command(str(binary), "ui", "--check")
        desktop = self.root / "usr/share/applications/scrawl.desktop"
        self.run_command("desktop-file-validate", str(desktop))
        self.assertIn("Exec=/usr/bin/scrawl app", desktop.read_text())

    def test_pacman_reinstall_and_remove_preserve_captures_and_settings(self):
        install_root = self.root / "system"
        database = install_root / "var/lib/pacman"
        database.mkdir(parents=True)
        config = self.root / "pacman.conf"
        config.write_text("[options]\nArchitecture = x86_64\nSigLevel = Never\n")
        user_files = [install_root / "home/test/.config/scrawl/config.json",
                      install_root / "home/test/.local/share/scrawl/captures/keep/summary.md"]
        for path in user_files:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("preserve\n")
        command = ["fakeroot", "pacman", "--root", str(install_root), "--dbpath", str(database),
                   "--config", str(config), "--logfile", str(self.root / "pacman.log"),
                   "--noconfirm", "--noscriptlet"]
        # Dependencies are checked as metadata above; this root intentionally has no OS.
        self.run_command(*command, "-Udd", str(PACKAGE))
        self.run_command(*command, "-Udd", str(PACKAGE))
        self.assertTrue((install_root / "usr/bin/scrawl").is_file())
        self.run_command(*command, "-R", "scrawl")
        self.assertFalse((install_root / "usr/bin/scrawl").exists())
        for path in user_files:
            self.assertEqual(path.read_text(), "preserve\n")


if __name__ == "__main__":
    unittest.main()
