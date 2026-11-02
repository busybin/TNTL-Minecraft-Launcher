# Signed updates

Update assets are protected by two independent checks:

1. **GitHub SHA-256 digest** — covers corruption and a truncated download.
2. **Detached Ed25519 signature** — covers authenticity when the release build
   embeds a public key.

Public releases from V9 on are built with the key embedded, so an update is
rejected unless the release publishes a sibling asset named `<asset-name>.sig`
and the signature verifies. A build without the key keeps the old digest-only
behaviour; it is useful for local builds but **not** for a public release.

The current public key (V9):

```
caa72b7b08966c031144234acaf4c2d1b9e7f0d168ba0d084f4f1d9c9e7e0cc7
```

## Verifying

`TNTL_UPDATE_PUBKEY` (32-byte Ed25519 public key, 64 hex characters) is read at
build time (`option_env!`). Debug builds may override it at runtime for testing;
a release build uses the embedded key only, so a local process cannot swap the
trust anchor of a published binary.

The signed message is the UTF-8 string `sha256:<lowercase hex digest>` of the
asset, not the asset itself. This keeps verification cheap and independent of
the asset size. `ring` does the verification.

A build without the key accepts unsigned updates. Never publish one: the
updater would fall back to the digest GitHub reports, which is not a substitute
for a signature.

## Publishing

Generate a key once, offline, at an absolute path outside the repository:

```sh
scripts/update-keygen.sh "$HOME/tntl-update-key.pem"
```

It refuses to write inside a git worktree, refuses to overwrite an existing
key, and prints the value for `TNTL_UPDATE_PUBKEY`. The underlying commands are:

```sh
openssl genpkey -algorithm ed25519 -out "$HOME/tntl-update-key.pem"
openssl pkey -in "$HOME/tntl-update-key.pem" -pubout -outform DER \
  | tail -c 32 | od -An -v -tx1 | tr -d ' \n'
```

Keep the private key out of the repository and off the build machine's shell
history. Back it up somewhere you control: if it is lost, the next release has
to be installed manually because builds that embed the old key will reject an
update signed with a new one.

For every asset:

```sh
digest="sha256:$(sha256sum TNTL-9-win64.exe | cut -d' ' -f1)"
printf '%s' "$digest" > /tmp/update-digest.txt
openssl pkeyutl -sign -inkey "$HOME/tntl-update-key.pem" -rawin \
  -in /tmp/update-digest.txt | od -An -v -tx1 | tr -d ' \n' > TNTL-9-win64.exe.sig
```

Or run `scripts/sign-update.sh <asset> <key.pem>`, which also writes
`<asset>.sig`.

Upload the asset and its `.sig` to the same GitHub release. The launcher only
accepts a `.sig` whose URL passes the same GitHub host/repository checks as the
asset.

Build the release with the public key embedded:

```sh
TNTL_UPDATE_PUBKEY=<64 hex chars> pnpm tauri build ...
```

Rotating the key means shipping a launcher build with the new key; the old
builds will keep rejecting updates signed by the new key. Document the rotation
in the release notes.
