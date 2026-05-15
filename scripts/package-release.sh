#!/usr/bin/env bash
set -euo pipefail

: "${TARGET:?TARGET environment variable is required}"

binary_name="${BINARY_NAME:-memtop}"
release_dir="${RELEASE_DIR:-target/${TARGET}/release}"
dist_root="${DIST_ROOT:-dist}"
dest="${dist_root}/${TARGET}"
binary_path="${release_dir}/${binary_name}"
archive_entry="${binary_name}-${TARGET}"

mkdir -p "${dest}"

if [[ ! -f "${binary_path}" ]]; then
  echo "Binary ${binary_path} not found" >&2
  exit 1
fi

cp "${binary_path}" "${dest}/${archive_entry}"
chmod 0755 "${dest}/${archive_entry}"
tar -C "${dest}" -czf "${dest}/${archive_entry}.tar.gz" "${archive_entry}"

(
  cd "${dest}"
  checksum_name="${archive_entry}.sha256"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${archive_entry}.tar.gz" > "${checksum_name}"
  else
    shasum -a 256 "${archive_entry}.tar.gz" > "${checksum_name}"
  fi
)
