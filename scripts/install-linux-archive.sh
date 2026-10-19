#!/usr/bin/env bash
# Entry point copied to install.sh in the transfer archive.
set -euo pipefail
archive_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
case "${1:-}" in
  --help|-h)
    printf '%s\n' 'Usage: ./install.sh [--prefix /absolute/directory]' \
      'Install the included Scrawl binary, launcher, icon, and shortcut examples.' \
      'No compiler is required. See README.md for Omarchy runtime dependencies.'
    exit 0
    ;;
esac
if [[ "$(uname -s)" != Linux ]]; then
  printf '%s\n' 'This archive is for Linux.' >&2
  exit 1
fi
if ! (cd -- "$archive_dir" && sha256sum --quiet --check SHA256SUMS); then
  printf '%s\n' 'Archive integrity check failed. Extract a fresh copy before installing.' >&2
  exit 1
fi
exec "$archive_dir/scripts/install-linux.sh" "$@" --binary "$archive_dir/bin/scrawl"
