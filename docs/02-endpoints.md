# Network endpoints

Every host the code may contact is listed below. Hosts shared by several crates
live in `tntl-source`; the rest sit next to the crate that calls them. Adding a
host means adding a row here and a suffix in `scripts/check-endpoints.sh`.

## Policy

- Official sources only. There is no mirror list, no source picker and no
  region detection.
- The launcher must work from a normal internet connection. Proxy settings are
  user-selectable (direct, HTTP or SOCKS5) and apply to every request the
  launcher makes; there is no environment or system-proxy mode.
- Every artifact is verified against the hash published by its source.
- No request is made before the user asks for it.

## Allowed

### Mojang

| Host | Purpose |
|---|---|
| `piston-meta.mojang.com` | version manifest and version JSON |
| `launchermeta.mojang.com` | legacy manifest (only if a legacy version is requested) |
| `piston-data.mojang.com` | client jar and data files |
| `launcher.mojang.com` | legacy client jars |
| `libraries.minecraft.net` | game libraries |
| `resources.download.minecraft.net` | asset objects |
| `api.minecraftservices.com` | profile, entitlements |

### Microsoft / Xbox

| Host | Purpose |
|---|---|
| `login.microsoftonline.com` | OAuth |
| `login.live.com` | OAuth (legacy flow) |
| `user.auth.xboxlive.com` | Xbox user token |
| `xsts.auth.xboxlive.com` | XSTS token |

### Loaders

| Host | Purpose |
|---|---|
| `meta.fabricmc.net`, `maven.fabricmc.net` | Fabric |
| `meta.legacyfabric.net`, `maven.legacyfabric.net` | Legacy Fabric (1.8–1.13.2) |
| `meta.quiltmc.org`, `maven.quiltmc.org` | Quilt |
| `maven.minecraftforge.net`, `files.minecraftforge.net` | Forge |
| `maven.neoforged.net` | NeoForge |
| `repo.spongepowered.org` | Mixin artifacts referenced by Forge |
| `repo1.maven.org` | Maven Central, for tooling the installer profile does not name |

Forge and NeoForge are installed by running the processors of their official
installer jars. Those processors fetch their own tooling from the same maven
hosts and mojmaps from the Mojang hosts listed above.

### Content

| Host | Purpose |
|---|---|
| `api.modrinth.com`, `cdn.modrinth.com` | Modrinth |
| `api.modpacks.ch`, `dist.modpacks.ch`, `edge.forgecdn.net` | FTB modpacks (catalogue and pack files) |
| `api.curseforge.com` | CurseForge catalogue, files and download links |
| `mediafilez.forgecdn.net`, `edge.forgecdn.net` | CurseForge file downloads (the exact host comes from the API) |

### Runtime

| Host | Purpose |
|---|---|
| `api.adoptium.net` | Eclipse Temurin JDK download (optional runtimes) |

### Skins

These three are only contacted when the user types a nickname or opens the skin
library; nothing is sent on startup.

| Host | Purpose |
|---|---|
| `api.mojang.com`, `sessionserver.mojang.com` | nickname to UUID, signed skin texture |
| `skinsystem.ely.by` | community skin mirror, used when Mojang has no texture |
| `api.mineskin.org` | public skin library shown in the skin dialog |

ely.by and MineSkin are community services, not Mojang. Uploading a skin or
switching a cape still goes through the official services API; only looking up
someone else's texture uses the fallbacks.

### Links opened in the browser

These are opened in the system browser by an explicit button; the launcher never
requests them itself.

| Host | Where |
|---|---|
| `learn.microsoft.com` | Settings → Microsoft: how to create an Entra client id |
| `console.curseforge.com` | Settings → CurseForge, and the empty CurseForge tab: create an API key |
| `www.curseforge.com` | project page shown in the content browser |
| `modrinth.com` | project page shown in the content browser |

The skin API documentation lives at `docs.ely.by`; that address only appears in
a code comment, never in a request.

## Anything else

There is no second source for anything, so a host that is not in the tables
above has no business in the tree. `scripts/check-endpoints.sh` fails on such a
host and CI runs it; adding a host means adding a row here and a suffix to the
allowlist in that script. The same applies to region detection: no time-zone,
GeoID or locale-country lookup decides where a file comes from.

## Keys and accounts

CurseForge only answers requests that carry a developer key. The launcher never
ships one: each user creates a key on `curseforge.com` and pastes it in the
settings (or exports `TNTL_CURSEFORGE_API_KEY`). The key is sent in the
`x-api-key` header and is never written to a log. Without a key the CurseForge
tab explains how to create one and stays empty; Modrinth keeps working.

CurseForge blocks downloads for files whose author disabled third-party
distribution. Those files are reported by name instead of being fetched from a
mirror.

Microsoft device-code sign-in uses the same rule: the client id belongs to the
user's own Entra app registration (`TNTL_MSA_CLIENT_ID` or the settings).
