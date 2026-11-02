# Signed updates

Update assets are protected by two independent checks:

1. **GitHub SHA-256 digest** — covers corruption and a truncated download.
2. **Detached Ed25519 signature** — covers authenticity when the release build
   embeds a public key.

## Verifying

`TNTL_UPDATE_PUBKEY` (32-byte Ed25519 public key, 64 hex characters) is read at
build time (`option_env!`) and can be overridden at runtime for testing. When it
is set, an update is rejected unless the release publishes a sibling asset named
`<asset-name>.sig` and the signature verifies.

The signed message is the UTF-8 string `sha256:<lowercase hex digest>` of the
asset, not the asset itself. This keeps verification cheap and independent of
the asset size. `ring` does the verification.

A build without the key keeps the old digest-only behaviour. That is useful for
local builds but **not** for a public release.

## Publishing

Generate a key once, offline:

```sh
openssl genpkey -algorithm ed25519 -out update-key.pem
openssl pkey -in update-key.pem -pubout -outform DER | tail -c 32 | xxd -p -c 32
```

Keep `update-key.pem` out of the repository and out of the build machine's
history. The last command prints the value for `TNTL_UPDATE_PUBKEY`.

For every asset:

```sh
digest="sha256:$(sha256sum TNTL-9-win64.exe | cut -d' ' -f1)"
printf '%s' "$digest" > /tmp/update-digest.txt
openssl pkeyutl -sign -inkey update-key.pem -rawin -in /tmp/update-digest.txt \
  | xxd -p -c 256 > TNTL-9-win64.exe.sig
```

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
