# pgx.dev `/install` endpoint (prepared, not deployed)

`https://pgx.dev/install` must serve the reviewed [`dist/install.sh`](../install.sh)
so that `curl -fsSL https://pgx.dev/install | sh` works. Today `pgx.dev` is a
Cloudflare-hosted placeholder page and `/install` returns **HTTP 404**.

## Current hosting (identified)
- `pgx.dev` resolves through **Cloudflare** (`server: cloudflare`, `cf-ray`, `cf-cache-status`).
- The root serves a **parked/placeholder page** (an `<x-dc>` document + `support.js`), not a managed site. There is **no** `/install` route.
- The serving configuration lives in the **Cloudflare account**, not in this repository.

## What this directory provides
A Cloudflare **Worker** (`install-worker.js` + `wrangler.toml`) that owns only the
`pgx.dev/install` route and returns `dist/install.sh` from a **pinned ref**
(`INSTALL_REF`). The ref is **required** and must be a full 40-character commit SHA —
there is no fallback to a mutable branch. The Worker returns `500` if `INSTALL_REF`
is missing/invalid and `502` if the upstream script can't be fetched (never a
misleading empty `200`). Behavior is covered by `install-worker.test.js`
(`node --test dist/pgx.dev/install-worker.test.js`, run in CI).

## Before deploying — verify the account controls the pgx.dev zone
```sh
cd dist/pgx.dev
npx wrangler whoami                     # confirm the intended Cloudflare account
npx wrangler zone list | grep -w pgx.dev  # the account must list the pgx.dev zone
```
`wrangler deploy` will refuse the `pgx.dev/install` route if the account does not
own the zone — so a successful deploy is itself proof of control.

## Deploy (maintainer, after approval — needs Cloudflare access)
```sh
cd dist/pgx.dev
INSTALL_COMMIT="$(git rev-parse HEAD)" # reviewed installer commit on main
npx wrangler deploy --var "INSTALL_REF:$INSTALL_COMMIT"
curl -fsSL https://pgx.dev/install | head             # verify it serves the script
```
This adds a single route; it does **not** change DNS or other pgx.dev content.

## Prerequisites before the public `curl | sh` works
1. Publish the GitHub release (tag `v0.1.0-beta.1`, **prerelease**) with the platform tarballs + `.sha256` + signed `.sigstore.json` provenance (via `.github/workflows/release.yml`, draft → manual publish).
2. Deploy this Worker with `INSTALL_REF` set to the reviewed full installer commit SHA.
3. `dist/install.sh` then resolves the release artifacts (its discovery reads the releases list, which includes prereleases).

The installer requires GitHub CLI with `gh attestation verify` (2.49 or newer).
The release workflow signs provenance through GitHub OIDC; the installer verifies
the artifact digest, repository, workflow identity and hosted runner policy
before extracting. Existing releases without provenance require the explicit
`PGX_INSECURE_SKIP_VERIFY=1` legacy override. Never enable that override in the
Worker or its public installer response.
