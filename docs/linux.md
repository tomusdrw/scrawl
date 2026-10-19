# Linux on Hyprland

Scrawl's Linux backend targets Hyprland on Wayland, including Omarchy. It records an output or a fixed screen rectangle, microphone audio, cursor position, and red freehand marks into the same `.scrawl` bundles as macOS. Recording and bundle processing run locally. This backend does not automatically send recordings to external sessions.

## Install

To move a prebuilt copy to another Omarchy machine, use the [transfer archive instructions](linux-archive.md). Run `python3 scripts/package-linux.py` to create an archive with an installer under `dist/`; the destination does not need Rust or this repository.

You need Rust/Cargo and a Hyprland session. On Arch Linux, the runtime packages are:

```sh
sudo pacman -S --needed wf-recorder ffmpeg wl-clipboard slurp python-gobject python-cairo gtk4-layer-shell libpulse
```

`pactl` comes from `libpulse`; microphone recording also needs a running PulseAudio-compatible server, usually `pipewire-pulse` on Omarchy. `python-gobject`, `python-cairo`, and `gtk4-layer-shell` provide the native controls and drawing overlay. The installer does not install system packages.

From the repository:

```sh
./scripts/install-linux.sh
~/.local/bin/scrawl doctor
```

The build uses `nice -n 10 cargo build -j 3 --offline --locked`. If your Cargo cache is empty, run `cargo fetch --locked` once with network access first. Use `--no-build` to install an already-built `target/debug/scrawl`, or `--prefix /absolute/user/directory` to choose the installation root. `CARGO_TARGET_DIR` is respected.

The default installation places the binary in `~/.local/bin/scrawl`, the launcher in `$XDG_DATA_HOME/applications/scrawl.desktop`, and its icon under `$XDG_DATA_HOME/icons/` (`XDG_DATA_HOME` defaults to `~/.local/share`). With `--prefix`, the launcher and icon go under that prefix's `share/`; a custom prefix must be discoverable through your desktop's data-directory search path to appear in its launcher. Add the binary directory to `PATH` if necessary. Reinstallation replaces the executable atomically, so an already-running process can finish using its previous version.

Two example bindings files are created under `$XDG_CONFIG_HOME/scrawl/` (`XDG_CONFIG_HOME` defaults to `~/.config`): `omarchy.lua` for Omarchy 4 and `hyprland.conf` for legacy Hyprland configuration. Existing copies of either file are preserved. The installer neither edits your compositor configuration nor starts Scrawl.

**Omarchy 4 uses Lua.** Add this line to `~/.config/hypr/bindings.lua`, which is loaded after Omarchy's defaults:

```lua
dofile(os.getenv("HOME") .. "/.config/scrawl/omarchy.lua")
```

Use the exact `dofile(...)` line printed by the installer if your config directory differs. The example registers `o.bind("ALT + SHIFT + S", "Scrawl record", command)` and corresponding Draw and Copy bindings. Check existing assignments with `omarchy menu keybindings --print` before enabling it. If a combination is already used, change the example shortcut or explicitly call `hl.unbind("ALT + SHIFT + S")` before loading your replacement. Validate the configuration after adding the line:

```sh
hyprctl reload
hyprctl configerrors
```

**Legacy Hyprland `.conf` configuration:** add this line to your own `hyprland.conf` instead:

```ini
source = ~/.config/scrawl/hyprland.conf
```

Use the path printed by the installer when your config directory differs. Load only the example matching your compositor's configuration format.

| Shortcut | Action |
| --- | --- |
| Alt+Shift+S | Start or finish a screen recording |
| Alt+Shift+D | Enter or leave Draw mode during recording |
| Alt+Shift+V | Copy the latest capture's plain-text summary and artifact paths |

These are example bindings; adjust them if your desktop already uses the same combinations.

## Record and review

Open **Scrawl** from your launcher, or run `scrawl app`, for the native control panel. Use the shortcuts above or the CLI:

