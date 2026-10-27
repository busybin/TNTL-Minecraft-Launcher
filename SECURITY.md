# Security policy

## Reporting a vulnerability

Use GitHub's private advisory form:

<https://github.com/busybin/TNTL-Minecraft-Launcher/security/advisories/new>

Please do not open a public issue for an unfixed vulnerability. Include the
build number from `tntl --version`, the platform, and the steps to reproduce.

## What the launcher stores

- Account tokens: `<root>/accounts.json`, directory `0700`, file `0600`.
- CurseForge key: `<root>/launcher.json`, same permissions.
- Logs: `<root>/logs`, no tokens and no request headers.

Nothing is uploaded anywhere: the launcher has no telemetry and no crash
reporting. See `docs/02-endpoints.md` for the complete list of hosts it talks
to.
