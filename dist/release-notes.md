# PGX release

A self-contained, relocatable, PostgreSQL 18 compatible runtime for local
development, CI and disposable databases. No system PostgreSQL required.

## Install

Requires GitHub CLI 2.49+ (`gh attestation verify`) for signed provenance.

```sh
curl -fsSL https://pgx.dev/install | sh
pgx run                             # a ready `app` database in one step
pgx clone app agent-001 --ttl 5m    # a disposable branch that auto-expires
```

Or download the tarball for your platform below, verify its SHA-256 against
the corresponding `.sha256`, and verify its signed provenance before extracting:

```sh
gh attestation verify pgx-<version>-<os>-<arch>.tar.gz \
  --bundle pgx-<version>-<os>-<arch>.tar.gz.sigstore.json \
  --repo pgrundev/pgx \
  --signer-workflow pgrundev/pgx/.github/workflows/release.yml \
  --deny-self-hosted-runners
```

`bin/pgx` is relocatable.

## Platforms

| Platform | Artifact |
|---|---|
| Linux x86-64 | `pgx-<version>-linux-amd64.tar.gz` |
| Linux arm64  | `pgx-<version>-linux-arm64.tar.gz` |
| macOS arm64  | `pgx-<version>-darwin-arm64.tar.gz` |

XML functions and ICU collations use the host's libxml2 and ICU when present.

## Verified in this release

See `docs/pgx/local-dev/validation-report.md` for the full matrix (unit,
lifecycle, SQL, compatibility vs PostgreSQL 18.6, crash recovery, isolation,
storage, concurrency, stress, and the application fixtures) and the
GO / NO-GO / BLOCKED decision.
