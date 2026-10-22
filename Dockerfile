# PGX — a local, disposable, PostgreSQL-compatible runtime.
#
# Multi-stage: build the engine and CLI from source with the pinned toolchain
# and PostgreSQL 18 client tools (for initdb, to bake the base cluster), then
# ship a slim runtime image with libxml2 and ICU for full SQL/XML and ICU.
#
#   docker build -t pgx:dev .
#   docker run --rm -p 5432:5432 -e PGX_PASSWORD=secret -v pgxdata:/var/lib/pgx pgx:dev
#   psql "postgres://postgres:secret@localhost:5432/postgres"

# ---------- builder ----------
FROM rust:1.96-bookworm@sha256:a339861ae23e9abb272cea45dfafde21760d2ce6577a70f8a926153677902663 AS builder
RUN apt-get update && apt-get install -y --no-install-recommends \
      build-essential pkg-config libre2-dev clang ca-certificates gnupg lsb-release wget \
 && install -d /usr/share/postgresql-common/pgdg \
 && wget -qO /usr/share/postgresql-common/pgdg/apt.postgresql.org.asc https://www.postgresql.org/media/keys/ACCC4CF8.asc \
 && echo "deb [signed-by=/usr/share/postgresql-common/pgdg/apt.postgresql.org.asc] https://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main" > /etc/apt/sources.list.d/pgdg.list \
 && apt-get update && apt-get install -y --no-install-recommends postgresql-18 postgresql-client-18 \
 && rm -rf /var/lib/apt/lists/*
ENV PATH="/usr/lib/postgresql/18/bin:${PATH}" CC=/usr/bin/cc
WORKDIR /src
COPY . .
RUN cargo build --release --locked --bin postgres && cargo build --release --locked --bin pgx
# initdb (run by assemble.sh to bake the base cluster) refuses to run as root,
# so assemble as a non-root user. /out is owned by that user; the built
# binaries and share trees under /src are world-readable.
RUN useradd -m builder && mkdir -p /out && chown builder:builder /out \
 && runuser -u builder -- dist/assemble.sh --out /out --version "${PGX_VERSION:-0.1.0-docker}" --pg-bindir /usr/lib/postgresql/18/bin \
 && mv /out/pgx-*/ /pgx

# ---------- runtime ----------
FROM ubuntu:24.04@sha256:68ee22004a9109b44973313b02ff7b1de250f1de0f5ecb628d73f579aaff33ac
# The PGX engine (like PostgreSQL) refuses to run as root, so the image runs as
# an unprivileged `pgx` user. Create it, own the data/run dirs, and COPY the
# distribution owned by pgx so the bundled base cluster (mode 700) is readable.
RUN apt-get update && apt-get install -y --no-install-recommends libxml2 libicu74 ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -r -u 10001 pgx \
 && mkdir -p /var/lib/pgx /var/run/pgx && chown pgx:pgx /var/lib/pgx /var/run/pgx
COPY --from=builder --chown=pgx:pgx /pgx /opt/pgx
COPY dist/docker/entrypoint.sh /usr/local/bin/pgx-entrypoint
RUN ln -s /opt/pgx/bin/pgx /usr/local/bin/pgx && chmod +x /usr/local/bin/pgx-entrypoint
# PGX_HOME lives on the volume so data and the generated config persist.
ENV PGX_HOME=/var/lib/pgx PGX_RUN_DIR=/var/run/pgx PGX_LISTEN=0.0.0.0 PGX_PORT=5432
USER pgx
VOLUME /var/lib/pgx
EXPOSE 5432
ENTRYPOINT ["pgx-entrypoint"]
