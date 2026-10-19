# Scrawl on Omarchy

Scrawl's Linux backend targets x86-64 Omarchy with Hyprland/Wayland. It records a display or a fixed screen rectangle, microphone audio, cursor position, and red freehand marks into the same `.scrawl` bundles as macOS. Capture, transcription, and bundle processing run locally.

## Build and install the package

The repository builds a native Arch package. Pacman owns the executable, launcher, icon, documentation, and shortcut examples; it resolves runtime dependencies and handles upgrades and removal.

Install the build tools, then run from the source checkout:

```sh
omarchy pkg add base-devel rust alsa-lib
python3 scripts/build-omarchy-package.py -- --syncdeps
sudo pacman -U "$(cd dist/omarchy && makepkg --packagelist)"
/usr/bin/scrawl doctor
/usr/bin/scrawl app
```

`--syncdeps` lets makepkg install declared build/runtime dependencies and may request sudo. The package requires Hyprland, wf-recorder, FFmpeg, wl-clipboard, slurp, PipeWire Pulse, GTK4 layer shell, PyGObject/Cairo, and xdg-utils. A running Omarchy desktop supplies the Wayland and audio sessions. The package is not yet published in an Omarchy repository or AUR; the commands above build the current checkout.

The builder creates a checksummed source snapshot, `PKGBUILD`, and `.SRCINFO` in `dist/omarchy/`. It uses a clean build directory so removed source files cannot survive a rebuild. `prepare()` fetches the locked Rust dependencies; compilation and tests run offline with `--frozen`. The release binary uses a generic x86-64 CPU target. Use `--prepare-only` to generate the recipe without compiling, or `--output-dir DIR` to choose a build directory. Supported makepkg options after `--` are `--syncdeps`, `--force`, `--nodeps`, `--nocheck`, and `--noconfirm`; use `--force` to rebuild an existing package. The Python desktop helper is embedded in the executable.

Installed paths:

- `/usr/bin/scrawl`: command-line tool and desktop application.
- `/usr/share/applications/scrawl.desktop`: launcher entry.
- `/usr/share/doc/scrawl/`: setup guide and Omarchy shortcut examples.

Finish any active capture and run `scrawl quit` before upgrading. Remove the package with `sudo pacman -R scrawl`; captures and personal configuration remain in the XDG directories described below. If a prior manual installation exists in `~/.local/bin`, it can shadow the packaged command: check `type -a scrawl` and use `/usr/bin/scrawl` while migrating. The launcher and supplied shortcuts use the packaged executable explicitly.

## Omarchy shortcuts

On Omarchy 4, check existing assignments with `omarchy menu keybindings --print`, then add this line to `~/.config/hypr/bindings.lua`:

```lua
dofile("/usr/share/doc/scrawl/omarchy.lua")
```

The package never writes into a home directory or edits the compositor configuration. To customize the bindings, copy the example into `~/.config/scrawl/` and load that copy instead. If a combination is already assigned, choose another or explicitly unbind it before loading the example. Validate after editing:

```sh
hyprctl reload
hyprctl configerrors
```

| Shortcut | Action |
| --- | --- |
| Alt+Shift+S | Start or finish a screen recording |
| Alt+Shift+D | Enter or leave Draw mode during recording |
| Alt+Shift+V | Copy the latest capture's plain-text summary and artifact paths |

Scrawl also works from the application launcher without enabling shortcuts. Omarchy versions using legacy Hyprland `.conf` configuration can bind these same `/usr/bin/scrawl toggle`, `draw`, and `send latest --clipboard` commands manually.

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

Microphone audio remains in the recording and exported audio even without a speech model. Optional transcription uses the [whisper.cpp CLI](https://github.com/ggml-org/whisper.cpp/tree/master/examples/cli) and a separately supplied compatible model:

```sh
omarchy pkg add whisper-cpp
```

GPU backends are separate optional packages: `ggml-vulkan` for Vulkan (AMD/Intel/NVIDIA) or `ggml-cuda` for NVIDIA CUDA. Install the backend appropriate to the GPU and driver. Whisper attempts GPU use automatically; verify its runtime log selects a GPU backend without initialization errors. `scrawl doctor` checks executable/model availability, not GPU acceleration.

An existing whisper.cpp-format model from another local application, including Voxtype, can be shared by pointing Scrawl at the same file. Scrawl does not download models or alter another application's configuration. Language detection is automatic and translation is disabled. Each capture starts a new Whisper process and loads the model.

Set these variables in the environment that starts Scrawl:

```sh
export SCRAWL_WHISPER_BIN=/usr/bin/whisper-cli
export SCRAWL_WHISPER_MODEL=/absolute/path/to/ggml-base.en.bin
scrawl doctor
```

Alternatively, merge these keys into `$XDG_CONFIG_HOME/scrawl/config.json`, preserving any other settings:

```json
{
  "linux": {
    "whisperBin": "/usr/bin/whisper-cli",
    "whisperModel": "/absolute/path/to/ggml-base.en.bin"
  }
}
```

Environment variables take precedence. Use absolute paths; `~` is not expanded inside JSON. Without a working CLI and model, captures retain their voice track and report that transcription is unavailable. Apple SpeechAnalyzer and Hex integration are not part of this Linux backend.

## Storage and limits

Captures use `$XDG_DATA_HOME/scrawl/captures/`, normally `~/.local/share/scrawl/captures/`. Configuration uses `$XDG_CONFIG_HOME/scrawl/`, normally `~/.config/scrawl/`. Daemon control uses a private runtime directory, with start/stop requests serialized so repeated shortcuts do not create competing recorders.

This first backend supports Hyprland only. GNOME, KDE, X11, iPhone mirroring, ScrawlKit phone ingest, macOS Settings/permission windows, and Sparkle updates are not implemented. Native isolated-window recording, automatic cropping, and automatic dev-site/session routing are also unavailable. Linux keeps the selected screen or rectangle; choose Window or Area for a tighter capture. A locked session, unavailable compositor capture protocol, or missing runtime package can prevent recording; `scrawl doctor` checks dependencies and compositor connectivity without starting a recording.

For development, run `nice -n 10 cargo test -j 3 --offline --locked -- --test-threads=2`, `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s helper -p test_linux.py`, and `python3 tests/linux_smoke.py target/debug/scrawl` after rebuilding. The service tests use isolated desktop adapters and real FFmpeg fixtures without touching the real clipboard or microphone. Geometry tests use JSON fixtures without changing the desktop. The package's `check()` runs these suites against its release build. `python3 tests/omarchy_builder.py` checks that rebuilds use changed sources and discard removed files. `python3 tests/omarchy_package.py dist/omarchy/scrawl-*.pkg.tar.zst` verifies metadata, the installed binary, and an isolated pacman install/reinstall/remove cycle. The Omarchy package workflow runs this in a fresh Arch container and publishes the package as a CI artifact.

Test interactive selection, drawing alignment, and audio sync on a running Hyprland session, then inspect `summary.md`, `scrawl.json`, `contact-sheet.png`, and individual `frames/*.png` in the resulting bundle. Automated checks do not establish microphone quality, drawing latency, or GPU transcription performance on every device.
