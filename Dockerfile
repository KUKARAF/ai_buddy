# syntax=docker/dockerfile:1

# ---- frontend build -------------------------------------------------------
FROM node:22-bookworm-slim AS frontend
WORKDIR /app/web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

# ---- backend build ---------------------------------------------------------
FROM rust:bookworm AS backend
RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    libssl-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates/ crates/
# docs/ is copied in case the server embeds any docs via include_str! at build
# time (harmless if it doesn't).
COPY docs/ ./docs/
RUN cargo build --release -p server

# ---- geoip database (optional) --------------------------------------------
# Downloads the DB-IP IP-to-Country Lite database (MaxMind-DB format) used for
# the OPTIONAL offline country pre-fill. The data is CC-BY-4.0: the attribution
# "IP Geolocation by DB-IP" (https://db-ip.com) must be preserved wherever this
# data is surfaced.
#
# Best-effort: the download is guarded so a failure (offline build, or the
# current month's file not yet published) is NON-FATAL. On failure no .mmdb is
# produced and the runtime simply disables country pre-fill. DB-IP publishes a
# fresh file monthly at dbip-country-lite-YYYY-MM.mmdb.gz; we try the current
# month, then fall back to the previous month.
FROM debian:bookworm-slim AS geoip
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /geoip
RUN set -eux; \
    base="https://download.db-ip.com/free"; \
    ym="$(date -u +%Y-%m)"; \
    prev="$(date -u -d 'last month' +%Y-%m 2>/dev/null || echo "$ym")"; \
    ( curl -fsSL "$base/dbip-country-lite-$ym.mmdb.gz" -o db.gz \
      || curl -fsSL "$base/dbip-country-lite-$prev.mmdb.gz" -o db.gz ) \
      && gunzip -c db.gz > dbip-country-lite.mmdb \
      && rm -f db.gz \
      || echo "geoip db download skipped (country pre-fill will be disabled)"

# ---- runtime -----------------------------------------------------------
FROM debian:bookworm-slim AS runtime
# sqlite-vec is bundled/loaded at runtime by the `sqlite-vec` crate itself, so
# no extra system package is needed beyond TLS certs + libssl.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin aibuddy

WORKDIR /app
COPY --from=backend /app/target/release/server /app/server
COPY --from=frontend /app/web/build /app/static
# Optional offline IP->country database (see the `geoip` stage). The directory
# always exists; the .mmdb inside it is present only when the download succeeded.
# Attribution: "IP Geolocation by DB-IP" (CC-BY-4.0).
COPY --from=geoip /geoip/ /app/geoip/

# Default data locations inside the container. `AIBUDDY_SQLITE_PATH`'s parent
# dir should be a persistent volume so wallet/sessions/goals survive restarts.
# `AIBUDDY_GEOIP_DB` points at the optional country database; if the file is
# absent (download skipped), the server just disables country pre-fill.
ENV AIBUDDY_SQLITE_PATH=/data/db/ai_buddy.db \
    AIBUDDY_STATIC_DIR=/app/static \
    AIBUDDY_BIND_ADDR=0.0.0.0:8080 \
    AIBUDDY_GEOIP_DB=/app/geoip/dbip-country-lite.mmdb

RUN mkdir -p /data/db && chown -R aibuddy:aibuddy /data /app
USER aibuddy

EXPOSE 8080
ENTRYPOINT ["/app/server"]
