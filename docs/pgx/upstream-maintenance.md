# Upstream maintenance

Status: documents the existing fork model. No automatic upstream merge or deployment is enabled.

PGX imported [PgRust](https://github.com/alexshapalov/pgrust) in commit
`79ad992ede`. The import was squashed: upstream changes cannot be assumed to
apply cleanly or share a merge base. The main and prod branches retain their
[separate release process](local-dev/release-strategy.md).

When reviewing an upstream fix:

1. Record its commit URL and whether PGX still contains the defect. Review
   upstream periodically and before releases, prioritizing security and correctness.
2. Fetch the selected commit into a developer checkout without merging main.
   Verify its source and inspect the diff.
3. Port the smallest applicable change and add a PGX reproducer. Review PGX's
   janitor, memory accounting, and shared caches when their paths overlap.
4. Run affected crate tests, distribution tests, and differential PostgreSQL
   regression checks. Record unsupported differences.
5. Open a PR linking the upstream commit and test evidence. Backport to prod
   only through its reviewed cherry-pick process.

Moving to a merged upstream tracking branch requires a separate maintainer
decision, verified merge base, and conflict/testing ownership. Do not rewrite
the squashed import or published history as part of a routine fix.

## Optional large datasets

The inherited fuzz corpus and historical benchmark results remain available.
CI should use sparse checkout when these datasets are not inputs. Contributors
can likewise start with `git clone --filter=blob:none --sparse` and select the
crates, configs, docs, dist, tests, and workflow directories they need.

Migrating published history or fuzz data to another service requires preserving
references and consumers. Sparse CI checkouts address checkout cost without
removing data or making an irreversible history change.