```sh
scrawl toggle                         # Start a screen capture; run again to finish
scrawl start --mode window            # Capture the active window's fixed rectangle
scrawl start --mode area              # Drag an area with slurp; Esc cancels selection
scrawl start --no-audio               # Record without a microphone
scrawl start --no-ui                  # Record without the controls or drawing overlay
scrawl draw                          # Toggle explicit Draw mode
scrawl undo                          # Undo the last mark
scrawl status --json
scrawl stop                          # Finish and build the bundle
scrawl cancel                        # Discard the active recording
scrawl record --duration 5            # Record for five seconds from the terminal
scrawl inspect latest
scrawl inspect latest --json
scrawl preview latest
scrawl send latest --clipboard
scrawl reprocess latest --out /tmp/reprocessed.scrawl
```

`start`, `toggle`, and `record` accept `--mode screen|window|area`, `--no-audio`, `--out DIR`, and `--note TEXT`. Explicit output paths must be new directories. Finishing builds the bundle asynchronously; `scrawl status` reports its progress. A processing failure keeps the original recording so it can be reprocessed.

Screen mode selects the output under the pointer, falling back to Hyprland's focused output. Window mode captures the window's current screen rectangle, including anything placed over it, and does not follow the window when it moves. A window extending beyond an outer desktop edge is clipped to its output. Windows and areas crossing multiple outputs are rejected; move the window to one display or choose an area entirely within it. Fractional scale, rotated outputs, and negative display coordinates are accounted for in the selected geometry. The underlying [wf-recorder geometry option](https://github.com/ammen99/wf-recorder/blob/master/README.md) uses the same logical rectangles as `slurp`.

Draw mode temporarily takes pointer input for red strokes. Leave Draw mode to interact with the app underneath. Linux does not use the macOS hold-Shift drawing gesture, tap-versus-hold capture switcher, or arbitrary global click/key interception. Cursor position is sampled through Hyprland. Live marks are already present in the video; bundle rendering avoids adding them twice.

The clipboard contains plain text with absolute contact-sheet, frame, video, and bundle paths. Paste it into your terminal or agent composer. There is no automatic GUI paste or external-session delivery on Linux.

## Local transcription

Microphone audio remains in the recording and exported audio even without a speech model. Transcription is optional and uses a locally installed [whisper.cpp CLI](https://github.com/ggml-org/whisper.cpp/tree/master/examples/cli) and a model you provide. Scrawl does not download models.

Set these variables in the environment that starts Scrawl:

```sh
export SCRAWL_WHISPER_BIN=/absolute/path/to/whisper-cli
export SCRAWL_WHISPER_MODEL=/absolute/path/to/ggml-base.en.bin
scrawl doctor
```

Alternatively, merge these keys into `$XDG_CONFIG_HOME/scrawl/config.json`, preserving any other settings:

```json
{
  "linux": {
    "whisperBin": "/absolute/path/to/whisper-cli",
    "whisperModel": "/absolute/path/to/ggml-base.en.bin"
  }
}
```

Environment variables take precedence. Use absolute paths; `~` is not expanded inside JSON. Without a working CLI and model, captures retain their voice track and report that transcription is unavailable. Apple SpeechAnalyzer and Hex integration are not part of this Linux backend.

## Storage and limits

Captures use `$XDG_DATA_HOME/scrawl/captures/`, normally `~/.local/share/scrawl/captures/`. Configuration uses `$XDG_CONFIG_HOME/scrawl/`, normally `~/.config/scrawl/`. Daemon control uses a private runtime directory, with start/stop requests serialized so repeated shortcuts do not create competing recorders.

This first backend supports Hyprland only. GNOME, KDE, X11, iPhone mirroring, ScrawlKit phone ingest, macOS Settings/permission windows, and Sparkle updates are not implemented. Native isolated-window recording, automatic cropping, and automatic dev-site/session routing are also unavailable. Linux keeps the selected screen or rectangle; choose Window or Area for a tighter capture. A locked session, unavailable compositor capture protocol, or missing runtime package can prevent recording; `scrawl doctor` checks dependencies and compositor connectivity without starting a recording.

For development, run `nice -n 10 cargo test -j 3 --offline --locked -- --test-threads=2`, `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s helper -p test_linux.py`, and `python3 tests/linux_smoke.py target/debug/scrawl` after rebuilding. The service tests use isolated desktop adapters and real FFmpeg fixtures without touching the real clipboard or microphone. Geometry tests use JSON fixtures without changing the desktop. Test interactive selection, drawing alignment, and audio sync on an unlocked Hyprland session, then inspect `summary.md`, `scrawl.json`, `contact-sheet.png`, and individual `frames/*.png` in the resulting bundle.
