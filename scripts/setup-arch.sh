#!/usr/bin/env bash
# Install the Linux build dependencies on Arch-based distributions.
set -euo pipefail

if ! command -v pacman >/dev/null 2>&1; then
    echo "This script targets Arch-based distributions." >&2
    exit 1
fi

sudo pacman -S --needed --noconfirm \
    base-devel pkgconf openssl git curl \
    nodejs pnpm \
    webkit2gtk-4.1 librsvg libappindicator-gtk3 patchelf \
    mold

if ! command -v rustup >/dev/null 2>&1; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain stable
    # shellcheck disable=SC1091
    source "$HOME/.cargo/env"
fi

rustup default stable
rustup component add rustfmt clippy

echo "Done. Ensure $HOME/.cargo/bin is in PATH."
