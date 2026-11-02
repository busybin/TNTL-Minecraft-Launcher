#!/bin/sh
# Sign one release asset for the launcher's update check.
# usage: scripts/sign-update.sh <asset> [key.pem]
# Writes <asset>.sig next to the asset; upload both to the same release.
set -eu
asset=${1:?usage: sign-update.sh <asset> [key.pem]}
key=${2:-update-key.pem}
[ -f "$asset" ] || { echo "no such file: $asset" >&2; exit 1; }
[ -f "$key" ] || { echo "no such key: $key" >&2; exit 1; }

digest="sha256:$(sha256sum "$asset" | cut -d' ' -f1)"
tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
printf '%s' "$digest" > "$tmp"
openssl pkeyutl -sign -inkey "$key" -rawin -in "$tmp" | xxd -p -c 256 > "$asset.sig"
echo "signed $asset"
echo "  digest  $digest"
echo "  sig     $asset.sig"
