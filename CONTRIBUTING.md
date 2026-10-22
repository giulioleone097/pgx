# Contributing to PGX

Bug reports and pull requests are welcome at <https://github.com/pgrundev/pgx>.
Include the version, platform, a minimal reproduction, expected behavior, and
actual output. Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## Build and test

Use the toolchain in `rust-toolchain.toml`, a C/C++ compiler, pkg-config,
RE2 development files, Python 3, and PostgreSQL 18 tools (`initdb`, `pg_config`,
`psql`). RE2 is required for release builds. On Debian/Ubuntu with PostgreSQL's
Apt repository configured:

```sh
apt-get install build-essential pkg-config libre2-dev postgresql-18 postgresql-client-18
export PATH="/usr/lib/postgresql/18/bin:$PATH"
cargo fmt -p pgx -- --check
cargo clippy -p pgx --all-targets -- -D warnings
cargo build --release --locked --bin postgres --bin pgx
cargo test --release -p pgx --bin pgx
dist/assemble.sh --out /tmp/pgx-dist --version 0.0.0-dev --pg-bindir /usr/lib/postgresql/18/bin
export PGX_DIST="/tmp/pgx-dist/pgx-0.0.0-dev-linux-amd64"
python3 -I tests/pgx/test_sql.py
cargo test --release -p pgx --tests
```

On macOS install `postgresql@18`, `re2`, and `pkg-config` with Homebrew; use
`$(brew --prefix postgresql@18)/bin` for the bin directory and PATH, and the
assembled `darwin-arm64` prefix for PGX_DIST. Installed PGX users do not need
system PostgreSQL tools.

Tests use disposable clusters. Never point PGX_HOME at valuable data while
running tests. PGX_TEST_PSQL selects a psql executable; PGX_TEST_KEEP=1 retains
clusters for inspection. Integration tests require PGX_DIST: a skipped or
ignored test is not evidence of a pass.

For engine changes, also run affected crate tests and feature checks:

```sh
cargo test --release --lib -p mcx -p l2cache -p relcache -p janitor -p memwatchdog -p guc_tables -p autovacuum -p pgstat -p adt_xml
python3 -I tests/pgx/test_features.py -v
```

Keep unrelated changes out of a fix. Include a regression test and record the
commands actually run in the PR. Engine fixes should be separate commits with
a reproducer. Native Linux amd64, Linux arm64, and macOS arm64 CI validates the
shipped platforms. See the [release strategy](docs/pgx/local-dev/release-strategy.md)
and [upstream maintenance](docs/pgx/upstream-maintenance.md).
