# Linux port

Build a usable Scrawl for the developer's Omarchy / Hyprland Wayland desktop. Preserve the existing macOS implementation and the portable Rust bundle format, keyframe selection, annotation, contact sheet, and summary algorithms. This is a native desktop port, not a website.

The implementation defaults chosen for this port are Rust orchestration, wf-recorder for compositor capture with microphone audio, FFmpeg for media decoding, optional local whisper.cpp for narration, and a small Python GTK4 + layer-shell helper for controls and drawing. No new Cargo dependencies or lockfile changes. Runtime dependencies are diagnosed with actionable messages. Captures use XDG data storage, configuration uses XDG config storage, and IPC uses a private runtime directory.

Users can start/stop via `scrawl toggle` bound to Alt+Shift+S, record a screen or a fixed window/area rectangle, choose silent recording, draw red strokes in an explicit Draw mode, undo, cancel, inspect/reprocess bundles, preview output, and copy a plain-text summary with absolute artifact paths. The GTK helper only renders controls and reports drawing events; Rust owns session state. Window capture records the chosen screen rectangle, including occluding windows. Normal Wayland input stays private: this version does not intercept arbitrary application keys or clicks. Cursor position can be sampled through Hyprland.

The recording process must be stopped and reaped on stop/cancel/shutdown, with concurrent starts serialized and failed processes reported. Finishing is asynchronous so status remains responsive. A failed build preserves the recording for reprocessing. Explicit output paths must not overwrite an existing bundle. No automatic sending to external sessions.

Reuse visual keyframe selection: static screens stay compact; real changes and recorded marks select frames. Decode small candidates with a memory bound and decode selected frames at higher quality. Live ink already present in the video must not be rendered twice. Preserve voice in the video/audio even when no speech model is configured, and make that limitation visible. Never download speech models automatically.

Installation is per-user: binary, desktop entry, and an example Hyprland bindings file. Do not replace user compositor configuration. macOS-only iPhone capture, Sparkle, the native Apple permission/settings windows, and universal GNOME/KDE support are outside this first Linux backend and must be documented honestly.

Verify unit tests, real FFmpeg fixture bundles, process lifecycle/error paths, a GTK smoke test, and live capture where the compositor is available. Inspect generated summary/manifest and actual contact-sheet/keyframe images. A locked desktop may prevent live validation; report it rather than manufacturing success.
