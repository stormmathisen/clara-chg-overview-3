# Stage 1: Build server + frontend WASM
FROM rust:1.85-bookworm AS builder

# Install trunk and wasm target
RUN rustup target add wasm32-unknown-unknown \
    && cargo install trunk --locked

WORKDIR /build

# Copy manifests first for layer caching
COPY Cargo.toml Cargo.lock* ./
COPY crates/shared/Cargo.toml crates/shared/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
COPY crates/frontend/Cargo.toml crates/frontend/Cargo.toml

# Create stub sources so cargo can resolve and cache dependencies
RUN mkdir -p crates/shared/src crates/server/src crates/frontend/src \
    && echo "pub mod messages; pub mod config;" > crates/shared/src/lib.rs \
    && echo "fn main() {}" > crates/server/src/main.rs \
    && echo "" > crates/frontend/src/lib.rs \
    && touch crates/shared/src/messages.rs crates/shared/src/config.rs \
    && cargo build --release -p server 2>/dev/null || true \
    && rm -rf crates/

# Copy real source code
COPY crates/ crates/

# Build server
RUN cargo build --release -p server

# Build frontend WASM
COPY crates/frontend/index.html crates/frontend/index.html
COPY crates/frontend/Trunk.toml crates/frontend/Trunk.toml
RUN cd crates/frontend && trunk build --release

# Stage 2: Minimal runtime image
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        epics-base \
    && rm -rf /var/lib/apt/lists/*

# epics-base provides caput/caget; if not available via apt, it can be omitted
# and caput calls will fail gracefully with logged errors.
RUN which caput 2>/dev/null || true

# Run as non-root user
RUN useradd -r -s /usr/sbin/nologin appuser

WORKDIR /app

COPY --from=builder /build/target/release/server /app/server
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
