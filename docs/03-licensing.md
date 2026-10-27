# Licensing

## Decision

The project is free software under the **GNU General Public License, version 3
or later** (`GPL-3.0-or-later`). The full text is in [`LICENSE`](../LICENSE).

In short:

- You may use, study, modify and redistribute the launcher, including
  commercially.
- If you distribute a modified version, you must publish its source under the
  same license and keep the copyright notices.
- There is no warranty.

## Why GPL-3.0

- It is a well-understood, OSI-approved license that matches the ecosystem this
  project lives in (HMCL and most Minecraft tooling are GPL-3.0).
- It keeps the launcher and every fork open. A closed derivative, including a
  paid rebrand, is not allowed.
- It removes the need for a custom license or a CLA.

## Platform support is not a license term

Only Windows and Linux are supported. That is a statement about builds, tests
and the code we maintain, not a restriction in the license:

- CI builds and tests `windows-latest` and `ubuntu-latest` only.
- There is no macOS bundle configuration and no macOS Java discovery.
- The version-rule evaluator still names macOS in a few `cfg` branches because
  Mojang's own rules are written in those terms; that is data handling, not a
  supported target.
- GPL-3.0 does not allow adding a "no macOS" clause, so anyone may port the code.
  They must use a different name and icon (see [`TRADEMARK.md`](../TRADEMARK.md)).

## Contributions

Contributions are accepted under the same license (inbound = outbound). No CLA
is required; contributors keep their copyright. A sign-off (`git commit -s`) is
welcome but not enforced yet.

## Dependencies

Compatible: MIT, Apache-2.0, BSD-2/3, ISC, Zlib, MPL-2.0, LGPL-2.1/3.0 (dynamic
linking only, and the LGPL text must be shipped), GPL-3.0.

Incompatible: GPL-2.0-only, proprietary licenses, SSPL, BUSL, Commons Clause,
and anything that adds restrictions GPL-3.0 forbids.

Every new dependency needs a license check in review. `cargo deny` will enforce
this once the dependency set stabilizes.

## Asset licensing

The launcher code is GPL-3.0. Minecraft itself is not: game files are downloaded
from Mojang at runtime under the Minecraft EULA and are never bundled. The name
"Minecraft" is a trademark of Mojang Studios; this project is not affiliated
with or endorsed by Mojang or Microsoft.
