# Development workflow

## Toolchain

- Rust stable (see `rust-toolchain.toml`), `rustfmt` and `clippy`.
- Node 22 LTS and pnpm for the desktop frontend.
- Linux GUI build dependencies: `webkit2gtk-4.1`, `librsvg`, `libappindicator-gtk3`,
  `patchelf`, `pkgconf`, `openssl`, `base-devel`.
- Windows: WebView2 runtime (preinstalled on Windows 10/11) and the MSVC
  toolchain.

`scripts/setup-arch.sh` installs the Linux side on Arch-based distributions.
Other distributions map the same packages.

## Commands

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
bash scripts/check-endpoints.sh
cargo run -p tntl-cli -- --help
```

## Headless end-to-end check

The CLI is the integration test. On a machine without a display, run the game
under Xvfb (Linux):

```sh
export TNTL_ROOT="$HOME/tmp/tntl-smoke"
export JAVA_HOME=/path/to/jdk-17
cargo run -p tntl-cli -- install 1.20.1
cargo run -p tntl-cli -- verify 1.20.1
xvfb-run -a -s "-screen 0 1280x720x24 +extension GLX" \
  cargo run -p tntl-cli -- launch 1.20.1 --timeout 40
```

Success looks like `Setting user:`, `Backend library: LWJGL` and
`Sound engine started` in the instance log, and a clean exit after the timeout.
Without software GL (`LIBGL_ALWAYS_SOFTWARE=1`) the client may fail in the
render thread on a server GPU-less host.

Mute the instance before a headless run so the host does not play the menu
music: put `soundCategory_master:0.0` (and the other `soundCategory_*` keys) in
`<instance>/minecraft/options.txt`. Do not point
`-Dorg.lwjgl.openal.libname` at a missing library; the client crashes during
sound initialization instead of falling back to silence.

## Packaging

```sh
cd apps/desktop
pnpm tauri build --bundles deb,appimage   # Linux
pnpm tauri build --bundles nsis           # Windows installer (works cross-compiled too)
```

Bundles land in `$CARGO_TARGET_DIR/release/bundle/`. The AppImage is
self-contained; the deb expects the distribution to provide webkit2gtk-4.1.
The NSIS installer is configured but has not been built on Windows yet.

## Bundles

The GUI needs a desktop session (the frontend loop is "edit, rebuild, look at the
window"); core work, tests and CI run headless.

Build bundles with the source path remapped, so the binary is reproducible and
does not embed the builder's absolute paths:

```sh
export RUSTFLAGS="--remap-path-prefix=$HOME=/build --remap-path-prefix=$HOME/.cargo=/cargo"
cd apps/desktop && pnpm tauri build
```

On Linux, `mold` as the linker cuts link time substantially (add
`-C link-arg=-fuse-ld=mold` to `RUSTFLAGS` for local builds, not for bundles).

## Windows checks

Windows-specific code (paths, registry Java discovery, process creation flags,
WebView2) needs a real Windows machine or a CI runner. Do not assume a Linux
build proves Windows behaviour.

## Source layout

- `crates/tntl-*` — core. No UI dependencies, no global state.
- `apps/desktop` — Tauri shell and Vue frontend.
- `docs/` — architecture, scope, endpoints, licensing.
- `scripts/` — developer and CI helpers.

## Style

- Comments, documentation and commit messages in English.
- Keep comments sparse; explain why, not what.
- Commit subjects describe the root cause or the change, not the process.

## Network discipline

`scripts/check-endpoints.sh` fails on a host that is not in
[`02-endpoints.md`](02-endpoints.md) and on region detection. Run it before
committing:

```sh
bash scripts/check-endpoints.sh
```
