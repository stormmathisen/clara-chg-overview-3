# Stage 1: Build server + frontend WASM
FROM rust:bookworm AS builder

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
    && rm -rf crates/ \
    && rm -f target/release/server target/release/server.d \
    && rm -f target/release/deps/server-* target/release/deps/shared-* \
    && rm -f target/release/deps/libshared-*

# Copy real source code
COPY crates/ crates/

# Build server
RUN cargo build --release -p server

# Build frontend WASM
COPY crates/frontend/index.html crates/frontend/index.html
COPY crates/frontend/Trunk.toml crates/frontend/Trunk.toml
RUN cd crates/frontend && trunk build --release

# Stage 2: Build EPICS base from source (provides the caput/caget CLI tools).
# Follows the approach in https://github.com/stormmathisen/epics-docker: clone the
# base repo at a release tag and `make`. readline/ncurses are needed by libCom.
# NOTE: the linux-x86_64 arch dir is hardcoded below; an arm64 image would need
# EPICS_HOST_ARCH=linux-aarch64 and the matching bin/lib paths.
FROM debian:bookworm-slim AS epics-builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        libreadline-dev \
        libncurses-dev \
        perl \
        git \
        ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Override with --build-arg EPICS_VERSION=R7.0.10 etc.
ARG EPICS_VERSION=R7.0.8.1
RUN git clone --depth 1 --branch "${EPICS_VERSION}" \
        https://github.com/epics-base/epics-base.git /epics-base

WORKDIR /epics-base
RUN make -j"$(nproc)"

# Fail here rather than shipping an image whose caput does not run.
RUN ./bin/linux-x86_64/caput -h >/dev/null && ./bin/linux-x86_64/caget -h >/dev/null

# Stage 3: Minimal runtime image
FROM debian:bookworm-slim

# libreadline/libncurses are runtime deps of EPICS libCom.
RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        libreadline8 \
        libncurses6 \
    && rm -rf /var/lib/apt/lists/*

# EPICS base CLI tools, built from source in the epics-builder stage. The server
# shells out to `caput` to write scalar PVs (corrA/corrB, DQcal, sweep windows,
# restore-defaults), so they must be on PATH at runtime.
#
# Keep the same /epics-base prefix the builder used: the binaries carry an rpath of
# /epics-base/lib/linux-x86_64, so relocating them would leave library resolution
# depending on LD_LIBRARY_PATH alone.
ENV EPICS_BASE=/epics-base
ENV EPICS_HOST_ARCH=linux-x86_64
ENV PATH="/epics-base/bin/linux-x86_64:${PATH}"

COPY --from=epics-builder /epics-base/bin/linux-x86_64/ /epics-base/bin/linux-x86_64/
COPY --from=epics-builder /epics-base/lib/linux-x86_64/ /epics-base/lib/linux-x86_64/

# Prove caput/caget resolve, load their shared libs, and run — so a broken EPICS
# copy fails the image build instead of silently degrading PV writes at runtime.
RUN ldd "$(command -v caput)" | grep "not found" && exit 1 || true
RUN caput -h >/dev/null && caget -h >/dev/null && echo "EPICS caput/caget OK"

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
