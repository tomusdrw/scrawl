#!/usr/bin/env bash
# Per-user Linux install. Does not launch Scrawl or edit compositor configuration.
set -euo pipefail

usage() {
  cat <<'HELP'
Usage: scripts/install-linux.sh [--no-build | --binary FILE] [--prefix DIR]

Build offline and install the Linux binary, launcher, icon, and example bindings.
  --no-build    Install the existing target/debug/scrawl binary.
  --binary FILE Install a prebuilt binary without running Cargo.
  --prefix DIR  Install bin/ and share/ under DIR (default: ~/.local).

XDG_DATA_HOME and XDG_CONFIG_HOME are respected for the default installation.
Existing scrawl/omarchy.lua and scrawl/hyprland.conf bindings are preserved.
HELP
}

prefix="${HOME:?HOME must be set}/.local"
custom_prefix=false
build=true
binary=""
while (($#)); do
  case "$1" in
    --no-build) build=false; shift ;;
    --binary)
      if (($# < 2)) || [[ -z "$2" ]]; then
        printf '%s\n' '--binary needs an executable path' >&2
        exit 2
      fi
      binary="$2"
      build=false
      shift 2
      ;;
    --prefix)
      if (($# < 2)) || [[ -z "$2" ]]; then
        printf '%s\n' '--prefix needs a directory' >&2
        exit 2
      fi
      prefix="$2"
      custom_prefix=true
      shift 2
      ;;
    --help|-h) usage; exit 0 ;;
    *) printf 'Unknown argument: %s\n' "$1" >&2; usage >&2; exit 2 ;;
  esac
done

if [[ "$(uname -s)" != Linux ]]; then
  printf '%s\n' 'Use scripts/install-app.sh on macOS.' >&2
  exit 1
fi
if [[ "$prefix" != /* || "$prefix" == *$'\n'* || "$prefix" == *$'\r'* ]]; then
  printf '%s\n' '--prefix must be an absolute directory without line breaks.' >&2
  exit 2
fi

repo_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
if "$build"; then
  (cd -- "$repo_dir" && nice -n 10 cargo build -j 3 --offline --locked)
fi
target_dir="${CARGO_TARGET_DIR:-$repo_dir/target}"
[[ "$target_dir" == /* ]] || target_dir="$repo_dir/$target_dir"
binary="${binary:-$target_dir/debug/scrawl}"
if [[ ! -f "$binary" || ! -x "$binary" ]]; then
  printf 'No executable found at %s. Run this installer without --no-build.\n' "$binary" >&2
  exit 1
fi
if ! "$binary" --version >/dev/null; then
  printf '%s\n' 'The Scrawl binary cannot run here. Check the archive architecture and required system libraries.' >&2
  exit 1
fi

if "$custom_prefix"; then
  data_dir="$prefix/share"
else
  data_dir="${XDG_DATA_HOME:-$HOME/.local/share}"
fi
config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/scrawl"
if [[ "$data_dir" != /* || "$config_dir" != /* ]]; then
  printf '%s\n' 'XDG data and config directories must be absolute paths.' >&2
  exit 2
fi
install -d -- "$prefix/bin" "$data_dir/applications" "$data_dir/icons/hicolor/1024x1024/apps" "$config_dir"
# Stage on the destination filesystem before replacing the directory entry. A
# running Scrawl keeps its old executable inode and does not block reinstalling.
bin_stage="$(mktemp "$prefix/bin/.scrawl.XXXXXX")"
trap '[[ -z "${bin_stage:-}" ]] || rm -f -- "$bin_stage"' EXIT
install -m 755 -- "$binary" "$bin_stage"
mv -fT -- "$bin_stage" "$prefix/bin/scrawl"
bin_stage=""
install -m 644 -- "$repo_dir/assets/app-icon.png" "$data_dir/icons/hicolor/1024x1024/apps/scrawl.png"

# Desktop Exec has its own quoting rules, separate from shell quoting.
desktop_arg() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//\$/\\\$}"
  value="${value//\`/\\\`}"
  value="${value//%/%%}"
  # Escape the backslashes once more for the desktop-entry string parser.
  value="${value//\\/\\\\}"
  printf '"%s"' "$value"
}
exec_path="$(desktop_arg "$prefix/bin/scrawl")"
cat > "$data_dir/applications/scrawl.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Scrawl
Comment=Record your screen, voice, and drawings
Exec=$exec_path app
Icon=scrawl
Terminal=false
Categories=AudioVideo;Recorder;
StartupNotify=false
DESKTOP
chmod 644 -- "$data_dir/applications/scrawl.desktop"

# Lua strings contain shell commands, so quote the path for the shell first,
# then escape the resulting command as a Lua double-quoted string.
lua_string() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  value="${value//$'\n'/\\n}"
  value="${value//$'\r'/\\r}"
  value="${value//$'\t'/\\t}"
  printf '"%s"' "$value"
}
shell_binary="'${prefix//\'/\'\\\'\'}/bin/scrawl'"

# Keep the user's bindings, including an existing symlink. noclobber closes the
# check/write race, so a concurrent install or editor cannot lose its contents.
lua_bindings="$config_dir/omarchy.lua"
if [[ -e "$lua_bindings" || -L "$lua_bindings" ]]; then
  printf 'Preserved existing bindings: %s\n' "$lua_bindings"
else
  if (umask 077; set -o noclobber; cat > "$lua_bindings" <<BINDINGS
-- Scrawl example bindings for Omarchy 4's Lua configuration.
-- Load this file with dofile(...) from your hypr/bindings.lua.
-- If a shortcut is already assigned, change it or unbind it before loading.
o.bind("ALT + SHIFT + S", "Scrawl record", $(lua_string "$shell_binary toggle"))
o.bind("ALT + SHIFT + D", "Scrawl draw", $(lua_string "$shell_binary draw"))
o.bind("ALT + SHIFT + V", "Scrawl copy latest", $(lua_string "$shell_binary send latest --clipboard"))
BINDINGS
  ); then
    printf 'Created example bindings: %s\n' "$lua_bindings"
  else
    printf 'Bindings already exist; preserved %s\n' "$lua_bindings"
  fi
fi

bindings="$config_dir/hyprland.conf"
if [[ -e "$bindings" || -L "$bindings" ]]; then
  printf 'Preserved existing bindings: %s\n' "$bindings"
else
  if (umask 077; set -o noclobber; cat > "$bindings" <<BINDINGS
# Legacy Hyprland .conf example; Omarchy 4 uses scrawl/omarchy.lua instead.
# Add a source = line for this file to your legacy Hyprland config yourself.
# Toggle a screen recording, enter/leave Draw mode, or copy the latest summary.
bind = ALT SHIFT, S, exec, $shell_binary toggle
bind = ALT SHIFT, D, exec, $shell_binary draw
bind = ALT SHIFT, V, exec, $shell_binary send latest --clipboard
BINDINGS
  ); then
    printf 'Created example bindings: %s\n' "$bindings"
  else
    printf 'Bindings already exist; preserved %s\n' "$bindings"
  fi
fi

if command -v update-desktop-database >/dev/null 2>&1; then
  update-desktop-database "$data_dir/applications" >/dev/null 2>&1 || true
fi
printf '\nInstalled %s\n' "$prefix/bin/scrawl"
printf 'Run: %s doctor\n' "$prefix/bin/scrawl"
hypr_config_dir="${XDG_CONFIG_HOME:-$HOME/.config}/hypr"
if [[ ! -f "$hypr_config_dir/hyprland.lua" && -f "$HOME/.config/hypr/hyprland.lua" ]]; then
  hypr_config_dir="$HOME/.config/hypr"
fi
if [[ -f "$hypr_config_dir/hyprland.lua" ]]; then
  printf 'Omarchy Lua configuration detected. Add this line to %s/bindings.lua:\n\n  dofile(%s)\n\n' "$hypr_config_dir" "$(lua_string "$lua_bindings")"
  printf '%s\n' 'Then validate with: hyprctl reload && hyprctl configerrors'
else
  printf 'For legacy Hyprland .conf, add this line to your configuration:\n\n  source = %s\n\n' "$bindings"
  printf 'For Omarchy Lua, load %s from hypr/bindings.lua with dofile(...).\n' "$lua_bindings"
fi
printf '%s\n' 'Scrawl was not started. Open Scrawl from your launcher or run scrawl app.'
