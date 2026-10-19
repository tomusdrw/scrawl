#!/usr/bin/env python3
"""Exercise source-snapshot rebuilds with real makepkg and a tiny Rust fixture."""
import contextlib
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("builder", ROOT / "scripts/build-omarchy-package.py")
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class BuilderTests(unittest.TestCase):
    def test_rejects_options_that_reuse_old_sources_or_binaries(self):
        for option in ("--noextract", "-e", "--repackage", "-R", "-fR"):
            with self.subTest(option=option), patch.object(sys, "argv", ["builder", "--", option]):
                with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                    builder.main()
                self.assertEqual(error.exception.code, 2)

    def test_rebuild_uses_changed_source_and_removes_deleted_files(self):
        with tempfile.TemporaryDirectory(prefix="scrawl-builder-") as directory:
            root = Path(directory)
            source = root / "project"
            output = root / "output"
            (source / "src").mkdir(parents=True)
            (source / "Cargo.toml").write_text('[package]\nname="scrawl-fixture"\nversion="1.0.0"\nedition="2021"\n')
            recipe = source / "packaging/omarchy/PKGBUILD.in"
            recipe.parent.mkdir(parents=True)
            recipe.write_text('''pkgname=scrawl-fixture
pkgver=@VERSION@
pkgrel=1
arch=('x86_64')
source=('scrawl-source.tar.gz')
sha256sums=('@SOURCE_SHA256@')
options=('!debug' '!lto')
build() {
  cd "$srcdir/scrawl"
  cargo build --release --offline --target-dir "$srcdir/target"
}
package() {
  install -Dm755 "$srcdir/target/release/scrawl-fixture" "$pkgdir/usr/bin/scrawl-fixture"
  if [[ -f "$srcdir/scrawl/src/deleted.txt" ]]; then
    install -Dm644 "$srcdir/scrawl/src/deleted.txt" "$pkgdir/usr/share/scrawl-fixture/deleted.txt"
  fi
}
''')
            obsolete = source / "src/deleted.txt"
            obsolete.write_text("obsolete\n")
            argv = ["builder", "--output-dir", str(output), "--", "--force", "--nodeps"]
            for message in ("before", "after"):
                (source / "src/main.rs").write_text(f'fn main() {{ println!("{message}"); }}\n')
                if message == "after":
                    obsolete.unlink()
                with patch.object(builder, "ROOT", source), patch.object(builder, "INPUTS", ("Cargo.toml", "src")):
                    with patch.object(sys, "argv", argv):
                        builder.main()
                package = subprocess.check_output(["makepkg", "--packagelist"], cwd=output, text=True).strip()
                extracted = root / message
                extracted.mkdir()
                subprocess.run(["bsdtar", "-xf", package, "-C", str(extracted)], check=True)
                binary = extracted / "usr/bin/scrawl-fixture"
                actual = subprocess.check_output([str(binary)], text=True).strip()
                self.assertEqual(actual, message, "rebuild reused an old Cargo output")
                self.assertEqual((extracted / "usr/share/scrawl-fixture/deleted.txt").exists(), message == "before")


if __name__ == "__main__":
    unittest.main()
