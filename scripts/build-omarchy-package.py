#!/usr/bin/env python3
"""Prepare a checksummed source recipe and build a pacman package for Omarchy."""
import argparse
import gzip
import hashlib
import io
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parent.parent
# Only application/build inputs enter the package's source snapshot.
INPUTS = (
    "Cargo.toml", "Cargo.lock", "build.rs", "LICENSE", "src",
    "assets/app-icon.png", "helper/linux.py", "helper/test_linux.py",
    "tests/linux_smoke.py", "packaging/omarchy",
)


def prepare(output_dir):
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"[0-9A-Za-z.+-]+", version):
        raise ValueError("Invalid package version")
    paths = []
    for name in INPUTS:
        path = ROOT / name
        paths.extend(sorted(p for p in path.rglob("*") if p.is_file()) if path.is_dir() else [path])
    source = output_dir / "scrawl-source.tar.gz"
    output_dir.mkdir(parents=True, exist_ok=True)
    # Fixed archive metadata makes the same source tree produce the same checksum.
    with tempfile.TemporaryFile() as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                for path in sorted(paths):
                    if path.is_symlink():
                        raise ValueError(f"Source symlinks are not supported: {path.relative_to(ROOT)}")
                    content = path.read_bytes()
                    info = tarfile.TarInfo("scrawl/" + path.relative_to(ROOT).as_posix())
                    info.size = len(content)
                    info.mode = 0o644
                    archive.addfile(info, io.BytesIO(content))
        raw.seek(0)
        with source.open("wb") as dest:
            shutil.copyfileobj(raw, dest)
    digest = hashlib.sha256(source.read_bytes()).hexdigest()
    template = (ROOT / "packaging/omarchy/PKGBUILD.in").read_text()
    recipe = template.replace("@VERSION@", version).replace("@SOURCE_SHA256@", digest)
    (output_dir / "PKGBUILD").write_text(recipe)
    metadata = subprocess.check_output(["makepkg", "--printsrcinfo"], cwd=output_dir, text=True)
    (output_dir / ".SRCINFO").write_text(metadata)
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist/omarchy")
    parser.add_argument("--prepare-only", action="store_true", help="prepare sources and PKGBUILD without building")
    parser.add_argument("makepkg_args", nargs=argparse.REMAINDER,
                        help="makepkg options after --: --syncdeps, --force, --nodeps, --nocheck, --noconfirm")
    args = parser.parse_args()
    makepkg_args = args.makepkg_args
    if makepkg_args[:1] == ["--"]:
        makepkg_args = makepkg_args[1:]
    allowed = {"--syncdeps", "--force", "--nodeps", "--nocheck", "--noconfirm"}
    if any(arg not in allowed for arg in makepkg_args):
        parser.error("unsupported makepkg option; use --syncdeps, --force, --nodeps, --nocheck or --noconfirm. "
                     "Reusing extracted sources or repackaging an old build is not supported.")
    if not shutil.which("makepkg"):
        parser.error("makepkg is required; install Omarchy's base-devel package")
    output_dir = args.output_dir.resolve()
    prepare(output_dir)
    print(f"Prepared recipe: {output_dir / 'PKGBUILD'}", flush=True)
    if args.prepare_only:
        return
    # Avoid inherited cross-compilation flags and target paths outside the build.
    env = dict(os.environ)
    for name in ("CARGO_ENCODED_RUSTFLAGS", "CARGO_BUILD_TARGET", "CARGO_TARGET_DIR"):
        env.pop(name, None)
    subprocess.run(["makepkg", "--cleanbuild", *makepkg_args], cwd=output_dir, env=env, check=True)
    package_paths = subprocess.check_output(["makepkg", "--packagelist"], cwd=output_dir, env=env, text=True)
    print(f"Package: {package_paths.strip()}")


if __name__ == "__main__":
    main()
