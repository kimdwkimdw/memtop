#!/usr/bin/env bash
set -euo pipefail

repo_root="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
launcher="${repo_root}/bin/memtop"

if [[ ! -x "${launcher}" ]]; then
  echo "Expected executable launcher at ${launcher}" >&2
  exit 1
fi

case "$(uname -s)" in
  Darwin)
    case "$(uname -m)" in
      arm64) target="aarch64-apple-darwin" ;;
      x86_64) target="x86_64-apple-darwin" ;;
      *)
        echo "Unsupported test architecture: $(uname -m)" >&2
        exit 1
        ;;
    esac
    ;;
  Linux)
    case "$(uname -m)" in
      aarch64 | arm64) target="aarch64-unknown-linux-musl" ;;
      x86_64 | amd64) target="x86_64-unknown-linux-musl" ;;
      *)
        echo "Unsupported test architecture: $(uname -m)" >&2
        exit 1
        ;;
    esac
    ;;
  *)
    echo "Unsupported test platform: $(uname -s)" >&2
    exit 1
    ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "${tmp}"' EXIT

package_root="${tmp}/lib/node_modules/@arthurkim/memtop"
global_bin="${tmp}/bin"
test_path="${tmp}/path"
mkdir -p "${package_root}/bin" "${package_root}/vendor/${target}" "${global_bin}" "${test_path}"

cp "${launcher}" "${package_root}/bin/memtop"
chmod 0755 "${package_root}/bin/memtop"

cat > "${package_root}/vendor/${target}/memtop" <<'SCRIPT'
#!/bin/sh
printf 'fake-native:%s\n' "$*"
SCRIPT
chmod 0755 "${package_root}/vendor/${target}/memtop"

uname_path="$(command -v uname)"
ln -s "${uname_path}" "${test_path}/uname"
readlink_path="$(command -v readlink)"
ln -s "${readlink_path}" "${test_path}/readlink"

cat > "${test_path}/node" <<'SCRIPT'
#!/bin/sh
echo "node must not be required to run memtop" >&2
exit 99
SCRIPT
chmod 0755 "${test_path}/node"

ln -s "${package_root}/bin/memtop" "${global_bin}/memtop"

output="$(PATH="${test_path}" "${global_bin}/memtop" --version 2>&1)"
expected="fake-native:--version"

if [[ "${output}" != "${expected}" ]]; then
  echo "Unexpected launcher output" >&2
  echo "Expected: ${expected}" >&2
  echo "Actual:   ${output}" >&2
  exit 1
fi
