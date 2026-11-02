#!/bin/sh
# Create an Ed25519 key pair for signed launcher updates.
# Run this offline; keep the private key out of the repository and backups.
# usage: scripts/update-keygen.sh [update-key.pem]
set -eu
key=${1:-update-key.pem}
if [ -e "$key" ]; then
  echo "refusing to overwrite $key" >&2
  exit 1
fi
umask 077
openssl genpkey -algorithm ed25519 -out "$key"
pub=$(openssl pkey -in "$key" -pubout -outform DER | tail -c 32 | xxd -p -c 32)
printf 'private key: %s\n' "$key"
printf 'public key:  %s\n' "$pub"
printf 'build with:  TNTL_UPDATE_PUBKEY=%s\n' "$pub"
printf 'sign with:   scripts/sign-update.sh <asset> %s\n' "$key"
