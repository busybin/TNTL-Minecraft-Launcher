# Architecture (ADR-0001)

## Goals

1. The launcher does not need a JVM. Java is only a runtime that gets launched.
2. The core is headless, deterministic where it matters, and testable without a
   desktop.
3. A launch is described by data (`LaunchPlan`) before anything is executed.
4. Network access is explicit, typed and auditable.
5. No global mutable state; no UI toolkit inside the core.

## Crate graph

```
tntl-model      pure data: version JSON, rules, arguments, asset index
tntl-resolve    pure: version tree -> LaunchPlan + missing objects
tntl-store      content-addressed objects, external read-only roots, verify/repair
tntl-source     network sources: Mojang, Fabric, Quilt
tntl-forge      Forge and NeoForge installer jars: processors, patched client
tntl-modrinth   Modrinth API: search, versions, dependency-aware installs and updates
tntl-curseforge CurseForge API: search, files, dependency-aware installs and updates
                (needs the user's own key; also keeps the source sidecar)
tntl-modpack    Modrinth modpacks (`.mrpack`): index, file downloads, overrides
tntl-ftb        FTB modpacks: catalogue, versions, parallel file downloads
tntl-runtime    Mojang Java runtimes: list and install them into `java/`
tntl-java       JVM discovery and version matching
tntl-auth       Microsoft/Xbox auth, token refresh, account store
tntl-instance   instance model, mods, worlds, logs
tntl-launch     process spawn, log pump, crash classification
tntl-job        cancellable jobs and progress events
tntl-app        service layer: instances, install, launch, Java; used by CLI and shell

tntl-cli        depends on tntl-app and the core crates
apps/desktop   Tauri commands -> core; depends on crates, never the reverse
```

Dependency direction is one-way. `tntl-model` has no project dependencies;
nothing under `crates/` may depend on Tauri, Vue or any GUI crate.

## Invariants

1. **No UI in the core.** `tntl-cli` can install, verify and launch a game with
   no window system.
2. **`resolve` is pure.** It takes the version tree, the environment and the
   instance overrides, and returns a `LaunchPlan`. It performs no I/O.
3. **Missing files are data, not an excuse to fetch.** `resolve` reports missing
   objects; fetching is a separate job the caller starts explicitly.
4. **No hidden network.** Hosts shared by several crates live in
   `tntl_source::endpoints`, the rest next to their crate, and all of them are
   listed in `docs/02-endpoints.md`. CI runs `scripts/check-endpoints.sh`, which
   fails on a host that is not on that list.
5. **No region detection and no mirror picking.** There is exactly one source
   per artifact, and it is the official one.
6. **Jobs are cancellable and observable.** Every long operation is a `Job`
   with an id, a stage, progress and a cancellation token. The UI subscribes to
   events; it never polls internal state.
7. **Settings are normalised on load.** One schema, unknown or out-of-range
   values clamped to safe defaults, written through a temp file and a rename. A
   file that does not parse is kept aside instead of being overwritten. Anything
   secret (the CurseForge key, proxy credentials, account tokens) is written with
   owner-only permissions (`0600`) and never logged.
8. **Instance writes are atomic.** Metadata goes through a temp file and a
   rename, so a crash leaves the previous `instance.toml` intact rather than a
   half-written one. Multi-step directory moves are not journalled.
9. **Source and docs are English.** User-visible strings live in
   `apps/desktop/src/locales` (eight languages, English as the fallback).

## LaunchPlan

```rust
pub struct LaunchPlan {
    pub java: PathBuf,
    pub jvm_args: Vec<String>,
    pub main_class: String,
    pub classpath: Vec<PathBuf>,
    pub natives: Vec<PathBuf>,
    pub game_args: Vec<String>,
    pub working_dir: PathBuf,
    pub env: BTreeMap<String, String>,
    pub warnings: Vec<Diagnostic>,
    pub missing: Vec<MissingObject>,
}
```

A plan can be printed, diffed, stored in a bug report and replayed. This is the
main debugging and testing surface.

## Storage

- `objects/<sha1[0..2]>/<sha1[2..]>` — content-addressed, immutable.
- External roots (an existing `.minecraft`, another launcher's libraries and
  assets) are mounted read-only and verified by hash; nothing is copied unless
  the user asks for isolation.
- Instances reference objects; a mark-and-sweep GC removes unreferenced
  objects.
- Downloads are hash-checked and written to a temp file first; an interrupted
  file is fetched again from the start (no byte-range resume).

## Network

- One module owns hosts, paths and user agents.
- Per-host concurrency limits, exponential backoff with jitter, `Retry-After`
  support.
- Nothing is fetched at startup. The version manifest is fetched when the
  install page opens; every other source (Modrinth, CurseForge, FTB, Adoptium,
  a skin lookup or the skin library) only when the user uses it.
- The proxy is launcher-wide: a direct connection, HTTP or SOCKS5, chosen in
  the settings page and applied to every client. There is no environment or
  system-proxy mode. HTTP and SOCKS5 proxies may take a user name and password;
  the password is stored in `launcher.json` next to the proxy address and never
  put into the URL.

## Security

- Zip extraction rejects absolute paths, `..`, symlinks and entries that escape
  the destination.
- Every downloaded artifact is verified against its published hash.
- Executing a downloaded jar is only allowed for loader installers, after an
  explicit user action.
- Logs and printed plans redact access tokens and session strings.

## Testing

- A synthetic version JSON (`crates/*/tests/fixtures/1.20.1-short.json`) for the
  parser and rule evaluation; it is hand-written, not a copy of a real manifest.
- Unit tests for rule evaluation (os, arch, features) next to the code.
- A loopback HTTP server (`TcpListener` on port 0) for download and retry tests;
  no test hits the internet by default.
- A smoke test that builds a plan for a hand-written 1.20.1 tree and asserts on
  the arguments, classpath and missing objects inline.

## UI

- Tauri 2 commands are thin wrappers around core jobs.
- The frontend keeps only view state; persisted state belongs to the core.
- i18n: plain TypeScript tables in `apps/desktop/src/locales`, English source,
  eight languages with an English fallback.
