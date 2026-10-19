# Linux Port Implementation Plan

**Goal:** Build and validate a native Hyprland/Wayland Scrawl with recording, drawing, narration, and portable bundles.

**Architecture:** Share the portable Rust modules and select a Linux entry point at compile time. Rust owns a private Unix-socket recording service and launches native capture/decoding processes. An embedded GTK4 helper provides desktop controls and drawing.

**Tech stack:** Existing Rust dependencies; wf-recorder, FFmpeg, Python/PyGObject, GTK4 layer shell, wl-clipboard, optional whisper.cpp.

**Spec:** `docs/superpowers/specs/2026-10-19-linux-design.md`

## Constraints

- Preserve Cargo.toml/Cargo.lock and macOS behavior.
- Keep work in the existing T3 worktree, with no publish or external sends.
- Use per-user paths and private IPC; no system configuration replacement.
- Preserve media after processing errors; reject destructive output reuse.

## Tasks

- [x] Platform boundary and Linux CLI: move the existing entry-point body into a macOS-only include; share model/render/keyframe/autocrop/window types/handoff; add Linux paths and clipboard. Verify Linux compile and shared tests.
- [x] Media pipeline (`src/linux/media.rs`): ffprobe/FFmpeg decode, bounded candidates, keyframes, optional Whisper, artifact writing and reprocess. Test invalid media, static vs changing scenes, timestamp handling, and real fixture bundles.
- [x] Desktop helper (`helper/linux.py`): GTK4 controls, screen/area/window selection, draw/undo/escape, status and errors. Use an explicit JSON socket protocol. Compile-check and smoke-test GTK where available.
- [x] Recording service (`src/linux/{mod,session,desktop}.rs`): geometry, recorder lifecycle, private socket, concurrent starts, responsive finishing, cancellation, recoverable errors. Test geometry/scaling, IPC and child lifecycle failures.
- [x] Packaging/docs and integrated verification: per-user install and example bindings, dependency doctor, command smoke tests, generated artifact inspection, review.

## Review focus

- Fractional scaling, negative monitor positions, and windows crossing output boundaries.
- Recorder startup failure and stale socket recovery without signaling unrelated PIDs.
- UI responsiveness while long recordings are being decoded/transcribed.
- No silent loss of voice or duplicate ink; nonzero subprocess errors are visible.
- No overwrite of existing output or clipboard mutation during automated tests.


## Verification outcome

- Linux build: `nice -n 10 cargo build -j 3 --offline --locked` passed; Cargo manifest/lockfile unchanged.
- Shared/Linux Rust suite: 95 passed, 1 existing live-network test ignored.
- GTK helper suite: 12 passed; native render/smoke, singleton and disconnect cleanup checked.
- Compiled-service integration suite: 10 passed using isolated desktop adapters and real FFmpeg video/audio, without touching the user's clipboard or microphone.
- Reviewed and fixed grouped ink visibility, initial capture clock, shortcut stroke flushing, relative output destinations, and Preview/Copy destination handling.
- Installed per-user binary, launcher, shortcut examples and the official Arch wf-recorder binary (user-local); runtime doctor passes. Local Whisper model is not configured.
- Installed binary completed an actual 1.7-second Wayland capture at 2560×1080. The desktop is locked, so this captured the lock screen; its manifest, summary, contact sheet, and frame have been inspected alongside the media fixtures and GTK renders. Interactive drawing alignment, microphone synchronization, and local Whisper inference still need a manual check on an unlocked desktop with a model configured.
