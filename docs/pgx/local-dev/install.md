# Installing PGX

PGX ships as one self-contained, relocatable directory. It needs **no system
PostgreSQL**; the only runtime dependencies are libc and, for XML and ICU
collations, the host's libxml2 and ICU (present by default on macOS and in the
Docker image).

## One-line install

The installer requires curl and GitHub CLI 2.70+ (`gh attestation verify`).

```sh
curl -fsSL https://pgx.dev/install | sh
```

The installer detects your OS and CPU, downloads the matching release tarball
and its `.sha256` and `.sigstore.json` build-provenance bundle. It verifies the
checksum and signed artifact digest, repository, exact workflow/source ref and
hosted runner policy before extracting. It then unpacks into
`~/.pgx/dist/<version>` and links `pgx` into `~/.local/bin`. It never uses root
and refuses an unsupported platform or a tampered download.

Environment overrides: `PGX_VERSION`, `PGX_INSTALL_DIR`, `PGX_BIN_DIR`,
`PGX_BASE_URL` (HTTPS). Ensure `PGX_BIN_DIR` (default `~/.local/bin`) is on your
`PATH`. The installer preserves an unrelated existing `pgx` executable or link;
`PGX_FORCE=1` explicitly permits replacing it.

Older releases without a provenance bundle require the explicit
`PGX_INSECURE_SKIP_VERIFY=1` override. That selects checksum-only installation
and disables the authenticity check; current signed releases use verification
by default. `PGX_INSECURE_ALLOW_HTTP=1` is reserved for local installer tests.

Then get a database in one step:

```sh
pgx run
```

`pgx run` initialises PGX if needed, starts it, ensures a default `app`
database, and prints its connection URL. It is idempotent — run it again any
time; it never resets your data.

## From a release tarball

Download the matching `pgx-<version>-<os>-<arch>.tar.gz`, its `.sha256`, and
its `.sigstore.json` bundle from the same release. With the files in one directory,
set the asset/version below and verify before extracting:

```sh
asset="pgx-<version>-<os>-<arch>.tar.gz"
ref="refs/tags/v<version>" # refs/heads/main for a workflow_dispatch build
shasum -a 256 -c "$asset.sha256"
gh attestation verify "$asset" --bundle "$asset.sigstore.json" \
  --repo pgrundev/pgx \
  --cert-identity "https://github.com/pgrundev/pgx/.github/workflows/release.yml@$ref" \
  --source-ref "$ref" --deny-self-hosted-runners
tar --no-same-owner -xzf "$asset"
"./${asset%.tar.gz}/bin/pgx" version
```

## Docker

```sh
docker run --init -p 5432:5432 -e PGX_PASSWORD=secret -v pgxdata:/var/lib/pgx ghcr.io/pgrundev/pgx
psql "postgres://postgres:secret@localhost:5432/postgres"
```

`-v pgxdata:/var/lib/pgx` persists data; `--init` gives the container an init
that reaps processes.

## From source

See [CONTRIBUTING.md](../../../CONTRIBUTING.md) for the pinned Rust toolchain,
RE2, PostgreSQL 18 tools, assembly, and verification commands.

## Supported platforms

| Platform | Status |
|---|---|
| macOS arm64 (Apple silicon) | supported, verified |
| Linux arm64 | supported, verified |
| Linux x86-64 | supported, built and smoke-tested in CI |
| Windows | not supported |
