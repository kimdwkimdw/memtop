#!/bin/sh
set -eu

if command -v cargo >/dev/null 2>&1; then
  exec cargo "$@"
fi

if command -v mise >/dev/null 2>&1; then
  exec mise exec -- cargo "$@"
fi

echo "cargo not found; install Rust or run through mise" >&2
exit 127
