# Scope

## Product

A Minecraft launcher for Windows and Linux that uses official upstream sources
only. Users install the game from Mojang, install loaders from their official
metadata/maven, and manage mods, resource packs, shader packs, worlds and
instances.

## In scope

- Vanilla install: version manifest, version JSON, client jar, libraries,
  assets, logging config, Java runtime selection.
- Loaders: Fabric, Quilt, Legacy Fabric, NeoForge, Forge.
- Content: mods, resource packs, shader packs, datapacks, worlds.
- Repositories: Modrinth and CurseForge. CurseForge needs a key from the user's
  own developer account; the launcher never bundles one.
- Accounts: Microsoft (OAuth device code), offline accounts.
- Skins: the official services API, with ely.by as a fallback for texture
  lookups and a MineSkin browse list, both only on request.
- Java: detect installed JVMs, match the version requirement, remember the
  per-instance choice.
- Instances: isolation, per-instance settings, export/import (MultiMC/Prism
  format where practical).
- Diagnostics: structured logs, crash classification, missing-file report.
- Content updates: mods, resource packs, shaders and modpacks can be checked and
  updated. The launcher itself has no self-update yet; new builds come from the
  release page.

## Out of scope

- macOS and any Apple platform.
- Mirrors of game files, loader metadata or maven artifacts; the launcher only
  talks to the upstreams in [`02-endpoints.md`](02-endpoints.md).
- Region detection of any kind.
- Third-party authentication: authlib-injector, Yggdrasil, custom skin services.
- Third-party LAN relay.
- Telemetry and crash uploads.
- OptiFine auto-install (manual install may be supported later).
- Feature parity with any existing launcher.

## Supported platforms

| OS | Architecture | Status |
|---|---|---|
| Windows 10/11 | x86_64 | target |
| Linux (desktop) | x86_64 | target |
| Windows | aarch64 | later |
| Linux | aarch64 | later |

Android is not a target even though it uses the Linux kernel.
