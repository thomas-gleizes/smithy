#!/usr/bin/env bash
# Downloads the latest smithy release from GitHub, verifies it against
# checksums.txt, and installs the smithy binary.
# See README.md#install for the other install methods (manual download, from source).
set -euo pipefail

REPO="thomas-gleizes/smithy"
INSTALL_DIR="${SMITHY_INSTALL_DIR:-/usr/local/bin}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
case "$os" in
    linux|darwin) ;;
    *) echo "error: unsupported OS '$os' (smithy ships linux and darwin builds only)" >&2; exit 1 ;;
esac

arch="$(uname -m)"
case "$arch" in
    x86_64|amd64) arch="amd64" ;;
    aarch64|arm64) arch="arm64" ;;
    *) echo "error: unsupported architecture '$arch'" >&2; exit 1 ;;
esac

archive="smithy_${os}_${arch}.tar.gz"
base_url="https://github.com/${REPO}/releases/latest/download"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

echo "Downloading ${archive}..."
curl -fL --progress-bar -o "${workdir}/${archive}" "${base_url}/${archive}"
curl -fL --progress-bar -o "${workdir}/checksums.txt" "${base_url}/checksums.txt"

echo "Verifying checksum..."
expected="$(awk -v f="$archive" '$2 == f { print $1 }' "${workdir}/checksums.txt")"
if command -v sha256sum >/dev/null 2>&1; then
    actual="$(sha256sum "${workdir}/${archive}" | awk '{ print $1 }')"
else
    actual="$(shasum -a 256 "${workdir}/${archive}" | awk '{ print $1 }')"
fi
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
    echo "error: checksum mismatch for ${archive}" >&2
    exit 1
fi

echo "Extracting..."
tar xzf "${workdir}/${archive}" -C "$workdir" smithy

if [ -w "$INSTALL_DIR" ]; then
    mv "${workdir}/smithy" "${INSTALL_DIR}/smithy"
else
    sudo mv "${workdir}/smithy" "${INSTALL_DIR}/smithy"
fi

echo "Installed to ${INSTALL_DIR}/smithy"
"${INSTALL_DIR}/smithy" --version
