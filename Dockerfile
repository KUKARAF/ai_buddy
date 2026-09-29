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

# Default data locations inside the container. `AIBUDDY_SQLITE_PATH`'s parent
# dir should be a persistent volume so wallet/sessions/goals survive restarts.
ENV AIBUDDY_SQLITE_PATH=/data/db/ai_buddy.db \
    AIBUDDY_STATIC_DIR=/app/static \
    AIBUDDY_BIND_ADDR=0.0.0.0:8080

RUN mkdir -p /data/db && chown -R aibuddy:aibuddy /data /app
USER aibuddy

EXPOSE 8080
ENTRYPOINT ["/app/server"]
