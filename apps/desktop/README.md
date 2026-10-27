# Desktop shell

Tauri 2 + Vue 3 + TypeScript shell over the `tntl-app` service layer. Windows and
Linux only; there is no macOS bundle target.

## Layout

- `src/` — Vue UI. No game logic: everything goes through the commands in
  `src-tauri/src/main.rs`.
- `src-tauri/` — Rust shell. Commands validate input, call `tntl-app` on a worker
  thread and stream progress back as Tauri events (`install-progress`,
  `game-line`, `game-exit`, …).
- The shell is a **separate Cargo workspace** so `cargo build --workspace` and CI
  never need the webkit toolchain. It reaches the core crates by path.

## Development

Requires `webkit2gtk-4.1`, `libsoup3`, `gtk3`, Node and Rust.

```sh
cd apps/desktop
pnpm install
pnpm tauri dev          # Vite on :1420 + the desktop window
```

The data directory defaults to the platform data dir (`~/.local/share/tntl`
on Linux). Pass `TNTL_ROOT` or a directory argument to use another one.

## Build

```sh
pnpm tauri build        # deb + appimage on Linux
```

## Checks

```sh
pnpm build              # vue-tsc + vite build
cd src-tauri && cargo check
```
