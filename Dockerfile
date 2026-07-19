# Stage 1: Build server + frontend WASM
# Pinned rather than floating on `rust:bookworm` so image builds are reproducible.
FROM rust:1.94-bookworm AS builder

# Prebuilt trunk (compiling it from source costs minutes) + wasm target.
# Keep the version in step with what's used locally.
ARG TRUNK_VERSION=v0.21.14
RUN rustup target add wasm32-unknown-unknown \
    && curl -fsSL "https://github.com/trunk-rs/trunk/releases/download/${TRUNK_VERSION}/trunk-x86_64-unknown-linux-gnu.tar.gz" \
       | tar -xz -C /usr/local/bin trunk

WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/

# Cache mounts keep the cargo registry and the incremental target dir on the
# build host across builds, so a code-only change rebuilds just the changed
# crates. Artifacts must be copied out — cache mounts aren't part of the layer.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release -p server \
    && cp target/release/server /server

# Frontend WASM (frontend_dist/ lands outside target/, so it persists in the layer)
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cd crates/frontend && trunk build --release

# Stage 2: Minimal runtime image
#
# No EPICS base here: the server speaks Channel Access natively via the `epicars`
# crate for both reads and writes, so it needs neither the caput/caget binaries nor
# their readline/ncurses runtime deps.
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

# Run as non-root user
RUN useradd -r -s /usr/sbin/nologin appuser

WORKDIR /app

COPY --from=builder /server /app/server
COPY --from=builder /build/frontend_dist/ /app/frontend_dist/
COPY config/ /app/config/

RUN chown -R appuser:appuser /app
USER appuser

ENV PORT=49195
ENV FRONTEND_DIR=/app/frontend_dist
ENV CHARGE_CONFIG=/app/config/charge_devices.yaml
ENV NETWORK_CONFIG=/app/config/network.yaml

EXPOSE 49195

HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
    CMD curl -f http://localhost:49195/ || exit 1

ENTRYPOINT ["/app/server"]
