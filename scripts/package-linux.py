#!/usr/bin/env python3
"""Build a self-contained Scrawl payload for another Omarchy machine.

Desktop/media libraries remain managed by the destination's package manager.
"""
import argparse
from datetime import datetime, timezone
import gzip
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib


ROOT = Path(__file__).resolve().parent.parent


def output(*args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    arch = platform.machine()
    targets = {
        "x86_64": ("x86_64-unknown-linux-gnu", "x86-64"),
        "aarch64": ("aarch64-unknown-linux-gnu", "generic"),
    }
    if platform.system() != "Linux" or arch not in targets:
        parser.error("Build on x86-64 or aarch64 Linux for the same destination architecture.")
    for tool in ("cargo", "strip", "readelf", "git"):
        if not shutil.which(tool):
            parser.error(f"Missing build tool: {tool}")
    target, cpu = targets[arch]
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target")))
    if not target_dir.is_absolute():
        target_dir = ROOT / target_dir
    build_env = dict(os.environ)
    # Override native/custom CPU flags so the archive can move between machines.
    build_env["CARGO_ENCODED_RUSTFLAGS"] = f"-C\x1ftarget-cpu={cpu}"
    subprocess.run([
        "nice", "-n", "10", "cargo", "build", "--release", "-j", "3", "--offline",
        "--locked", "--target", target, "--target-dir", str(target_dir),
    ], cwd=ROOT, env=build_env, check=True)
    binary = target_dir / target / "release/scrawl"
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if not re.fullmatch(r"[0-9A-Za-z.+-]+", version):
        parser.error("Package version is not suitable for an archive filename.")
    name = f"scrawl-{version}-linux-{arch}"
    destination = args.output_dir.resolve()
    destination.mkdir(parents=True, exist_ok=True)
    archive_path = destination / f"{name}.tar.gz"
    checksum_path = destination / f"{archive_path.name}.sha256"
    with tempfile.TemporaryDirectory(prefix=".scrawl-package-", dir=destination) as temp:
        stage = Path(temp) / name
        stage.mkdir(mode=0o755)
        files = {
            "bin/scrawl": binary,
            "install.sh": ROOT / "scripts/install-linux-archive.sh",
            "scripts/install-linux.sh": ROOT / "scripts/install-linux.sh",
            "assets/app-icon.png": ROOT / "assets/app-icon.png",
            "LICENSE": ROOT / "LICENSE",
            "README.md": ROOT / "docs/linux-archive.md",
            "docs/linux.md": ROOT / "docs/linux.md",
            "docs/linux-archive.md": ROOT / "docs/linux-archive.md",
        }
        for relative, source in files.items():
            dest = stage / relative
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, dest)
            dest.chmod(0o755 if relative in {"bin/scrawl", "install.sh", "scripts/install-linux.sh"} else 0o644)
        subprocess.run(["strip", "--strip-unneeded", str(stage / "bin/scrawl")], check=True)
        binary_version = output(str(stage / "bin/scrawl"), "--version")
        symbols = output("readelf", "--version-info", str(stage / "bin/scrawl"))
        glibc_versions = [match.group(1) for line in symbols.splitlines()
                          if "WEAK" not in line
                          for match in re.finditer(r"Name: GLIBC_([\d.]+)", line)]
        required_glibc = max(glibc_versions, key=lambda value: tuple(map(int, value.split("."))))
        built_at = datetime.now(timezone.utc)
        metadata = {
            "version": version, "binaryVersion": binary_version,
            "architecture": arch, "target": target, "cpu": cpu,
            "profile": "release", "builtAt": built_at.isoformat(),
            "sourceRevision": output("git", "rev-parse", "HEAD"),
            "sourceModified": bool(output("git", "status", "--porcelain")),
            "minimumGlibc": required_glibc,
            "runtimePackages": ["glibc", "gcc-libs", "hyprland", "wf-recorder", "ffmpeg",
                                "wl-clipboard", "slurp", "python-gobject", "python-cairo",
                                "gtk4-layer-shell", "libpulse"],
        }
        (stage / "build-info.json").write_text(json.dumps(metadata, indent=2) + "\n")
        checksums = [f"{sha256(path)}  {path.relative_to(stage).as_posix()}\n"
                     for path in sorted(stage.rglob("*")) if path.is_file()]
        (stage / "SHA256SUMS").write_text("".join(checksums))

        def normalize(info):
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            info.mtime = int(built_at.timestamp())
            info.mode = 0o755 if info.isdir() or info.mode & 0o111 else 0o644
            return info

        staged_archive = Path(temp) / "payload.tar.gz"
        with staged_archive.open("wb") as raw:
            with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as tar:
                    tar.add(stage, arcname=name, filter=normalize)
        staged_checksum = Path(temp) / "payload.sha256"
        staged_checksum.write_text(f"{sha256(staged_archive)}  {archive_path.name}\n")
        os.replace(staged_archive, archive_path)
        os.replace(staged_checksum, checksum_path)
    print(f"Archive: {archive_path}")
    print(f"Checksum: {checksum_path}")
    print(f"Size: {archive_path.stat().st_size / (1024 * 1024):.1f} MiB")
    print(f"Target: {arch}, glibc >= {required_glibc}")


if __name__ == "__main__":
    main()
