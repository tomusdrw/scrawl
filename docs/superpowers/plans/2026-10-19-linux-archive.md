# Linux Transfer Archive Implementation Plan

**Goal:** Transfer the current Scrawl Linux port to another x86-64 Omarchy/Wayland machine without compiling it there.

**Architecture:** Package a stripped release binary, the existing per-user installer, icon, license, documentation, build metadata, and SHA-256 checksums in a tar.gz archive. Runtime desktop/media dependencies come from Omarchy's package manager. The embedded GTK helper travels inside the binary.

**Constraints:** No Cargo dependency changes, system package installation, publishing, or changes to the current desktop configuration. Include no recordings, user settings, credentials, or development caches. Keep the existing source-install workflow working. Use a generic CPU target for transfers between machines.

- [x] Add `--binary PATH` to `scripts/install-linux.sh`, bypassing Cargo and checking that the executable runs before writing installation files.
- [x] Add `scripts/package-linux.py` to build a release archive under ignored `dist/`, and `scripts/install-linux-archive.sh` as its checksum-verifying installer entry point. Include transfer instructions in `docs/linux-archive.md` and link them from `docs/linux.md`.
- [x] Build the archive, extract it into a path containing spaces, verify checksums and dependencies, and install under an isolated prefix/config/data directory. Verify the binary, launcher, helper smoke check, preservation of existing binding files, and rejection of corrupted payloads.
- [x] Review packaging changes and hand over the archive with destination install commands.

**Review focus:** No source-tree dependency after extraction; paths with spaces/apostrophes; no user configuration copied; no installer writes before integrity/executable checks; honest architecture and runtime requirements.

**Validation:** Release archive 1.7 MiB, x86-64 generic CPU, glibc >= 2.35. Four archive/install smoke tests and ten compiled recording-service tests pass against the extracted binary. Runtime doctor passes; optional Whisper model remains unconfigured. Scoped packaging review found no important defects. No current installation or Hyprland configuration was changed.
