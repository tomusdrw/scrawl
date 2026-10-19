# Move Scrawl to another Omarchy machine

This archive contains a prebuilt Scrawl binary with its GTK helper embedded, a per-user installer, launcher, icon, license, and shortcut examples. You do not need Rust, Cargo, or the source repository on the destination machine. It does not include personal configuration, recordings, transcription models, or system libraries.

Use a machine with the architecture named in the archive (`x86_64` for this build), running Omarchy with Hyprland/Wayland. `build-info.json` records the build and required glibc symbol version. This is a Linux preview of Scrawl 0.3.2; see `docs/linux.md` for the supported features and limits.

## Install on the destination machine

Copy the `.tar.gz` and its `.sha256` file to the same directory. Verify and extract them (substitute the exact filename):

```sh
sha256sum --check scrawl-0.3.2-linux-x86_64.tar.gz.sha256
tar -xzf scrawl-0.3.2-linux-x86_64.tar.gz
cd scrawl-0.3.2-linux-x86_64
```

Install the runtime dependencies through Omarchy:

```sh
omarchy pkg add wf-recorder ffmpeg wl-clipboard slurp python-gobject python-cairo gtk4-layer-shell libpulse
```

On older Omarchy versions without `omarchy pkg add`, use `sudo pacman -S --needed` with the same package list. Microphone recording needs a running PulseAudio-compatible server, normally `pipewire-pulse` on Omarchy. If the loader reports an unavailable glibc version, update the destination through `omarchy update` before retrying.

Install and launch:

```sh
./install.sh
~/.local/bin/scrawl doctor
~/.local/bin/scrawl app
```

The installer checks the archive contents and whether the binary can run before writing installation files. It installs under `~/.local`, respects XDG data/config paths, and preserves existing Scrawl shortcut files. It does not edit your Hyprland configuration or start recording. To use another installation prefix, pass `--prefix /absolute/path`.

The installer prints the line needed to enable the shortcut examples in your version of Hyprland. Follow `docs/linux.md` to enable them. The launcher works without shortcuts. Without a local Whisper model, Scrawl still records voice but does not transcribe it; optional transcription setup is also in `docs/linux.md`.

Once installed, you can remove the extracted archive folder. Captures are saved under `~/.local/share/scrawl/captures/` by default. To replace an older install, finish any active capture, run `~/.local/bin/scrawl quit`, and run the new archive's installer.

## Build another archive from the repository

```sh
python3 scripts/package-linux.py
```

This builds an optimized release with a generic CPU target, using the existing Cargo lockfile and offline dependency cache. The archive and checksum are written to `dist/`. It requires Python 3.11+, Rust/Cargo, and binutils (`strip` and `readelf`) on the build machine. Run `cargo fetch --locked` first if the Cargo cache is empty. `--output-dir DIR` changes the destination. Desktop/media dependencies are installed on the destination rather than bundled from the build machine.
