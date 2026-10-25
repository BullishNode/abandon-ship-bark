# Portable runtime bundle

Run `build-bundle.py` on the build machine with Python 3.11 or later, Git, Nix
and the qualified GNU programs runnable there. It assembles existing artifacts.
It does not build programs, install software or execute deployment commands.
This candidate pairs the server and watchman with matching protocol 6 Bark and
`barkd` clients. Use fresh stores and signed recipient addresses as described
in `OPERATOR.md`.

Use the complete CLN, grpc and Core store outputs from the qualified build.
The grpc output must contain the exact qualified plugin bytes. A rebuild of the
same derivation can produce different bytes; a matching store name is not a
checksum check. Supply hold's library directories from the pinned
`nix/build-shell.nix` environment: GCC's C++ and support libraries, OpenSSL,
SQLite and PostgreSQL 17's client library. Do not supply host library paths.

```sh
python3 contrib/release/build-bundle.py \
  --source "$QUALIFIED_SOURCE" --artifacts "$GNU_ARTIFACTS" \
  --cln-store "$CLN_STORE" --grpc-store "$GRPC_STORE" \
  --core-store "$CORE_STORE" \
  --hold-lib-dir "$GCC_LIB" --hold-lib-dir "$OPENSSL_LIB" \
  --hold-lib-dir "$SQLITE_LIB" --hold-lib-dir "$POSTGRES_LIB" \
  --output "$NEW_BUNDLE_DIRECTORY"
```

The source must have no tracked or untracked changes. Commit the reviewed
public documentation before the build. Each of the four program versions must
report that full source commit. The artifact directory must contain these
regular files without symlink components:

- `bin/bark`, `bin/barkd`, `bin/captaind` and `bin/watchmand`.
- `plugins/hold` and `plugins/cln-grpc`.
- `SHA256SUMS.uncompressed`, with standard SHA256 entries for each `bin/` file.
- `release-artifacts.json`, with `source_commit` equal to the full Git commit
  and `plugins` as a mapping from `hold` and `cln-grpc` to SHA256 values.
  A list of objects with `name` and `sha256` fields is also accepted.

If both checksum files describe a plugin, the values must agree. Only the
specified programs, plugins, runtime closures, committed public documents and
generated runtime metadata enter the bundle. Other fields in the input JSON
are ignored. The builder creates a new allowlisted `release-artifacts.json`;
it never copies the private build record. Checksums establish byte identity
with the supplied qualification record. They do not establish the truth of that record.

The builder evaluates `clightning.outPath` from the exact committed local Git
flake with `nix eval --offline --no-write-lock-file`. It enables `nix-command`
and `flakes` for that command. Evaluation requires the locked flake inputs in
the local cache. It does not realize a derivation or fetch missing runtime
outputs. Export requires access to the Nix store. All runtime store paths and
their requisites must already exist. The CLN version must be
`v26.06.6-abandon6`, and the bundled Core version must be 31.0.

The output parent directory must exist. The output directory, `.tar.gz` archive
and `.tar.gz.sha256` sidecar must not exist, including dangling symlinks. A
failed export can leave incomplete output; retain it for investigation and
choose a new destination for the next attempt. The builder never overwrites an
existing bundle. With identical inputs, output basename, commit timestamp and
Nix exports, the archive has stable member order, modes, ownership and times.
The builder reads every archived payload again and verifies its checksum
without extraction. It rejects missing, duplicate or unexpected members.

On the release host, verify the archive against a checksum obtained through the
release's trusted channel. Then extract the verified archive and execute
`sha256sum --check SHA256SUMS` from its root. Import every `*-closure.nar` with
`nix-store --import`. Do not build on the release host. Configure the full CLN
runtime and both important plugins. Load `cln-runtime.env` only in the CLN
service. Point CLN's `bitcoin-cli` setting at the bundled Core store output's
`bin/bitcoin-cli`. Keep that environment out of host administration commands.

Nix must support the build machine's architecture on the destination. The
bundle does not replace PostgreSQL installation or provision service accounts,
private configuration, wallets or credentials. Follow `OPERATOR.md` and
`ROLLBACK.md`. Import and test the exact archive on a separate host before
release qualification. Bundle assembly does not publish release assets.

The bundle includes matching public `captaind.default.toml` and
`watchmand.default.toml` templates, and a `cln.conf` fragment requiring
`xpay-slow-mode=true`. Merge the fragment into the CLN configuration and verify
it with `lightning-cli listconfigs xpay-slow-mode`. Copy and edit the templates as described in
`OPERATOR.md`; private deployment configuration must remain outside the release.
It also includes `server-seed-descriptors.py` for the private, non-mainnet
seed-only recovery procedure in `OPERATOR.md`. This helper needs Python 3.

Review the generated documents, config references and environment before
publication. No deployment configuration, home path, internal run label,
private build record, wallet, seed, credential or backup belongs in a release
bundle or its Git commit.
