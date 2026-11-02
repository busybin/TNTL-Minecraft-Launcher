#!/bin/sh
# Create an Ed25519 key pair for signed launcher updates.
# Run this offline and never keep the private key in the repository.
# usage: scripts/update-keygen.sh <absolute path outside the repository>
set -eu
key=${1:?usage: update-keygen.sh <absolute path outside the repository>}
case $key in
  /*) ;;
  *) echo "use an absolute path outside the repository" >&2; exit 1 ;;
esac
if root=$(git -C "$(dirname "$key")" rev-parse --show-toplevel 2>/dev/null); then
  echo "refusing to write a private key inside the worktree $root" >&2
  exit 1
fi
if [ -e "$key" ]; then
  echo "refusing to overwrite $key" >&2
  exit 1
fi
umask 077
openssl genpkey -algorithm ed25519 -out "$key"
pub=$(openssl pkey -in "$key" -pubout -outform DER | tail -c 32 | od -An -v -tx1 | tr -d ' \n')
printf 'private key: %s\n' "$key"
printf 'public key:  %s\n' "$pub"
printf 'build with:  TNTL_UPDATE_PUBKEY=%s\n' "$pub"
printf 'sign with:   scripts/sign-update.sh <asset> %s\n' "$key"
